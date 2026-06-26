//! Real-time **health dashboard** (`oura live`).
//!
//! "Live mode" holds one BLE connection and pushes every signal the ring will
//! give in real time to a self-contained web page (no CDN/external scripts):
//!
//! - **Motion** — the accelerometer is the only true high-rate BLE stream
//!   (~50 Hz), armed with `SetRealtime(ACM)`. Rock solid; the load-bearing signal
//!   for movement/restlessness.
//! - **Heart rate** — this firmware never pushes an IBI stream, so we hold daytime
//!   HR in `CONNECTED_LIVE` (forcing the optical sensor on) and *poll*
//!   `GetFeatureLatestValues`. A value appears only when the ring locks a beat
//!   (intermittent by design); the dashboard shows ring state meanwhile.
//! - **SpO2** — polled the same way (needs long stillness to produce a value).
//! - **Battery / charging** — polled on a slow timer; reliable even on the charger.
//!
//! Pressing **Stop** (or closing the tab) returns the ring to normal: realtime
//! off, features back to `AUTOMATIC` (the ring's own periodic background mode).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use oura_link::ble::BleTransport;
use oura_link::client::AcmSample;
use oura_link::transport::Transport;
use oura_link::OuraClient;
use oura_protocol::protocol::{self, feature, feature_mode, req_set_feature_mode, req_set_notification};

type Client = Arc<OuraClient<BleTransport>>;

/// Serve the dashboard at `127.0.0.1:port`. `minutes` is how long each realtime
/// arming lasts before the poll loop re-arms it (the ring auto-stops otherwise).
pub async fn run(client: OuraClient<BleTransport>, port: u16, minutes: u16) -> Result<()> {
    let client: Client = Arc::new(client);
    let (tx, _) = broadcast::channel::<String>(1024);
    let live = Arc::new(AtomicBool::new(false));
    let clients = Arc::new(AtomicUsize::new(0));

    spawn_parser(&client, &tx);
    spawn_poll_loop(client.clone(), tx.clone(), live.clone(), minutes);

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Ready — open http://127.0.0.1:{port}  (press Start in the page)");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                live.store(false, Ordering::SeqCst);
                restore_normal(&client).await;
                println!("\nStopped live mode, exiting.");
                break;
            }
            accept = listener.accept() => {
                if let Ok((sock, _)) = accept {
                    let rx = tx.subscribe();
                    let c = client.clone();
                    let lv = live.clone();
                    let cl = clients.clone();
                    tokio::spawn(async move {
                        let _ = handle(sock, rx, c, lv, cl, port).await;
                    });
                }
            }
        }
    }
    Ok(())
}

/// Background task: raw ring notifications -> typed JSON messages for the page.
fn spawn_parser(client: &Client, tx: &broadcast::Sender<String>) {
    let mut rx = client.transport().subscribe();
    let tx = tx.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    // Accelerometer indications (the high-rate live stream).
                    for s in AcmSample::parse_frame(&frame) {
                        let _ = tx.send(format!(
                            "{{\"t\":\"accel\",\"x\":{},\"y\":{},\"z\":{}}}",
                            s.x, s.y, s.z
                        ));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// The single writer task. All protocol writes happen here so they never race;
/// the HTTP handlers only flip the `live` flag.
fn spawn_poll_loop(
    client: Client,
    tx: broadcast::Sender<String>,
    live: Arc<AtomicBool>,
    minutes: u16,
) {
    tokio::spawn(async move {
        let mut was_live = false;
        let mut armed_at: Option<Instant> = None;
        let mut hr_cursor: u32 = 0;
        let mut tick: u64 = 0;
        let rearm_after = Duration::from_secs((minutes.max(1) as u64) * 60 - 20);

        loop {
            let now_live = live.load(Ordering::SeqCst);

            // Rising edge: enter live mode. Enable async notifications + force the
            // green-LED HR measurement, then position the event cursor at "now" so
            // we only stream HR events recorded from here on.
            if now_live && !was_live {
                let _ = tx.send("{\"t\":\"status\",\"live\":true}".into());
                let _ = client.transport().write(&req_set_notification(0x3f)).await;
                let _ = client
                    .transport()
                    .write(&req_set_feature_mode(feature::DAYTIME_HR, feature_mode::CONNECTED_LIVE))
                    .await;
                let _ = client
                    .transport()
                    .write(&req_set_feature_mode(feature::SPO2, feature_mode::AUTOMATIC))
                    .await;
                // Baseline drain (no UI output) to find the newest event timestamp.
                if let Ok(out) = client
                    .drain_events_live(0, Duration::from_millis(2500), |_| {})
                    .await
                {
                    hr_cursor = out.next_cursor;
                }
                arm_acm(&client, minutes).await;
                armed_at = Some(Instant::now());
            }
            // Falling edge: back to normal.
            if !now_live && was_live {
                restore_normal(&client).await;
                armed_at = None;
                let _ = tx.send("{\"t\":\"status\",\"live\":false}".into());
            }
            was_live = now_live;

            if !now_live {
                tokio::time::sleep(Duration::from_millis(400)).await;
                continue;
            }

            // Re-arm the accelerometer before its timer lapses.
            if armed_at.map(|t| t.elapsed() > rearm_after).unwrap_or(true) {
                arm_acm(&client, minutes).await;
                armed_at = Some(Instant::now());
            }

            // Drain freshly-recorded events (stream-safe) and forward HR / SpO2.
            // 0x80 green_ibi_quality_event carries {hr_bpm, ibi_ms, quality}.
            if let Ok(out) = client
                .drain_events_live(hr_cursor, Duration::from_millis(1500), |ev| match ev.tag {
                    0x80 => {
                        if let Some(d) = &ev.decoded {
                            let _ = tx.send(format!("{{\"t\":\"hr80\",\"d\":{d}}}"));
                        }
                    }
                    0x6f | 0x70 | 0x77 => {
                        if let Some(d) = &ev.decoded {
                            let _ = tx.send(format!("{{\"t\":\"spo2e\",\"d\":{d}}}"));
                        }
                    }
                    0x46 | 0x69 | 0x75 => {
                        if let Some(d) = &ev.decoded {
                            let _ = tx.send(format!("{{\"t\":\"temp\",\"d\":{d}}}"));
                        }
                    }
                    _ => {}
                })
                .await
            {
                hr_cursor = out.next_cursor;
            }

            // Battery roughly every ~15s.
            if tick % 6 == 0 {
                if let Ok(Some(b)) = client.battery_live(Duration::from_millis(700)).await {
                    let charging = if b.charging_progress > 0 { "true" } else { "false" };
                    let _ = tx.send(format!(
                        "{{\"t\":\"batt\",\"pct\":{},\"charging\":{charging}}}",
                        b.percent
                    ));
                }
            }

            tick += 1;
            tokio::time::sleep(Duration::from_millis(2000)).await;
        }
    });
}

async fn arm_acm(client: &Client, minutes: u16) {
    let _ = client
        .transport()
        .write(&protocol::req_set_realtime(protocol::realtime::ACM, minutes, 0))
        .await;
}

/// Return the ring to its normal background state: realtime off, features auto.
async fn restore_normal(client: &Client) {
    let _ = client.transport().write(&protocol::req_realtime_off()).await;
    let _ = client
        .transport()
        .write(&req_set_feature_mode(feature::DAYTIME_HR, feature_mode::AUTOMATIC))
        .await;
}

fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn handle(
    mut sock: TcpStream,
    mut rx: broadcast::Receiver<String>,
    _client: Client,
    live: Arc<AtomicBool>,
    clients: Arc<AtomicUsize>,
    port: u16,
) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");

    // Same loopback + CSRF defences as the motion server.
    let host_ok = header(&req, "host")
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return forbidden(&mut sock).await;
    }
    if matches!(path, "/start" | "/stop") {
        if header(&req, "x-oura-viz").is_none() {
            return forbidden(&mut sock).await;
        }
        let origin_ok = header(&req, "origin").is_none_or(|o| {
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        });
        if !origin_ok {
            return forbidden(&mut sock).await;
        }
    }

    match path {
        "/stream" => {
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            )
            .await?;
            clients.fetch_add(1, Ordering::SeqCst);
            loop {
                match rx.recv().await {
                    Ok(line) => {
                        if sock.write_all(format!("data: {line}\n\n").as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            // Last viewer gone: leave live mode so we stop draining the battery.
            if clients.fetch_sub(1, Ordering::SeqCst) == 1 {
                live.store(false, Ordering::SeqCst);
            }
        }
        "/start" => {
            live.store(true, Ordering::SeqCst);
            ok(&mut sock, "started").await?;
        }
        "/stop" => {
            live.store(false, Ordering::SeqCst);
            ok(&mut sock, "stopped").await?;
        }
        _ => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Cache-Control: no-store\r\nContent-Length: {}\r\n\r\n{}",
                INDEX_HTML.len(),
                INDEX_HTML
            );
            sock.write_all(resp.as_bytes()).await?;
        }
    }
    Ok(())
}

async fn ok(sock: &mut TcpStream, msg: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        msg.len(),
        msg
    );
    sock.write_all(resp.as_bytes()).await?;
    Ok(())
}

async fn forbidden(sock: &mut TcpStream) -> Result<()> {
    sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await?;
    Ok(())
}

const INDEX_HTML: &str = include_str!("live.html");
