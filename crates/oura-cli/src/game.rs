//! "Ring Runner" — a tilt-controlled asteroid-dodging game driven by the ring.
//!
//! Same live-ACM pipeline as [`crate::viz`]: the page derives the gravity vector
//! from the accelerometer and turns the ring's *orientation* (pitch/roll) into an
//! absolute analog stick — which, unlike the integrated trajectory, does not
//! drift. On Start it watches the hand for ~3 s to capture a neutral pose, then
//! steers a ship through an oncoming asteroid field. The HTTP/SSE plumbing lives
//! in [`crate::motion_server`].

use anyhow::Result;

use oura_core::ble::BleTransport;
use oura_core::OuraClient;

/// Serve the game at `127.0.0.1:port` (see [`crate::motion_server::run`]).
pub async fn run(client: OuraClient<BleTransport>, port: u16, minutes: u16) -> Result<()> {
    crate::motion_server::run(client, port, minutes, INDEX_HTML).await
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>Oura ring — Ring Runner</title>
<style>
 html,body{margin:0;height:100%;background:#05060a;color:#cdd6f4;font:13px ui-monospace,monospace;overflow:hidden;user-select:none}
 canvas{display:block}
 #panel{position:fixed;top:10px;left:12px;background:#0b0d12d9;border:1px solid #313244;border-radius:8px;padding:10px 12px;line-height:1.7;min-width:210px;z-index:5}
 #panel b{color:#89dceb}
 .row{display:flex;justify-content:space-between;gap:10px;align-items:center}
 input[type=range]{width:110px}
 button{background:#1e2030;color:#cdd6f4;border:1px solid #45475a;border-radius:6px;padding:5px 10px;cursor:pointer;margin:2px 0}
 button.on{background:#a6e3a1;color:#11131a;border-color:#a6e3a1}
 .dim{color:#7f849c}
 #hud{position:fixed;top:12px;right:16px;text-align:right;z-index:5;line-height:1.4}
 #hud .sc{font-size:30px;color:#f9e2af;font-weight:bold}
 #hud .bt{color:#7f849c}
 #center{position:fixed;inset:0;display:flex;align-items:center;justify-content:center;flex-direction:column;
   text-align:center;z-index:6;pointer-events:none}
 #center h1{font-size:34px;margin:0 0 6px;color:#cdd6f4;letter-spacing:1px}
 #center p{margin:4px 0;color:#bac2de;max-width:460px}
 #center .big{font-size:80px;color:#89dceb;font-weight:bold;line-height:1}
 #center .hint{color:#7f849c;margin-top:14px}
 #center.hidden{display:none}
 .stick{margin-top:6px;width:64px;height:64px;border:1px solid #313244;border-radius:8px;position:relative;background:#0b0d12}
 .stick i{position:absolute;width:10px;height:10px;border-radius:50%;background:#89dceb;left:27px;top:27px;transition:none}
</style></head>
<body>
<canvas id="c"></canvas>

<div id="panel">
 <div><b>Ring Runner</b></div>
 <div class="row"><span>stream</span><span><button id="start">Start</button> <button id="stop">Stop</button></span></div>
 <div class="row"><span class="dim">status</span><span id="status" class="dim">idle</span></div>
 <div class="row"><span class="dim">rate</span><span><span id="rate">--</span> Hz</span></div>
 <hr style="border-color:#313244"/>
 <div class="row"><span>sensitivity</span><input id="sens" type="range" min="40" max="260" value="120"></div>
 <div class="row"><span>dead-zone</span><input id="dz" type="range" min="0" max="100" value="25"></div>
 <div class="row"><span>smoothing</span><input id="alpha" type="range" min="1" max="40" value="10"></div>
 <div class="row"><span>invert vertical ↕</span><input id="flipy" type="checkbox"></div>
 <button id="recal">Recalibrate</button>
 <div class="row"><span class="dim">stick</span><div class="stick"><i id="stickdot"></i></div></div>
 <div class="dim" style="font-size:11px;margin-top:4px">tilt = steer · arrows = test</div>
</div>

<div id="hud">
 <div class="sc"><span id="score">0</span></div>
 <div class="bt">best <span id="best">0</span></div>
</div>

<div id="center">
 <h1 id="ctitle">Ring Runner</h1>
 <div id="cbig" class="big" style="display:none"></div>
 <p id="cbody">Put on your ring, then press <b>Start</b>. You'll get 3 seconds to hold your hand in a comfortable neutral pose — that becomes the centre of the stick. Then tilt to dodge.</p>
 <p class="hint" id="chint"></p>
</div>

<script>
const cv=document.getElementById('c'),ctx=cv.getContext('2d');
function resize(){cv.width=innerWidth;cv.height=innerHeight;} addEventListener('resize',resize);resize();
const $=id=>document.getElementById(id);

// ---- tunables / settings -------------------------------------------------
const set={
 get alpha(){return $('alpha').value/100;},           // gravity low-pass
 get sens(){return +$('sens').value/100;},            // higher = more responsive
 get dz(){return +$('dz').value/10;},                 // dead-zone, degrees
 get flipy(){return $('flipy').checked;},
};
const FOCAL=340, ZSPAWN=1300, R0MIN=58, R0MAX=104, COLLIDE_Z=70;
const clamp=(v,a,b)=>v<a?a:v>b?b:v;

// ---- vector helpers ------------------------------------------------------
const add=(a,b)=>[a[0]+b[0],a[1]+b[1],a[2]+b[2]];
const sc=(a,s)=>[a[0]*s,a[1]*s,a[2]*s];
const len=a=>Math.hypot(a[0],a[1],a[2]);
const norm=a=>{const l=len(a)||1;return sc(a,1/l);};

// ---- live sensor state ---------------------------------------------------
let G=null, pitch=0, roll=0, haveSample=false, frames=0, rate=0;
function feed(d){
 const raw=[d.x,d.y,d.z];
 G=G?add(sc(G,1-set.alpha),sc(raw,set.alpha)):raw.slice();
 const up=norm(sc(G,set.flipy?-1:1));
 pitch=Math.atan2(up[2],up[1])*180/Math.PI;   // hand tilt fwd/back
 roll =Math.atan2(up[0],up[1])*180/Math.PI;   // hand tilt left/right
 haveSample=true; frames++;
}

// ---- game state ----------------------------------------------------------
let state='idle';            // idle | calibrating | playing | dead
let pitch0=0, roll0=0;       // captured neutral pose
let calibSum=[0,0], calibN=0, calibStart=0;
let ship={x:0,y:0}, rocks=[], stars=[], score=0, best=+(localStorage.ringRunnerBest||0), tStart=0, spawnAcc=0, last=0;
const keys={};

function initStars(){
 stars=[];
 for(let i=0;i<140;i++) stars.push({x:(Math.random()*2-1),y:(Math.random()*2-1),z:Math.random()*ZSPAWN});
}
initStars();

function beginCalibration(){
 state='calibrating'; calibSum=[0,0]; calibN=0; calibStart=0;
 $('status').textContent='calibrating'; showCenter('Hold still','','keep your hand in a neutral, comfortable steering pose');
}
function startGame(){
 pitch0=calibN?calibSum[0]/calibN:pitch;
 roll0 =calibN?calibSum[1]/calibN:roll;
 ship={x:0,y:0}; rocks=[]; score=0; spawnAcc=0; tStart=performance.now(); state='playing';
 $('status').textContent='flying'; hideCenter();
}
function die(){
 state='dead';
 best=Math.max(best,Math.floor(score)); localStorage.ringRunnerBest=best;
 $('status').textContent='wrecked';
 showCenter('Hull breached', '', 'press <b>Space</b> to fly again · <b>Recalibrate</b> to re-centre');
 $('cbig').style.display='block'; $('cbig').textContent=Math.floor(score);
 $('ctitle').textContent='Hull breached';
}
function showCenter(title,big,hint){
 $('center').classList.remove('hidden'); $('ctitle').textContent=title;
 $('cbig').style.display=big===''?'none':'block'; if(big!=='')$('cbig').textContent=big;
 $('cbody').style.display=title==='Hull breached'?'none':'block';
 $('chint').innerHTML=hint||'';
}
function hideCenter(){$('center').classList.add('hidden');}

// ---- control: orientation delta -> normalised stick ----------------------
function axis(delta){
 const dz=set.dz, a=Math.abs(delta);
 if(a<dz) return 0;
 // full deflection range shrinks as sensitivity rises
 const range=Math.max(4,(34/set.sens));
 return clamp(Math.sign(delta)*(a-dz)/range,-1,1);
}
function stickTarget(){
 // keyboard override for testing without a ring
 if(keys.ArrowLeft||keys.ArrowRight||keys.ArrowUp||keys.ArrowDown){
  return [ (keys.ArrowRight?1:0)-(keys.ArrowLeft?1:0),
           (keys.ArrowDown?1:0)-(keys.ArrowUp?1:0) ];
 }
 const nx=axis(roll-roll0);
 const ny=-axis(pitch-pitch0);    // tilt hand up -> ship up
 return [nx,ny];
}

// ---- per-frame update ----------------------------------------------------
function update(dt,now){
 if(state==='calibrating'){
  if(haveSample){
   if(!calibStart) calibStart=now;
   calibSum[0]+=pitch; calibSum[1]+=roll; calibN++;
   const left=3-(now-calibStart)/1000;
   showCenter('Hold still', Math.max(0,Math.ceil(left)), 'capturing your neutral pose…');
   if(left<=0) startGame();
  } else {
   showCenter('Hold still','','waiting for ring data… (press Start to stream)');
  }
  return;
 }
 if(state!=='playing') return;

 const t=(now-tStart)/1000;
 // difficulty ramp
 const speed=520+t*15;                       // world units / s, climbing
 const interval=Math.max(0.30,0.82-t*0.011); // seconds between spawns

 // steer ship (normalised stick -> world position near plane)
 const [nx,ny]=stickTarget();
 const ampX=cv.width*0.46, ampY=cv.height*0.42;
 const tx=nx*ampX, ty=ny*ampY;
 const f=1-Math.pow(0.0001,dt);             // smooth follow (~frame-rate independent)
 ship.x+=(tx-ship.x)*f; ship.y+=(ty-ship.y)*f;

 // spawn
 spawnAcc+=dt;
 while(spawnAcc>=interval){ spawnAcc-=interval;
  rocks.push({x:(Math.random()*2-1)*cv.width*0.5, y:(Math.random()*2-1)*cv.height*0.5,
              z:ZSPAWN, r:R0MIN+Math.random()*(R0MAX-R0MIN), spin:Math.random()*6.28, dspin:(Math.random()-0.5)*2,
              seed:Math.random()*100});
 }

 // advance rocks; score on pass; collide near plane
 const shipR=15;
 for(let i=rocks.length-1;i>=0;i--){const o=rocks[i];
  o.z-=speed*dt; o.spin+=o.dspin*dt;
  if(o.z<=0){rocks.splice(i,1); score+=10; continue;}
  if(o.z<COLLIDE_Z){
   const fo=FOCAL/(o.z+FOCAL), os=[cv.width/2+o.x*fo, cv.height/2+o.y*fo], orad=o.r*fo;
   const ss=[cv.width/2+ship.x, cv.height/2+ship.y];
   if(Math.hypot(os[0]-ss[0],os[1]-ss[1]) < orad*0.74+shipR){ die(); break; }
  }
 }
 score+=dt*8; // survival trickle
}

// ---- rendering -----------------------------------------------------------
function drawStars(dt){
 const speed = state==='playing' ? 520 : 120;
 for(const s of stars){ s.z-=speed*dt; if(s.z<=1){s.x=Math.random()*2-1;s.y=Math.random()*2-1;s.z=ZSPAWN;}
  const f=FOCAL/(s.z+FOCAL), x=cv.width/2+s.x*cv.width*0.5*f, y=cv.height/2+s.y*cv.height*0.5*f;
  const a=clamp(1-s.z/ZSPAWN,0,1); ctx.fillStyle='rgba(180,200,240,'+(0.15+a*0.6)+')';
  ctx.fillRect(x,y,1+a*1.6,1+a*1.6);
 }
}
function drawRock(o){
 const f=FOCAL/(o.z+FOCAL), x=cv.width/2+o.x*f, y=cv.height/2+o.y*f, r=o.r*f;
 const g=ctx.createRadialGradient(x-r*0.3,y-r*0.3,r*0.2,x,y,r);
 g.addColorStop(0,'#6c7086'); g.addColorStop(1,'#272a3a');
 ctx.fillStyle=g; ctx.strokeStyle='#181a26'; ctx.lineWidth=2;
 ctx.beginPath();
 for(let k=0;k<=14;k++){const a=k/14*6.283+o.spin; const rr=r*(0.82+0.22*Math.sin(o.seed+k*1.7));
  const px=x+Math.cos(a)*rr, py=y+Math.sin(a)*rr; k?ctx.lineTo(px,py):ctx.moveTo(px,py);}
 ctx.closePath(); ctx.fill(); ctx.stroke();
}
function drawShip(){
 const x=cv.width/2+ship.x, y=cv.height/2+ship.y;
 ctx.save(); ctx.translate(x,y);
 // engine glow
 const gl=ctx.createRadialGradient(0,16,2,0,16,26); gl.addColorStop(0,'rgba(137,220,235,.9)'); gl.addColorStop(1,'rgba(137,220,235,0)');
 ctx.fillStyle=gl; ctx.beginPath(); ctx.arc(0,16,26,0,6.283); ctx.fill();
 // hull
 ctx.fillStyle='#cdd6f4'; ctx.strokeStyle='#89dceb'; ctx.lineWidth=2;
 ctx.beginPath(); ctx.moveTo(0,-18); ctx.lineTo(13,16); ctx.lineTo(0,9); ctx.lineTo(-13,16); ctx.closePath();
 ctx.fill(); ctx.stroke();
 ctx.fillStyle='#89b4fa'; ctx.beginPath(); ctx.arc(0,-2,3.4,0,6.283); ctx.fill();
 ctx.restore();
}
function frame(now){
 requestAnimationFrame(frame);
 const dt=Math.min(0.05,(now-(last||now))/1000); last=now;
 update(dt,now);

 ctx.fillStyle='#05060a'; ctx.fillRect(0,0,cv.width,cv.height);
 drawStars(dt);
 if(state==='playing'||state==='dead'){
  rocks.sort((a,b)=>b.z-a.z).forEach(drawRock);
  drawShip();
 }
 // hud + stick indicator
 $('score').textContent=Math.floor(score); $('best').textContent=best;
 const [sx,sy]=stickTarget(); const d=$('stickdot');
 d.style.left=(27+sx*24)+'px'; d.style.top=(27+sy*24)+'px';
}
requestAnimationFrame(frame);
setInterval(()=>{rate=frames;frames=0;$('rate').textContent=rate.toFixed(0);},1000);

// ---- wiring --------------------------------------------------------------
const es=new EventSource('/stream');
es.onmessage=e=>feed(JSON.parse(e.data));

const H={headers:{'X-Oura-Viz':'1'}};
$('start').onclick=async()=>{await fetch('/start',H);$('start').classList.add('on');$('stop').classList.remove('on');beginCalibration();};
$('stop').onclick=async()=>{await fetch('/stop',H);$('start').classList.remove('on');$('stop').classList.add('on');state='idle';$('status').textContent='stopped';showCenter('Ring Runner','','press Start to stream and play');};
$('recal').onclick=()=>{ if(state==='playing'||state==='dead') beginCalibration(); };

addEventListener('keydown',e=>{ keys[e.key]=true;
 if(e.code==='Space'){ e.preventDefault();
  if(state==='dead') startGame();             // quick retry, reuse neutral
  else if(state==='idle') $('start').click();
 }
});
addEventListener('keyup',e=>{keys[e.key]=false;});
best&&($('best').textContent=best);
</script>
</body></html>"##;
