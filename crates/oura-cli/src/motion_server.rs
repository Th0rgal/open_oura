//! Shared local web server for the live-motion pages (`viz`, `game`).
//!
//! Streams the ring's accelerometer over Server-Sent Events to a self-contained
//! HTML page (no external scripts/CDN), and exposes `/start` and `/stop` to arm
//! the BLE stream. Each caller supplies its own page via `index_html`; everything
//! else — parsing, fan-out, and the loopback/CSRF defences — is shared.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use oura_link::ble::BleTransport;
use oura_link::client::AcmSample;
use oura_link::transport::Transport;
use oura_link::OuraClient;
use oura_protocol::protocol;

type Client = Arc<OuraClient<BleTransport>>;

/// Serve `index_html` at `127.0.0.1:port`. Streaming is toggled from the page;
/// each "start" arms the ring for `minutes` (so it auto-stops if the page closes).
pub async fn run(
    client: OuraClient<BleTransport>,
    port: u16,
    minutes: u16,
    index_html: &'static str,
) -> Result<()> {
    let client: Client = Arc::new(client);
    let (tx, _) = broadcast::channel::<String>(512);
    // Count of live SSE clients: when the last one drops (tab closed), stop the
    // ring so we don't keep streaming (and draining battery) until its timer.
    let clients = Arc::new(AtomicUsize::new(0));

    // Always-on parser: raw ring notifications -> ACM samples -> JSON to the page.
    let mut raw_rx = client.transport().subscribe();
    let tx_parse = tx.clone();
    tokio::spawn(async move {
        loop {
            match raw_rx.recv().await {
                Ok(frame) => {
                    for s in AcmSample::parse_frame(&frame) {
                        let _ =
                            tx_parse.send(format!("{{\"x\":{},\"y\":{},\"z\":{}}}", s.x, s.y, s.z));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Ready — open http://127.0.0.1:{port}  (use Start in the page)");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client.transport().write(&protocol::req_realtime_off()).await;
                println!("\nStopped streaming, exiting.");
                break;
            }
            accept = listener.accept() => {
                if let Ok((sock, _)) = accept {
                    let rx = tx.subscribe();
                    let c = client.clone();
                    let cl = clients.clone();
                    tokio::spawn(async move { let _ = handle(sock, rx, c, cl, port, minutes, index_html).await; });
                }
            }
        }
    }
    Ok(())
}

/// Case-insensitive lookup of an HTTP header value in the raw request.
fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn handle(
    mut sock: TcpStream,
    mut rx: broadcast::Receiver<String>,
    client: Client,
    clients: Arc<AtomicUsize>,
    port: u16,
    minutes: u16,
    index_html: &'static str,
) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");

    // Defend the local server against DNS-rebinding and cross-site (CSRF) calls:
    // require a loopback Host on every request, and a same-origin Origin on the
    // control endpoints (browsers attach Origin to cross-site fetches).
    let host_ok = header(&req, "host")
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return forbidden(&mut sock).await;
    }
    if matches!(path, "/start" | "/stop") {
        // Require a custom header. Same-origin fetch (our page) can set it; an
        // <img>/<form>/navigation cannot add headers, and a cross-origin fetch
        // that tries is blocked by the CORS preflight we never approve. This
        // closes the no-Origin GET CSRF vector that an Origin check alone misses.
        if header(&req, "x-oura-viz").is_none() {
            return forbidden(&mut sock).await;
        }
        // Defence in depth: also reject a mismatched Origin when present.
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
                        if sock
                            .write_all(format!("data: {line}\n\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            // This client is gone; if it was the last one, stop the ring stream.
            if clients.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = client
                    .transport()
                    .write(&protocol::req_realtime_off())
                    .await;
            }
        }
        "/start" => {
            let _ = client
                .transport()
                .write(&protocol::req_set_realtime(
                    protocol::realtime::ACM,
                    minutes,
                    0,
                ))
                .await;
            ok(&mut sock, "started").await?;
        }
        "/stop" => {
            let _ = client
                .transport()
                .write(&protocol::req_realtime_off())
                .await;
            ok(&mut sock, "stopped").await?;
        }
        _ => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Cache-Control: no-store\r\nContent-Length: {}\r\n\r\n{}",
                index_html.len(),
                index_html
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
    sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        .await?;
    Ok(())
}
