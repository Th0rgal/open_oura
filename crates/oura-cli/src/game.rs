//! "Ring Runner" — a tilt-controlled asteroid-dodging game driven by the ring.
//!
//! Same live-ACM pipeline as [`crate::viz`]: the page derives the gravity vector
//! from the accelerometer and turns the ring's *orientation* (pitch/roll) into an
//! absolute analog stick — which, unlike the integrated trajectory, does not
//! drift. On Start it watches the hand for ~3 s to capture a neutral pose, then
//! steers a ship through an oncoming asteroid field. The HTTP/SSE plumbing lives
//! in [`crate::motion_server`].
//!
//! The page is a self-contained WebGL renderer + synthesized WebAudio engine —
//! no external scripts/CDN, matching the rest of the repo.

use anyhow::Result;

use oura_link::ble::BleTransport;
use oura_link::OuraClient;

/// Serve the game at `127.0.0.1:port` (see [`crate::motion_server::run`]).
pub async fn run(client: OuraClient<BleTransport>, port: u16, minutes: u16) -> Result<()> {
    crate::motion_server::run(client, port, minutes, INDEX_HTML).await
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>RING RUNNER</title>
<style>
 :root{--cy:#3fe0ff;--cy2:#7af7ff;--amber:#ffd36b;--red:#ff506e;--ink:#04070e;}
 *{box-sizing:border-box}
 html,body{margin:0;height:100%;background:var(--ink);color:#cfe9f2;
   font:12px/1.5 ui-monospace,"SF Mono",Menlo,monospace;overflow:hidden;user-select:none}
 canvas{display:block;position:fixed;inset:0;width:100vw;height:100vh}
 /* CRT scanlines + vignette */
 #fx{position:fixed;inset:0;z-index:4;pointer-events:none;
   background:repeating-linear-gradient(0deg,rgba(0,0,0,0) 0 2px,rgba(0,0,0,.10) 2px 3px),
              radial-gradient(120% 90% at 50% 45%,rgba(0,0,0,0) 55%,rgba(0,0,0,.55) 100%);
   mix-blend-mode:multiply}
 /* cockpit corner brackets */
 .br{position:fixed;width:34px;height:34px;z-index:4;pointer-events:none;border:2px solid rgba(63,224,255,.45);
   filter:drop-shadow(0 0 4px rgba(63,224,255,.5))}
 .br.tl{top:14px;left:14px;border-right:0;border-bottom:0}
 .br.tr{top:14px;right:14px;border-left:0;border-bottom:0}
 .br.bl{bottom:14px;left:14px;border-right:0;border-top:0}
 .br.brr{bottom:14px;right:14px;border-left:0;border-top:0}

 .hud{position:fixed;z-index:5;background:linear-gradient(160deg,rgba(10,20,30,.62),rgba(6,12,20,.5));
   border:1px solid rgba(63,224,255,.35);backdrop-filter:blur(7px);
   box-shadow:0 0 18px rgba(0,0,0,.5),inset 0 0 22px rgba(63,224,255,.06);
   clip-path:polygon(0 10px,10px 0,100% 0,100% calc(100% - 10px),calc(100% - 10px) 100%,0 100%)}
 .lbl{text-transform:uppercase;letter-spacing:2.5px;font-size:10px;color:#5f9fb5}
 .val{color:var(--cy2)}

 #sys{top:22px;left:22px;padding:12px 14px;min-width:208px}
 #sys .ttl{font-size:15px;letter-spacing:5px;font-weight:700;color:var(--cy);
   text-shadow:0 0 10px rgba(63,224,255,.7);margin-bottom:2px}
 #sys .ttl b{color:var(--amber);text-shadow:0 0 10px rgba(255,211,107,.6)}
 .row{display:flex;justify-content:space-between;gap:14px;align-items:center;margin:3px 0}
 .ctl{display:flex;gap:7px;margin-top:9px}
 button{flex:1;background:rgba(63,224,255,.06);color:var(--cy2);border:1px solid rgba(63,224,255,.4);
   padding:7px 10px;cursor:pointer;text-transform:uppercase;letter-spacing:2px;font:inherit;font-size:11px;
   transition:.12s;clip-path:polygon(0 0,calc(100% - 7px) 0,100% 7px,100% 100%,7px 100%,0 calc(100% - 7px))}
 button:hover{background:rgba(63,224,255,.16);box-shadow:0 0 12px rgba(63,224,255,.35)}
 button.on{background:rgba(63,224,255,.28);color:#eafdff;border-color:var(--cy2);box-shadow:0 0 14px rgba(63,224,255,.5)}
 button.warn.on{background:rgba(255,80,110,.25);border-color:var(--red);box-shadow:0 0 14px rgba(255,80,110,.4)}
 .mini{margin-top:9px;display:flex;gap:7px}
 .mini button{font-size:10px;padding:5px 6px;letter-spacing:1.5px}

 #adv{top:22px;left:250px;padding:12px 14px;width:218px}
 #adv.hidden{display:none}
 #adv .row span:first-child{text-transform:uppercase;letter-spacing:1.5px;font-size:10px;color:#5f9fb5}
 input[type=range]{-webkit-appearance:none;width:108px;height:3px;background:rgba(63,224,255,.25);outline:0}
 input[type=range]::-webkit-slider-thumb{-webkit-appearance:none;width:12px;height:12px;border-radius:50%;
   background:var(--cy);box-shadow:0 0 8px var(--cy);cursor:pointer}
 input[type=checkbox]{accent-color:var(--cy)}

 /* score readout */
 #hud{top:22px;right:22px;text-align:right;z-index:5}
 #hud .sc{font-size:46px;font-weight:700;color:var(--amber);line-height:1;
   text-shadow:0 0 18px rgba(255,211,107,.55);font-variant-numeric:tabular-nums}
 #hud .lbl{margin-top:2px}
 #hud .bt{color:#5f9fb5;font-size:11px;letter-spacing:1px}

 /* attitude reticle */
 #att{position:fixed;left:24px;bottom:24px;z-index:5;width:96px;height:96px;border-radius:50%;
   border:1px solid rgba(63,224,255,.4);background:radial-gradient(circle,rgba(63,224,255,.05),transparent 70%);
   box-shadow:inset 0 0 18px rgba(63,224,255,.12)}
 #att:before,#att:after{content:"";position:absolute;background:rgba(63,224,255,.25)}
 #att:before{left:8px;right:8px;top:50%;height:1px}
 #att:after{top:8px;bottom:8px;left:50%;width:1px}
 #att .ring{position:absolute;inset:30px;border:1px solid rgba(63,224,255,.3);border-radius:50%}
 #stickdot{position:absolute;width:12px;height:12px;border-radius:50%;background:var(--cy);
   box-shadow:0 0 12px var(--cy);left:42px;top:42px}
 #att .cap{position:absolute;bottom:-18px;left:0;right:0;text-align:center;font-size:9px;letter-spacing:2px;color:#5f9fb5}

 /* centre overlay */
 #center{position:fixed;inset:0;z-index:6;display:flex;align-items:center;justify-content:center;flex-direction:column;
   text-align:center;pointer-events:none}
 #center h1{font-size:40px;letter-spacing:8px;margin:0 0 8px;color:var(--cy);font-weight:700;
   text-shadow:0 0 22px rgba(63,224,255,.6)}
 #center.dead h1{color:var(--red);text-shadow:0 0 22px rgba(255,80,110,.6)}
 #center p{margin:5px 0;color:#9fc3d2;max-width:440px;letter-spacing:.5px}
 #center .big{font-size:96px;font-weight:700;color:var(--amber);line-height:1;
   text-shadow:0 0 26px rgba(255,211,107,.5);font-variant-numeric:tabular-nums}
 #center .hint{color:#5f8597;margin-top:16px;letter-spacing:2px;font-size:11px;text-transform:uppercase}
 #center.hidden{display:none}
 .hidden{display:none}
</style></head>
<body>
<canvas id="c"></canvas>
<div id="fx"></div>
<div class="br tl"></div><div class="br tr"></div><div class="br bl"></div><div class="br brr"></div>

<div id="sys" class="hud">
 <div class="ttl">RING<b>·</b>RUNNER</div>
 <div class="row"><span class="lbl">link</span><span id="status" class="val">idle</span></div>
 <div class="row"><span class="lbl">acm rate</span><span><span id="rate" class="val">--</span> hz</span></div>
 <div class="ctl"><button id="start">Engage</button><button id="stop">Halt</button></div>
 <div class="mini"><button id="advBtn">Adv ▸</button><button id="snd" class="on">♪ Sound</button></div>
</div>

<div id="adv" class="hud hidden">
 <div class="row"><span>sensitivity</span><input id="sens" type="range" min="40" max="260" value="120"></div>
 <div class="row"><span>dead-zone</span><input id="dz" type="range" min="0" max="100" value="25"></div>
 <div class="row"><span>smoothing</span><input id="alpha" type="range" min="1" max="40" value="10"></div>
 <div class="row"><span>invert vertical ↕</span><input id="flipy" type="checkbox" checked></div>
 <div class="mini"><button id="recal">Recalibrate</button></div>
 <div class="lbl" style="margin-top:8px">tilt = steer · arrows = test</div>
</div>

<div id="hud">
 <div class="sc"><span id="score">0</span></div>
 <div class="lbl">distance</div>
 <div class="bt">best <span id="best">0</span></div>
</div>

<div id="att">
 <div class="ring"></div><div id="stickdot"></div>
 <div class="cap">ATTITUDE</div>
</div>

<div id="center">
 <h1 id="ctitle">RING RUNNER</h1>
 <div id="cbig" class="big hidden"></div>
 <p id="cbody">Put on your ring and press <b>ENGAGE</b>. Hold your hand still for 3 seconds to set neutral — then tilt to fly.</p>
 <p class="hint" id="chint"></p>
</div>

<script>
const cv=document.getElementById('c');
const $=id=>document.getElementById(id);
const clamp=(v,a,b)=>v<a?a:v>b?b:v;

// ---- settings (advanced panel) -------------------------------------------
const set={
 get alpha(){return $('alpha').value/100;},
 get sens(){return +$('sens').value/100;},
 get dz(){return +$('dz').value/10;},
 get flipy(){return $('flipy').checked;},
};

// ---- vec helpers ---------------------------------------------------------
const add=(a,b)=>[a[0]+b[0],a[1]+b[1],a[2]+b[2]];
const sub=(a,b)=>[a[0]-b[0],a[1]-b[1],a[2]-b[2]];
const sc=(a,s)=>[a[0]*s,a[1]*s,a[2]*s];
const dot=(a,b)=>a[0]*b[0]+a[1]*b[1]+a[2]*b[2];
const cross=(a,b)=>[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
const vlen=a=>Math.hypot(a[0],a[1],a[2]);
const norm=a=>{const l=vlen(a)||1;return sc(a,1/l);};

// ---- mat4 (column-major) -------------------------------------------------
const M={
 mul(a,b){const o=new Float32Array(16);
  for(let i=0;i<4;i++)for(let j=0;j<4;j++){let s=0;for(let k=0;k<4;k++)s+=a[k*4+j]*b[i*4+k];o[i*4+j]=s;}return o;},
 persp(fov,asp,n,f){const t=1/Math.tan(fov/2);return new Float32Array([t/asp,0,0,0, 0,t,0,0, 0,0,(f+n)/(n-f),-1, 0,0,2*f*n/(n-f),0]);},
 look(e,c,up){const f=norm(sub(c,e)),s=norm(cross(f,up)),u=cross(s,f);
  return new Float32Array([s[0],u[0],-f[0],0, s[1],u[1],-f[1],0, s[2],u[2],-f[2],0, -dot(s,e),-dot(u,e),dot(f,e),1]);},
 trans(x,y,z){const m=this.ident();m[12]=x;m[13]=y;m[14]=z;return m;},
 scale(s){return new Float32Array([s,0,0,0, 0,s,0,0, 0,0,s,0, 0,0,0,1]);},
 rx(a){const c=Math.cos(a),s=Math.sin(a);return new Float32Array([1,0,0,0, 0,c,s,0, 0,-s,c,0, 0,0,0,1]);},
 ry(a){const c=Math.cos(a),s=Math.sin(a);return new Float32Array([c,0,-s,0, 0,1,0,0, s,0,c,0, 0,0,0,1]);},
 rz(a){const c=Math.cos(a),s=Math.sin(a);return new Float32Array([c,s,0,0, -s,c,0,0, 0,0,1,0, 0,0,0,1]);},
 ident(){return new Float32Array([1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]);},
};
// compose: T * Rz * Ry * Rx * S
function compose(p,r,s){let m=M.scale(s===undefined?1:s);
 if(r){m=M.mul(M.rx(r[0]||0),m);m=M.mul(M.ry(r[1]||0),m);m=M.mul(M.rz(r[2]||0),m);}
 return M.mul(M.trans(p[0],p[1],p[2]),m);}

// ---- GL setup ------------------------------------------------------------
const gl=cv.getContext('webgl',{antialias:true,alpha:false,preserveDrawingBuffer:true});
let DPR=Math.min(2,window.devicePixelRatio||1);
function resize(){DPR=Math.min(2,window.devicePixelRatio||1);
 cv.width=innerWidth*DPR;cv.height=innerHeight*DPR;gl.viewport(0,0,cv.width,cv.height);}
addEventListener('resize',resize);resize();

function shader(t,src){const s=gl.createShader(t);gl.shaderSource(s,src);gl.compileShader(s);
 if(!gl.getShaderParameter(s,gl.COMPILE_STATUS))console.error(gl.getShaderInfoLog(s));return s;}
function prog(vs,fs){const p=gl.createProgram();gl.attachShader(p,shader(gl.VERTEX_SHADER,vs));
 gl.attachShader(p,shader(gl.FRAGMENT_SHADER,fs));gl.linkProgram(p);
 if(!gl.getProgramParameter(p,gl.LINK_STATUS))console.error(gl.getProgramInfoLog(p));return p;}

const litP=prog(
`attribute vec3 aPos;attribute vec3 aNrm;uniform mat4 uMVP,uM;varying vec3 vN,vW;
 void main(){vec4 w=uM*vec4(aPos,1.);vW=w.xyz;vN=mat3(uM)*aNrm;gl_Position=uMVP*vec4(aPos,1.);}`,
`precision highp float;varying vec3 vN,vW;
 uniform vec3 uCol,uCam,uKey,uKeyC,uFill,uFillC,uAmb,uEmis,uFog;uniform float uSpec,uShin,uFogD;
 void main(){vec3 N=normalize(vN),V=normalize(uCam-vW);if(dot(N,V)<0.)N=-N;vec3 L=normalize(uKey),L2=normalize(uFill);
  float d=max(dot(N,L),0.),d2=max(dot(N,L2),0.);
  vec3 H=normalize(L+V);float s=pow(max(dot(N,H),0.),uShin)*uSpec*(d>0.?1.:0.);
  float rim=pow(1.-max(dot(N,V),0.),3.)*.22;
  vec3 c=uCol*(uAmb+uKeyC*d+uFillC*d2)+uKeyC*s+rim*vec3(.45,.7,1.)+uEmis;
  float f=clamp(exp(-length(uCam-vW)*uFogD),0.,1.);
  gl_FragColor=vec4(mix(uFog,c,f),1.);}`);
const glowP=prog(
`attribute vec3 aPos;attribute vec2 aUV;uniform mat4 uVP;varying vec2 vUV;
 void main(){vUV=aUV;gl_Position=uVP*vec4(aPos,1.);}`,
`precision mediump float;varying vec2 vUV;uniform vec3 uCol;uniform float uA;
 void main(){float d=length(vUV-.5)*2.;float a=smoothstep(1.,0.,d);gl_FragColor=vec4(uCol,a*a*uA);}`);
const starP=prog(
`attribute vec3 aPos;uniform mat4 uP,uV;uniform float uScroll,uRange,uPx;varying float vB;
 void main(){vec3 p=aPos;p.z=mod(p.z+uScroll,uRange)-uRange+8.;vec4 v=uV*vec4(p,1.);
  gl_Position=uP*v;float dz=-v.z;gl_PointSize=clamp(90./dz,1.,4.)*uPx;vB=clamp(1.2-dz/130.,0.,1.);}`,
`precision mediump float;varying float vB;
 void main(){float d=length(gl_PointCoord-.5)*2.;float a=smoothstep(1.,0.,d)*vB;
  gl_FragColor=vec4(vec3(.7,.82,1.)*a,a);}`);

const La={pos:gl.getAttribLocation(litP,'aPos'),nrm:gl.getAttribLocation(litP,'aNrm')};
const Ga={pos:gl.getAttribLocation(glowP,'aPos'),uv:gl.getAttribLocation(glowP,'aUV')};
const Sa={pos:gl.getAttribLocation(starP,'aPos')};

// ---- mesh builders -------------------------------------------------------
function mesh(pos,nrm){const p=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,p);
 gl.bufferData(gl.ARRAY_BUFFER,new Float32Array(pos),gl.STATIC_DRAW);
 const n=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,n);
 gl.bufferData(gl.ARRAY_BUFFER,new Float32Array(nrm),gl.STATIC_DRAW);
 return {p,n,count:pos.length/3};}
function faceN(a,b,c){return norm(cross(sub(b,a),sub(c,a)));}
// flatten indexed verts to per-face flat-shaded triangles
function flat(verts,faces){const pos=[],nrm=[];for(const f of faces){
  const a=verts[f[0]],b=verts[f[1]],c=verts[f[2]];const N=faceN(a,b,c);
  if(!isFinite(N[0]))continue;
  pos.push(a[0],a[1],a[2],b[0],b[1],b[2],c[0],c[1],c[2]);
  for(let k=0;k<3;k++)nrm.push(N[0],N[1],N[2]);}return mesh(pos,nrm);}

function icosa(){const t=(1+Math.sqrt(5))/2;
 let v=[[-1,t,0],[1,t,0],[-1,-t,0],[1,-t,0],[0,-1,t],[0,1,t],[0,-1,-t],[0,1,-t],[t,0,-1],[t,0,1],[-t,0,-1],[-t,0,1]].map(norm);
 let f=[[0,11,5],[0,5,1],[0,1,7],[0,7,10],[0,10,11],[1,5,9],[5,11,4],[11,10,2],[10,7,6],[7,1,8],
        [3,9,4],[3,4,2],[3,2,6],[3,6,8],[3,8,9],[4,9,5],[2,4,11],[6,2,10],[8,6,7],[9,8,1]];
 return {v,f};}
function subdiv(g,n){let {v,f}=g;for(let s=0;s<n;s++){const nf=[],mid={};
  const get=(i,j)=>{const k=i<j?i+'_'+j:j+'_'+i;if(mid[k]!=null)return mid[k];
    const m=norm(sc(add(v[i],v[j]),.5));v.push(m);return mid[k]=v.length-1;};
  for(const t of f){const a=get(t[0],t[1]),b=get(t[1],t[2]),c=get(t[2],t[0]);
    nf.push([t[0],a,c],[t[1],b,a],[t[2],c,b],[a,b,c]);}f=nf;}return {v,f};}
function noise(p,s){return .55*Math.sin(p[0]*1.7+s)+.45*Math.sin(p[1]*2.3+s*1.7)+.4*Math.cos(p[2]*2.9+s*.7)
  +.25*Math.sin((p[0]+p[1]+p[2])*4.1+s*2.1)+.16*Math.cos((p[0]-p[2])*6.+s);}
function asteroid(seed){const g=subdiv(icosa(),2);
 const v=g.v.map(p=>sc(p,1+.17*noise(p,seed)));return flat(v,g.f);}
const ROCKS=[asteroid(1.3),asteroid(7.1),asteroid(13.7),asteroid(21.2),asteroid(30.9)];
const ROCKCOL=[[.52,.49,.45],[.58,.53,.47],[.47,.50,.58],[.6,.55,.49],[.54,.5,.58]];

// primitives
function lathe(profile,seg){const v=[],f=[];const ringStart=[];
 for(let k=0;k<profile.length;k++){ringStart.push(v.length);
  for(let i=0;i<seg;i++){const a=i/seg*Math.PI*2;v.push([profile[k][0]*Math.cos(a),profile[k][0]*Math.sin(a),profile[k][1]]);}}
 for(let k=0;k<profile.length-1;k++)for(let i=0;i<seg;i++){const i2=(i+1)%seg;
  const a=ringStart[k]+i,b=ringStart[k]+i2,c=ringStart[k+1]+i2,d=ringStart[k+1]+i;
  f.push([a,b,c],[a,c,d]);}
 return flat(v,f);}
function box(w,h,d){const x=w/2,y=h/2,z=d/2;
 const v=[[-x,-y,-z],[x,-y,-z],[x,y,-z],[-x,y,-z],[-x,-y,z],[x,-y,z],[x,y,z],[-x,y,z]];
 const f=[[0,3,2],[0,2,1],[4,5,6],[4,6,7],[0,4,7],[0,7,3],[1,2,6],[1,6,5],[3,7,6],[3,6,2],[0,1,5],[0,5,4]];
 return flat(v,f);}
function disk(r,seg){const v=[[0,0,0]],f=[];for(let i=0;i<=seg;i++){const a=i/seg*Math.PI*2;v.push([r*Math.cos(a),r*Math.sin(a),0]);}
 for(let i=1;i<=seg;i++)f.push([0,i,i+1]);return flat(v,f);}
function sphere(sub){const g=subdiv(icosa(),sub);const pos=[],nrm=[];
 for(const t of g.f)for(const idx of t){const p=g.v[idx];pos.push(p[0],p[1],p[2]);nrm.push(p[0],p[1],p[2]);}return mesh(pos,nrm);}

const ME={
 fus:lathe([[0.02,-1.15],[0.12,-0.7],[0.24,-0.2],[0.30,0.25],[0.24,0.55],[0.10,0.72]],12),
 canopy:sphere(1),
 wing:box(1.25,0.05,0.5),
 fin:box(0.05,0.42,0.36),
 eng:lathe([[0.12,0.45],[0.13,0.62],[0.10,0.74]],10),
 engGlow:disk(0.10,12),
 flap:box(0.46,0.035,0.16),
 rudder:box(0.04,0.34,0.16),
 nose:lathe([[0.10,-0.72],[0.02,-1.16]],10),
};

// ship part list: {mesh,col,spec,shin,emis, local:{p,r,s}, dyn?}
const SHIP=[
 {m:ME.fus,col:[.62,.68,.78],spec:.7,shin:48,local:{p:[0,0,0]}},
 {m:ME.canopy,col:[.18,.42,.66],spec:1.0,shin:90,emis:[.02,.05,.09],local:{p:[0,0.11,-0.34],s:0.14}},
 {m:ME.wing,col:[.50,.55,.63],spec:.5,shin:30,local:{p:[-0.62,-0.02,0.12],r:[0,-0.55,0.05]}},
 {m:ME.wing,col:[.50,.55,.63],spec:.5,shin:30,local:{p:[0.62,-0.02,0.12],r:[0,0.55,-0.05]}},
 {m:ME.fin,col:[.22,.62,.74],spec:.6,shin:40,emis:[.01,.05,.06],local:{p:[0,0.18,0.5]}},
 {m:ME.eng,col:[.16,.17,.22],spec:.8,shin:60,local:{p:[-0.26,-0.02,0.0]}},
 {m:ME.eng,col:[.16,.17,.22],spec:.8,shin:60,local:{p:[0.26,-0.02,0.0]}},
 {m:ME.engGlow,col:[0,0,0],spec:0,shin:1,emis:[.25,.95,1.0],local:{p:[-0.26,-0.02,0.745]}},
 {m:ME.engGlow,col:[0,0,0],spec:0,shin:1,emis:[.25,.95,1.0],local:{p:[0.26,-0.02,0.745]}},
 // control surfaces (deflect with steering)
 {m:ME.flap,col:[.46,.5,.58],spec:.5,shin:30,local:{p:[-0.95,-0.02,0.42],r:[0,-0.55,0]},dyn:'aileronL'},
 {m:ME.flap,col:[.46,.5,.58],spec:.5,shin:30,local:{p:[0.95,-0.02,0.42],r:[0,0.55,0]},dyn:'aileronR'},
 {m:ME.rudder,col:[.22,.62,.74],spec:.6,shin:40,local:{p:[0,0.18,0.64]},dyn:'rudder'},
];

// glow billboard (camera-facing quad), rebuilt per draw
const gBuf=gl.createBuffer(),gUV=gl.createBuffer();
gl.bindBuffer(gl.ARRAY_BUFFER,gUV);gl.bufferData(gl.ARRAY_BUFFER,new Float32Array([0,0,1,0,1,1,0,0,1,1,0,1]),gl.STATIC_DRAW);
let camR=[1,0,0],camU=[0,1,0];
function drawGlow(center,size,col,a,VP){
 const r=sc(camR,size),u=sc(camU,size);
 const p=[],c=center;
 const A=sub(sub(c,r),u),B=sub(add(c,r),u),C=add(add(c,r),u),D=add(sub(c,r),u);
 p.push(...A,...B,...C,...A,...C,...D);
 gl.bindBuffer(gl.ARRAY_BUFFER,gBuf);gl.bufferData(gl.ARRAY_BUFFER,new Float32Array(p),gl.DYNAMIC_DRAW);
 gl.enableVertexAttribArray(Ga.pos);gl.vertexAttribPointer(Ga.pos,3,gl.FLOAT,false,0,0);
 gl.bindBuffer(gl.ARRAY_BUFFER,gUV);gl.enableVertexAttribArray(Ga.uv);gl.vertexAttribPointer(Ga.uv,2,gl.FLOAT,false,0,0);
 gl.uniform3fv(gl.getUniformLocation(glowP,'uCol'),col);gl.uniform1f(gl.getUniformLocation(glowP,'uA'),a);
 gl.uniformMatrix4fv(gl.getUniformLocation(glowP,'uVP'),false,VP);
 gl.drawArrays(gl.TRIANGLES,0,6);}

// starfield buffer
const STAR_RANGE=170,STAR_N=420;
const starArr=new Float32Array(STAR_N*3);
for(let i=0;i<STAR_N;i++){starArr[i*3]=(Math.random()*2-1)*44;starArr[i*3+1]=(Math.random()*2-1)*30;starArr[i*3+2]=-Math.random()*STAR_RANGE;}
const starBuf=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,starBuf);gl.bufferData(gl.ARRAY_BUFFER,starArr,gl.STATIC_DRAW);
let starScroll=0;

// ---- live sensor state ---------------------------------------------------
let G=null, haveSample=false, frames=0, rate=0;
let u0=null, bR=[1,0,0], bF=[0,0,1];
function feed(d){const raw=[d.x,d.y,d.z];
 G=G?add(sc(G,1-set.alpha),sc(raw,set.alpha)):raw.slice();haveSample=true;frames++;}

// ---- game state ----------------------------------------------------------
let state='idle';
let calibG=[0,0,0],calibN=0,calibStart=0;
let ship={x:0,y:0}, rocks=[], score=0, best=+(localStorage.ringRunnerBest||0), tStart=0, spawnAcc=0, last=0;
let sx=0,sy=0,psx=0,psy=0, thrust=0.4, shake=0, ex=[], paused=false;
const keys={};

function beginCalibration(){state='calibrating';calibG=[0,0,0];calibN=0;calibStart=0;
 $('status').textContent='calibrating';$('center').classList.remove('dead');
 showCenter('HOLD STILL','','keep your hand in a neutral, comfortable steering pose');}
function startGame(){
 u0=norm(calibN?calibG:(G||[0,1,0]));
 const seed=Math.abs(dot([1,0,0],u0))<0.9?[1,0,0]:[0,0,1];
 bR=norm(sub(seed,sc(u0,dot(seed,u0))));
 bF=cross(u0,bR);
 ship={x:0,y:0};rocks=[];ex=[];score=0;spawnAcc=0;sx=sy=psx=psy=0;paused=false;tStart=performance.now();state='playing';
 for(let i=0;i<7;i++)spawnRock(-200+Math.random()*170);   // light pre-fill across the full depth
 $('status').textContent='flying';hideCenter();}
function die(){state='dead';best=Math.max(best,Math.floor(score));localStorage.ringRunnerBest=best;
 $('status').textContent='wrecked';shake=1.0;boom();
 for(let i=0;i<26;i++){const dir=norm([Math.random()*2-1,Math.random()*2-1,Math.random()*2-1]);
  ex.push({p:[ship.x,ship.y,0],v:sc(dir,3+Math.random()*5),life:1});}
 $('center').classList.add('dead');
 showCenter('HULL BREACHED','','press SPACE to fly again');
 $('cbig').classList.remove('hidden');$('cbig').textContent=Math.floor(score);}
function showCenter(t,big,hint){$('center').classList.remove('hidden');$('ctitle').textContent=t;
 if(big===''){$('cbig').classList.add('hidden');}else{$('cbig').classList.remove('hidden');$('cbig').textContent=big;}
 $('cbody').style.display=(t==='RING RUNNER')?'block':'none';$('chint').innerHTML=hint||'';}
function hideCenter(){$('center').classList.add('hidden');}
function spawnRock(z){const s=0.2+Math.pow(Math.random(),2.3)*3.2;   // lots of small, rare big
 const spread=4.2+s*1.7;                                            // big boulders sit wider out (scenery)
 rocks.push({x:(Math.random()*2-1)*spread,y:(Math.random()*2-1)*(2.8+s*0.9),z:z,s:s,mi:(Math.random()*ROCKS.length)|0,
 ax:norm([Math.random()*2-1,Math.random()*2-1,Math.random()*2-1]),aa:Math.random()*6.28,spin:(Math.random()-0.5)*1.6});}
function togglePause(){if(state!=='playing')return;paused=!paused;
 if(paused){$('status').textContent='paused';showCenter('PAUSED','','press SPACE to resume');
  if(eng&&AC)eng.master.gain.setTargetAtTime(0,AC.currentTime,0.1);}
 else{hideCenter();$('status').textContent='flying';
  if(eng&&AC&&!muted)eng.master.gain.setTargetAtTime(0.9,AC.currentTime,0.05);}}

// ---- control -------------------------------------------------------------
function axis(delta){const dz=set.dz,a=Math.abs(delta);if(a<dz)return 0;
 const range=Math.max(4,34/set.sens);return clamp(Math.sign(delta)*(a-dz)/range,-1,1);}
function tilt(){if(!u0||!G)return [0,0];const u=norm(G);
 return [Math.asin(clamp(dot(u,bR),-1,1))*180/Math.PI,Math.asin(clamp(dot(u,bF),-1,1))*180/Math.PI];}
function stickTarget(){
 if(keys.ArrowLeft||keys.ArrowRight||keys.ArrowUp||keys.ArrowDown)
  return [(keys.ArrowRight?1:0)-(keys.ArrowLeft?1:0),(keys.ArrowDown?1:0)-(keys.ArrowUp?1:0)];
 const [h,v]=tilt();return [axis(h),(set.flipy?1:-1)*axis(v)];}

// ---- update --------------------------------------------------------------
function update(dt,now){
 if(state==='calibrating'){
  if(haveSample&&G){if(!calibStart)calibStart=now;calibG=add(calibG,norm(G));calibN++;
   const left=3-(now-calibStart)/1000;showCenter('HOLD STILL',Math.max(0,Math.ceil(left)),'capturing neutral pose');
   if(left<=0)startGame();}
  else showCenter('HOLD STILL','','waiting for ring data… (press Engage)');
  return;}
 if(shake>0)shake=Math.max(0,shake-dt*1.6);
 if(ex.length){for(const e of ex){e.p=add(e.p,sc(e.v,dt));e.v=sc(e.v,0.94);e.life-=dt*1.1;}ex=ex.filter(e=>e.life>0);}
 if(state!=='playing'||paused)return;
 const t=(now-tStart)/1000;
 const speed=24+t*1.0, interval=Math.max(0.6,1.15-t*0.008);
 const [nx,ny]=stickTarget();
 const f=1-Math.pow(0.0001,dt);
 sx+=(nx-sx)*f;sy+=(ny-sy)*f;
 ship.x+=(nx*3.6-ship.x)*f;ship.y+=(ny*2.3-ship.y)*f;
 ship.x=clamp(ship.x,-4.4,4.4);ship.y=clamp(ship.y,-2.8,2.8);
 spawnAcc+=dt;while(spawnAcc>=interval){spawnAcc-=interval;spawnRock(-200);}
 const shipR=0.6;
 for(let i=rocks.length-1;i>=0;i--){const o=rocks[i];o.z+=speed*dt;o.aa+=o.spin*dt;
  if(o.z>7){rocks.splice(i,1);score+=10;continue;}
  if(o.z>-1.3&&o.z<1.3&&Math.hypot(o.x-ship.x,o.y-ship.y)<o.s*1.0+shipR){die();break;}}
 score+=dt*8;
 // visuals + audio drive
 const steer=Math.hypot(sx,sy);
 thrust+=(0.4+clamp(steer,0,1)*0.85-thrust)*Math.min(1,dt*6);
 const dsteer=Math.hypot(nx-psx,ny-psy)/Math.max(dt,1e-3);psx=nx;psy=ny;
 updateAudio(steer,dsteer,speed);
}

// ---- render --------------------------------------------------------------
const ZCAM=6.2;   // camera distance behind the ship
const KEY=norm([0.5,0.8,0.35]),FILL=norm([-0.6,-0.2,0.4]);
const FOG=[0.02,0.05,0.10];
let _VP=null;
function setLit(P,V,cam){gl.useProgram(litP);
 _VP=M.mul(P,V);
 gl.uniform3fv(gl.getUniformLocation(litP,'uCam'),cam);
 gl.uniform3fv(gl.getUniformLocation(litP,'uKey'),KEY);gl.uniform3fv(gl.getUniformLocation(litP,'uKeyC'),[1.25,1.15,1.0]);
 gl.uniform3fv(gl.getUniformLocation(litP,'uFill'),FILL);gl.uniform3fv(gl.getUniformLocation(litP,'uFillC'),[0.28,0.45,0.7]);
 gl.uniform3fv(gl.getUniformLocation(litP,'uAmb'),[0.22,0.26,0.34]);
 gl.uniform3fv(gl.getUniformLocation(litP,'uFog'),FOG);gl.uniform1f(gl.getUniformLocation(litP,'uFogD'),0.018);}
function drawLit(m,model,col,spec,shin,emis){
 gl.uniformMatrix4fv(gl.getUniformLocation(litP,'uMVP'),false,M.mul(_VP,model));
 gl.uniformMatrix4fv(gl.getUniformLocation(litP,'uM'),false,model);
 gl.uniform3fv(gl.getUniformLocation(litP,'uCol'),col);
 gl.uniform1f(gl.getUniformLocation(litP,'uSpec'),spec);gl.uniform1f(gl.getUniformLocation(litP,'uShin'),shin);
 gl.uniform3fv(gl.getUniformLocation(litP,'uEmis'),emis||[0,0,0]);
 gl.bindBuffer(gl.ARRAY_BUFFER,m.p);gl.enableVertexAttribArray(La.pos);gl.vertexAttribPointer(La.pos,3,gl.FLOAT,false,0,0);
 gl.bindBuffer(gl.ARRAY_BUFFER,m.n);gl.enableVertexAttribArray(La.nrm);gl.vertexAttribPointer(La.nrm,3,gl.FLOAT,false,0,0);
 gl.drawArrays(gl.TRIANGLES,0,m.count);}

function render(now,dt){
 const asp=cv.width/cv.height;
 const P=M.persp(1.0,asp,0.1,400);
 const shk=shake*0.18;
 // camera looks straight down the lane so the ship sits dead-centre; rocks spawn
 // far and grow as they approach. only a tiny idle sway + death shake.
 const eye=[Math.sin(now*0.0006)*0.04+(Math.random()*2-1)*shk,(Math.random()*2-1)*shk,ZCAM];
 const ctr=[0,0,-9];
 const V=M.look(eye,ctr,[0,1,0]);
 const VP=M.mul(P,V);
 // camera basis for billboards
 const fwd=norm(sub(ctr,eye));camR=norm(cross(fwd,[0,1,0]));camU=cross(camR,fwd);

 gl.clearColor(FOG[0],FOG[1],FOG[2],1);gl.clear(gl.COLOR_BUFFER_BIT|gl.DEPTH_BUFFER_BIT);

 // stars (additive, no depth)
 gl.useProgram(starP);gl.disable(gl.DEPTH_TEST);gl.enable(gl.BLEND);gl.blendFunc(gl.SRC_ALPHA,gl.ONE);
 starScroll=(starScroll+(state==='playing'?60:14)*dt)%STAR_RANGE;
 gl.uniformMatrix4fv(gl.getUniformLocation(starP,'uP'),false,P);
 gl.uniformMatrix4fv(gl.getUniformLocation(starP,'uV'),false,V);
 gl.uniform1f(gl.getUniformLocation(starP,'uScroll'),starScroll);gl.uniform1f(gl.getUniformLocation(starP,'uRange'),STAR_RANGE);gl.uniform1f(gl.getUniformLocation(starP,'uPx'),DPR);
 gl.bindBuffer(gl.ARRAY_BUFFER,starBuf);gl.enableVertexAttribArray(Sa.pos);gl.vertexAttribPointer(Sa.pos,3,gl.FLOAT,false,0,0);
 gl.drawArrays(gl.POINTS,0,STAR_N);

 // lit scene
 gl.enable(gl.DEPTH_TEST);gl.depthMask(true);gl.disable(gl.BLEND);
 setLit(P,V,eye);
 for(const o of rocks){
  const rot=M.mul(M.ry(o.aa),M.rx(o.aa*0.6));const m=M.mul(M.trans(o.x,o.y,o.z),M.mul(rot,M.scale(o.s)));
  drawLit(ROCKS[o.mi],m,ROCKCOL[o.mi],0.35,18);}
 // ship
 if(state==='playing'||state==='dead'){
  const bank=-sx*0.55, pit=sy*0.32, yaw=sx*0.18;
  const base=compose([ship.x,ship.y,0],[pit,yaw,bank],1.5);
  const ail=sx*0.5, elev=sy*0.5, rud=-sx*0.45;
  for(const part of SHIP){
   const l=part.local;let lm=compose(l.p,l.r,l.s===undefined?1:l.s);
   if(part.dyn==='aileronL')lm=M.mul(compose(l.p,[elev-ail,l.r[1],0]),M.scale(1));
   if(part.dyn==='aileronR')lm=M.mul(compose(l.p,[elev+ail,l.r[1],0]),M.scale(1));
   if(part.dyn==='rudder')lm=compose(l.p,[0,rud,0]);
   drawLit(part.m,M.mul(base,lm),part.col,part.spec,part.shin,part.emis);}
 }

 // glow: engine flames + explosion (additive, depth-test, no depth write)
 gl.useProgram(glowP);gl.enable(gl.BLEND);gl.blendFunc(gl.SRC_ALPHA,gl.ONE);gl.depthMask(false);
 if(state==='playing'||state==='dead'){
  const bank=-sx*0.55,pit=sy*0.32,yaw=sx*0.18;const base=compose([ship.x,ship.y,0],[pit,yaw,bank],1.5);
  for(const ex2 of [[-0.26,-0.02,0.85],[0.26,-0.02,0.85]]){
   const w=base, c=[w[0]*ex2[0]+w[4]*ex2[1]+w[8]*ex2[2]+w[12],
                    w[1]*ex2[0]+w[5]*ex2[1]+w[9]*ex2[2]+w[13],
                    w[2]*ex2[0]+w[6]*ex2[1]+w[10]*ex2[2]+w[14]];
   const fl=0.22+thrust*0.30+Math.random()*0.03;
   drawGlow(c,fl,[0.4,0.95,1.0],0.9,VP);drawGlow(c,fl*0.5,[0.9,1.0,1.0],0.9,VP);}
 }
 for(const e of ex){drawGlow(e.p,0.5*e.life+0.15,[1.0,0.6,0.25],e.life,VP);}
 gl.depthMask(true);
}

function loop(now){requestAnimationFrame(loop);
 const dt=Math.min(0.05,(now-(last||now))/1000);last=now;
 update(dt,now);render(now,dt);
 $('score').textContent=Math.floor(score);$('best').textContent=best;
 const [tx,ty]=stickTarget();const d=$('stickdot');d.style.left=(42+tx*30)+'px';d.style.top=(42+ty*30)+'px';
}
requestAnimationFrame(loop);
setInterval(()=>{rate=frames;frames=0;$('rate').textContent=rate;},1000);

// ---- audio (synthesized; created on first gesture) -----------------------
let AC=null,eng=null,muted=false;
function initAudio(){if(AC)return;try{AC=new (window.AudioContext||window.webkitAudioContext)();}catch(e){return;}
 const master=AC.createGain();master.gain.value=0.0;master.connect(AC.destination);
 const lp=AC.createBiquadFilter();lp.type='lowpass';lp.frequency.value=420;
 const oa=AC.createOscillator();oa.type='sawtooth';oa.frequency.value=58;
 const ob=AC.createOscillator();ob.type='sawtooth';ob.frequency.value=87;
 const eg=AC.createGain();eg.gain.value=0.0;oa.connect(eg);ob.connect(eg);eg.connect(lp);lp.connect(master);
 // whoosh from filtered noise
 const nb=AC.createBuffer(1,AC.sampleRate,AC.sampleRate);const ch=nb.getChannelData(0);
 for(let i=0;i<ch.length;i++)ch[i]=Math.random()*2-1;
 const ns=AC.createBufferSource();ns.buffer=nb;ns.loop=true;
 const bp=AC.createBiquadFilter();bp.type='bandpass';bp.frequency.value=900;bp.Q.value=0.7;
 const wg=AC.createGain();wg.gain.value=0;ns.connect(bp);bp.connect(wg);wg.connect(master);
 // low hum
 const hs=AC.createOscillator();hs.type='sine';hs.frequency.value=46;const hg=AC.createGain();hg.gain.value=0.04;
 hs.connect(hg);hg.connect(master);
 oa.start();ob.start();ns.start();hs.start();
 eng={master,lp,oa,ob,eg,wg};
 master.gain.setTargetAtTime(muted?0:0.9,AC.currentTime,0.05);}
function updateAudio(steer,dsteer,speed){if(!eng||muted)return;const t=AC.currentTime;
 eng.eg.gain.setTargetAtTime(0.04+clamp(steer,0,1)*0.16,t,0.08);
 eng.lp.frequency.setTargetAtTime(360+steer*1500+speed*4,t,0.08);
 eng.oa.frequency.setTargetAtTime(56+steer*40+speed*0.3,t,0.08);
 eng.ob.frequency.setTargetAtTime((56+steer*40+speed*0.3)*1.5,t,0.08);
 eng.wg.gain.setTargetAtTime(clamp(dsteer*3.5,0,0.4),t,0.05);}
function boom(){if(!AC||muted)return;const t=AC.currentTime;
 const nb=AC.createBuffer(1,AC.sampleRate*0.6,AC.sampleRate);const ch=nb.getChannelData(0);
 for(let i=0;i<ch.length;i++)ch[i]=(Math.random()*2-1)*Math.pow(1-i/ch.length,2);
 const ns=AC.createBufferSource();ns.buffer=nb;const lp=AC.createBiquadFilter();lp.type='lowpass';
 lp.frequency.setValueAtTime(1800,t);lp.frequency.exponentialRampToValueAtTime(120,t+0.5);
 const g=AC.createGain();g.gain.setValueAtTime(0.9,t);g.gain.exponentialRampToValueAtTime(0.001,t+0.6);
 ns.connect(lp);lp.connect(g);g.connect(eng?eng.master:AC.destination);ns.start();
 const o=AC.createOscillator();o.type='sine';o.frequency.setValueAtTime(120,t);o.frequency.exponentialRampToValueAtTime(38,t+0.4);
 const og=AC.createGain();og.gain.setValueAtTime(0.8,t);og.gain.exponentialRampToValueAtTime(0.001,t+0.5);
 o.connect(og);og.connect(eng?eng.master:AC.destination);o.start();o.stop(t+0.55);}

// ---- wiring --------------------------------------------------------------
const es=new EventSource('/stream');es.onmessage=e=>feed(JSON.parse(e.data));
const H={headers:{'X-Oura-Viz':'1'}};
$('start').onclick=async()=>{initAudio();if(AC&&AC.state==='suspended')AC.resume();
 await fetch('/start',H);$('start').classList.add('on');$('stop').classList.remove('on');beginCalibration();};
$('stop').onclick=async()=>{await fetch('/stop',H);$('stop').classList.add('on');$('start').classList.remove('on');
 state='idle';paused=false;$('status').textContent='stopped';$('center').classList.remove('dead');showCenter('RING RUNNER','','press ENGAGE to stream and fly');};
$('recal').onclick=()=>{if(state==='playing'||state==='dead')beginCalibration();};
$('advBtn').onclick=()=>{const a=$('adv');a.classList.toggle('hidden');
 $('advBtn').classList.toggle('on',!a.classList.contains('hidden'));$('advBtn').textContent=a.classList.contains('hidden')?'Adv ▸':'Adv ▾';};
$('snd').onclick=()=>{muted=!muted;$('snd').classList.toggle('on',!muted);$('snd').textContent=(muted?'✕ Sound':'♪ Sound');
 if(!muted){initAudio();if(AC){AC.resume();eng&&eng.master.gain.setTargetAtTime(0.9,AC.currentTime,0.05);}}
 else if(eng){eng.master.gain.setTargetAtTime(0,AC.currentTime,0.05);}};
addEventListener('keydown',e=>{keys[e.key]=true;if(e.code==='Space'){e.preventDefault();
 if(state==='dead')startGame();else if(state==='idle')$('start').click();else if(state==='playing')togglePause();}});
addEventListener('keyup',e=>{keys[e.key]=false;});
// Stop the ring stream the moment the tab is closed/navigated away (keepalive lets
// the request outlive the page); the server also stops on disconnect as a backstop.
addEventListener('pagehide',()=>{try{fetch('/stop',{headers:H.headers,keepalive:true});}catch(e){}});
$('best').textContent=best;
</script>
</body></html>"##;
