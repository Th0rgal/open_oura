//! Real-time 3D visualizer for the ring's motion.
//!
//! Serves a self-contained web page (no external scripts — a hand-rolled canvas
//! renderer, so there is no CDN/Subresource-Integrity exposure) that shows the
//! ring's orientation (from the gravity vector) and a best-effort motion
//! trajectory. The page can start/stop the ring's BLE stream and tune the motion
//! sensitivity live.
//!
//! Sensor note: the gyroscope is not on the live BLE channel (only the
//! accelerometer is; gyro is RData-only and not real-time), so orientation is
//! accel-derived (pitch/roll observable, yaw not) and the integrated trajectory
//! drifts — a zero-velocity update plus the sensitivity controls keep it usable.

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use oura_core::ble::BleTransport;
use oura_core::client::AcmSample;
use oura_core::protocol;
use oura_core::transport::Transport;
use oura_core::OuraClient;

type Client = Arc<OuraClient<BleTransport>>;

/// Serve the visualizer at `127.0.0.1:port`. Streaming is toggled from the page;
/// each "start" arms the ring for `minutes` (so it auto-stops if the page closes).
pub async fn run(client: OuraClient<BleTransport>, port: u16, minutes: u16) -> Result<()> {
    let client: Client = Arc::new(client);
    let (tx, _) = broadcast::channel::<String>(512);

    // Always-on parser: raw ring notifications -> ACM samples -> JSON to the page.
    let mut raw_rx = client.transport().subscribe();
    let tx_parse = tx.clone();
    tokio::spawn(async move {
        loop {
            match raw_rx.recv().await {
                Ok(frame) => {
                    for s in AcmSample::parse_frame(&frame) {
                        let _ = tx_parse.send(format!("{{\"x\":{},\"y\":{},\"z\":{}}}", s.x, s.y, s.z));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Visualizer ready — open http://127.0.0.1:{port}  (use Start in the page)");

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
                    tokio::spawn(async move { let _ = handle(sock, rx, c, port, minutes).await; });
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
    port: u16,
    minutes: u16,
) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");

    // Defend the local server against DNS-rebinding and cross-site (CSRF) calls:
    // require a loopback Host on every request, and a same-origin Origin on the
    // control endpoints (browsers attach Origin to cross-site fetches).
    let host_ok = header(&req, "host").is_some_and(|h| {
        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    });
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
        }
        "/start" => {
            let _ = client
                .transport()
                .write(&protocol::req_set_realtime(protocol::realtime::ACM, minutes, 0))
                .await;
            ok(&mut sock, "started").await?;
        }
        "/stop" => {
            let _ = client.transport().write(&protocol::req_realtime_off()).await;
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
    sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        .await?;
    Ok(())
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>Oura ring — live motion</title>
<style>
 html,body{margin:0;height:100%;background:#0b0d12;color:#cdd6f4;font:13px ui-monospace,monospace;overflow:hidden}
 canvas{display:block}
 #panel{position:fixed;top:10px;left:12px;background:#11131aee;border:1px solid #313244;border-radius:8px;padding:10px 12px;line-height:1.7;min-width:230px}
 #panel b{color:#89dceb}
 .row{display:flex;justify-content:space-between;gap:10px;align-items:center}
 input[type=range]{width:120px}
 button{background:#1e2030;color:#cdd6f4;border:1px solid #45475a;border-radius:6px;padding:5px 10px;cursor:pointer;margin:2px 0}
 button.on{background:#a6e3a1;color:#11131a;border-color:#a6e3a1}
 .dim{color:#7f849c}
 .warn{color:#f9e2af;font-size:12px}
</style></head>
<body>
<canvas id="c"></canvas>
<div id="panel">
 <div><b>Oura ring — live motion</b></div>
 <div class="row"><span>stream</span><span><button id="start">Start</button> <button id="stop">Stop</button></span></div>
 <div class="row"><span class="dim">status</span><span id="status" class="dim">idle</span></div>
 <hr style="border-color:#313244"/>
 <div class="row"><span>|a|</span><span><span id="mag">--</span> g</span></div>
 <div class="row"><span>rate</span><span><span id="rate">--</span> Hz</span></div>
 <div class="row"><span>pitch / roll</span><span><span id="pr">-- / --</span>°</span></div>
 <hr style="border-color:#313244"/>
 <div class="row"><span>smoothing</span><input id="alpha" type="range" min="1" max="40" value="8"></div>
 <div class="row"><span>damping</span><input id="damp" type="range" min="80" max="100" value="97"></div>
 <div class="row"><span>still thresh.</span><input id="zupt" type="range" min="1" max="30" value="4"></div>
 <div class="row"><span>counts/g</span><input id="cpg" type="range" min="500" max="2000" value="1024"></div>
 <div class="row"><span>path scale</span><input id="pscale" type="range" min="5" max="200" value="40"></div>
 <div class="row"><span>invert vertical ↕</span><input id="flipy" type="checkbox"></div>
 <button id="reset">Reset path</button>
 <div class="warn">trajectory drifts (no live gyro) — drag to rotate view</div>
</div>
<script>
const cv=document.getElementById('c'),ctx=cv.getContext('2d');
function resize(){cv.width=innerWidth;cv.height=innerHeight;} addEventListener('resize',resize);resize();

// view (orbit) controls
let az=0.6,el=0.5,drag=false,px=0,py=0;
cv.addEventListener('mousedown',e=>{drag=true;px=e.clientX;py=e.clientY});
addEventListener('mouseup',()=>drag=false);
addEventListener('mousemove',e=>{if(!drag)return;az+=(e.clientX-px)*0.01;el+=(e.clientY-py)*0.01;el=Math.max(-1.5,Math.min(1.5,el));px=e.clientX;py=e.clientY});

// settings
const $=id=>document.getElementById(id);
const set={get alpha(){return $('alpha').value/100},get damp(){return $('damp').value/100},
 get zupt(){return +$('zupt').value/100},get cpg(){return +$('cpg').value},get pscale(){return +$('pscale').value}};

// vec helpers
const sub=(a,b)=>[a[0]-b[0],a[1]-b[1],a[2]-b[2]],add=(a,b)=>[a[0]+b[0],a[1]+b[1],a[2]+b[2]];
const sc=(a,s)=>[a[0]*s,a[1]*s,a[2]*s],dot=(a,b)=>a[0]*b[0]+a[1]*b[1]+a[2]*b[2];
const cross=(a,b)=>[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
const len=a=>Math.hypot(a[0],a[1],a[2]),norm=a=>{const l=len(a)||1;return sc(a,1/l)};

// project a world point to screen
function proj(p){
 const ca=Math.cos(az),sa=Math.sin(az),ce=Math.cos(el),se=Math.sin(el);
 let x=p[0]*ca-p[2]*sa, z=p[0]*sa+p[2]*ca, y=p[1];
 let y2=y*ce-z*se;
 return [cv.width/2+x*60, cv.height/2-y2*60];
}
function line(a,b,col){const A=proj(a),B=proj(b);ctx.strokeStyle=col;ctx.beginPath();ctx.moveTo(A[0],A[1]);ctx.lineTo(B[0],B[1]);ctx.stroke();}

let G=null,vel=[0,0,0],pos=[0,0,0],still=0,trail=[],frames=0,rate=0,mag=0,pitch=0,roll=0,streaming=false;

function feed(d){
 const raw=[d.x,d.y,d.z];
 G=G?add(sc(G,1-set.alpha),sc(raw,set.alpha)):raw.slice();
 // accelerometer reads +1g along "up" at rest (up = +G); the toggle flips it
 const up=norm(sc(G, $('flipy').checked?-1:1)); mag=len(raw)/set.cpg;
 pitch=Math.atan2(up[2],up[1])*180/Math.PI; roll=Math.atan2(up[0],up[1])*180/Math.PI;
 // integrate motion in a gravity-referenced frame (up = world Y) so vertical hand
 // motion maps to screen-vertical regardless of how the ring is held (yaw unknown)
 const r0=Math.abs(up[1])<0.9?[0,1,0]:[1,0,0];
 const right=norm(cross(up,r0)), fwd=cross(right,up);
 const linS=sc(sub(raw,G),1/set.cpg);
 const linW=[dot(linS,right),dot(linS,up),dot(linS,fwd)];
 const dt=0.02;
 if(len(linW)<set.zupt){if(++still>8)vel=[0,0,0];}else still=0;
 vel=sc(add(vel,sc(linW,9.81*dt)),set.damp);
 pos=add(pos,sc(vel,dt*set.pscale));
 trail.push(pos.slice()); if(trail.length>800)trail.shift();
 frames++;
}

function draw(){
 requestAnimationFrame(draw);
 ctx.clearRect(0,0,cv.width,cv.height);
 // grid
 for(let i=-5;i<=5;i++){line([i,0,-5],[i,0,5],'#1e2030');line([-5,0,i],[5,0,i],'#1e2030');}
 line([0,0,0],[1.5,0,0],'#f38ba8');line([0,0,0],[0,1.5,0],'#a6e3a1');line([0,0,0],[0,0,1.5],'#89b4fa');
 // trajectory
 ctx.strokeStyle='#f38ba8';ctx.beginPath();
 for(let i=0;i<trail.length;i++){const P=proj(trail[i]);i?ctx.lineTo(P[0],P[1]):ctx.moveTo(P[0],P[1]);}ctx.stroke();
 // ring (disk perpendicular to gravity) at current position
 if(G){const n=norm(G);let t1=norm(cross(n,Math.abs(n[0])<0.9?[1,0,0]:[0,0,1]));let t2=cross(n,t1);
  ctx.strokeStyle='#89b4fa';ctx.beginPath();
  for(let k=0;k<=32;k++){const a=k/32*Math.PI*2;const pt=add(pos,add(sc(t1,0.5*Math.cos(a)),sc(t2,0.5*Math.sin(a))));const P=proj(pt);k?ctx.lineTo(P[0],P[1]):ctx.moveTo(P[0],P[1]);}ctx.stroke();}
 // hud
 $('mag').textContent=mag.toFixed(2);$('rate').textContent=rate.toFixed(0);
 $('pr').textContent=pitch.toFixed(0)+' / '+roll.toFixed(0);
}
draw();
setInterval(()=>{rate=frames;frames=0;},1000);

const es=new EventSource('/stream');
es.onmessage=e=>feed(JSON.parse(e.data));

$('reset').onclick=()=>{trail=[];pos=[0,0,0];vel=[0,0,0];};
const H={headers:{'X-Oura-Viz':'1'}};
$('start').onclick=async()=>{await fetch('/start',H);streaming=true;$('start').classList.add('on');$('stop').classList.remove('on');$('status').textContent='streaming';};
$('stop').onclick=async()=>{await fetch('/stop',H);streaming=false;$('start').classList.remove('on');$('stop').classList.add('on');$('status').textContent='stopped';};
</script>
</body></html>"##;
