// open_oura dashboard — fetch /api/summary (Rust computes it) and render.
"use strict";

const $ = (id) => document.getElementById(id);
const el = (tag, cls, html) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
};
const num = (v, d = "—") => (v == null || Number.isNaN(v) ? d : v);

function sparkline(series, { good = "up" } = {}) {
  const s = (series || []).filter((x) => x != null);
  if (s.length < 2) return "";
  const w = 100, h = 26, min = Math.min(...s), max = Math.max(...s);
  const rng = max - min || 1;
  const pts = s.map((v, i) => {
    const x = (i / (s.length - 1)) * w;
    const y = h - ((v - min) / rng) * (h - 4) - 2;
    return [x, y];
  });
  const d = pts.map((p, i) => (i ? "L" : "M") + p[0].toFixed(1) + " " + p[1].toFixed(1)).join(" ");
  const up = s[s.length - 1] >= s[0];
  const ok = good === "up" ? up : !up;
  const col = ok ? "var(--good)" : "var(--warn)";
  const last = pts[pts.length - 1];
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
    <path d="${d}" fill="none" stroke="${col}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/>
    <circle cx="${last[0].toFixed(1)}" cy="${last[1].toFixed(1)}" r="2.4" fill="${col}"/>
  </svg>`;
}

function deltaTag(pct, { good = "up" } = {}) {
  if (pct == null) return el("span", "delta flat", "");
  const up = pct > 0, flat = pct === 0;
  const cls = flat ? "flat" : (good === "up" ? up : !up) ? "up" : "down";
  const sign = pct > 0 ? "+" : "";
  return el("span", "delta " + cls, `${sign}${pct}% vs baseline`);
}

function tile(label, value, unit, deltaPct, series, opts = {}) {
  const t = el("article", "tile");
  t.append(el("div", "label", label));
  const v = el("div", "value", `${num(value)}<span class="unit">${unit || ""}</span>`);
  t.append(v);
  if (deltaPct !== undefined) t.append(deltaTag(deltaPct, opts));
  if (series) t.append(el("div", "spark", sparkline(series, opts)));
  return t;
}

function renderTiles(d) {
  const box = $("tiles");
  box.innerHTML = "";
  box.classList.add("reveal");
  const hv = d.vitals?.hrv || {}, rh = d.vitals?.rhr || {};
  const n0 = (d.nights || [])[0] || {};
  const effSeries = [...(d.nights || [])].reverse().map((n) => n.efficiency).filter((x) => x != null);
  box.append(tile("HRV (rmssd)", hv.latest, " ms", hv.delta_pct, hv.series, { good: "up" }));
  box.append(tile("Resting HR", rh.latest, " bpm", rh.delta_pct, rh.series, { good: "down" }));
  box.append(tile("Sleep efficiency", n0.efficiency, "%", undefined, effSeries, { good: "up" }));
  const cv = d.cardio;
  if (cv && cv.vascular_age != null) {
    const t = tile("Vascular age", cv.vascular_age, " yr");
    const diff = Math.round((cv.vascular_age - cv.chronological_age) * 10) / 10;
    t.append(el("div", "sub", `${diff <= 0 ? diff : "+" + diff} yr vs your age`));
    box.append(t);
  } else {
    box.append(tile("Vascular age", "—", "", undefined, null));
  }
}

function hypnogram(stages) {
  const wrap = el("div", "hyp");
  (stages || []).forEach((s) => wrap.append(el("i", "s" + s)));
  return wrap;
}

function renderNights(d) {
  const box = $("nights");
  box.innerHTML = "";
  $("sleep-legend").hidden = false;
  if (!d.nights || !d.nights.length) {
    box.append(el("div", "error", "No overnight data yet. Wear the ring and sync."));
    return;
  }
  d.nights.forEach((n) => {
    const row = el("div", "night");
    const meta = el("div", "meta");
    meta.append(el("div", "date", n.date));
    meta.append(el("div", "span", `${n.start}-${n.end} · ${num(n.in_bed_h)}h`));
    row.append(meta);

    const right = el("div", "right");
    if (n.stages && n.stages.length) right.append(hypnogram(n.stages));
    else right.append(el("div", "hyp"));

    // row 1: sleep composition
    const comp = el("div", "breakdown");
    const seg = (lbl, v) => `<span>${lbl} <b>${num(v)}%</b></span>`;
    comp.innerHTML =
      seg("Deep", n.deep_pct) + seg("Light", n.light_pct) + seg("REM", n.rem_pct) + seg("Awake", n.wake_pct);
    right.append(comp);

    // row 2: recovery vitals
    const vit = el("div", "breakdown vitals-row");
    const bits = [`Efficiency <b>${num(n.efficiency)}%</b>`];
    if (n.hrv_ms != null) bits.push(`HRV <b>${n.hrv_ms}</b> ms`);
    if (n.rhr != null) bits.push(`Resting HR <b>${n.rhr}</b>`);
    if (n.spo2_mean != null) bits.push(`O₂ <b>${n.spo2_mean}%</b>`);
    vit.innerHTML = bits.map((b) => `<span>${b}</span>`).join("");
    right.append(vit);

    row.append(right);
    box.append(row);
  });
}

function renderCardio(d) {
  const box = $("cardio");
  const cv = d.cardio;
  box.innerHTML = "";
  if (!cv || cv.vascular_age == null) {
    box.append(el("div", "error", "Cardiovascular age needs the cva_ppg feature on. Enable it, then sync overnight."));
    return;
  }
  box.append(el("div", "big-metric", `<span class="n">${cv.vascular_age}</span><span class="u">years vascular age</span>`));
  const diff = Math.round((cv.vascular_age - cv.chronological_age) * 10) / 10;
  box.append(el("div", "sub", `${diff <= 0 ? Math.abs(diff) + " years younger than" : diff + " years above"} your chronological age (${cv.chronological_age})`));
  const kvs = el("div", "kvs");
  kvs.append(el("div", "kv", `<div class="k">Pulse-wave velocity</div><div class="v">${cv.pwv_ms} m/s</div>`));
  kvs.append(el("div", "kv", `<div class="k">Segments analysed</div><div class="v">${cv.segments}</div>`));
  box.append(kvs);
}

function renderSpo2(d) {
  const box = $("spo2");
  box.innerHTML = "";
  const n0 = (d.nights || []).find((n) => n.spo2_mean != null);
  if (!n0) {
    box.append(el("div", "error", "Blood oxygen needs the spo2 feature on overnight."));
    return;
  }
  box.append(el("div", "big-metric", `<span class="n">${n0.spo2_mean}</span><span class="u">% avg, last night</span>`));
  box.append(el("div", "sub", `Calibrated from the ring's R-ratio (Oura's own curve).`));
  const pct = Math.max(0, Math.min(100, ((n0.spo2_mean - 85) / 15) * 100));
  const g = el("div", "gauge");
  const fill = el("i");
  fill.style.width = pct + "%";
  g.append(fill);
  box.append(g);
  box.append(el("div", "scale", `<span>85</span><span>healthy ≥ 95</span><span>100</span>`));
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const fmtDay = (ymd) => {
  const p = (ymd || "").split("-");
  return p.length === 3 ? `${MONTHS[+p[1] - 1]} ${+p[2]}` : ymd;
};

function renderActivity(d) {
  const box = $("activity");
  box.innerHTML = "";
  const a = (d.activity || []).slice().reverse().slice(0, 12);
  if (!a.length) {
    box.append(el("div", "error", "No activity sessions detected."));
    return;
  }
  const list = el("div", "sessions");
  let lastDay = null;
  a.forEach((s) => {
    const ymd = (s.start || "").split(" ")[0];
    if (ymd !== lastDay) {
      list.append(el("div", "day-head", fmtDay(ymd)));
      lastDay = ymd;
    }
    const row = el("div", "session");
    row.append(el("div", "when", `${(s.start || "").split(" ")[1] || ""}-${s.end || ""}`));
    const what = el("div", "what");
    what.append(el("span", "name", s.label || "activity"));
    if (s.is_workout >= 0.5) what.append(el("span", "tag", "workout"));
    row.append(what);
    row.append(el("div", "dur", `${s.duration_min} min`));
    list.append(row);
  });
  box.append(list);
}

function renderDevice(d) {
  const box = $("device");
  const dev = d.device || {};
  box.innerHTML = "";

  const stats = el("div", "dh-stats");
  const stat = (k, v, u) => el("div", "dh-stat", `<div class="k">${k}</div><div class="v">${v}<span class="u">${u || ""}</span></div>`);
  const fresh = dev.fresh_hours != null ? (dev.fresh_hours < 1 ? "<1" : Math.round(dev.fresh_hours)) : "—";
  stats.append(stat("Last sync", fresh, " h ago"));
  stats.append(stat("History", num(dev.days_of_data), " days"));
  stats.append(stat("Events stored", (dev.total_events || 0).toLocaleString()));
  box.append(stats);

  const left = el("div");
  left.append(el("p", "subhead", "What your ring is measuring"));
  const chips = el("div", "chips");
  (dev.measuring || []).forEach((m) => {
    const c = el("span", "measure", `<span class="dot"></span>${m.name}`);
    c.setAttribute("data-on", String(!!m.on));
    chips.append(c);
  });
  left.append(chips);
  box.append(left);

  const right = el("div");
  right.append(el("p", "subhead", "Insights available"));
  const ins = el("div", "insights");
  (dev.insights || []).forEach((i) => {
    const row = el("div", "insight");
    const l = el("div", null, `${i.name}${i.status === "gated" && i.why ? `<span class="why"> · ${i.why}</span>` : ""}`);
    row.append(l);
    row.append(el("span", "status " + i.status, i.status));
    ins.append(row);
  });
  right.append(ins);
  box.append(right);
}

function renderSync(d) {
  const dev = d.device || {};
  const fresh = dev.fresh_hours;
  const dot = document.querySelector("#sync-chip .dot");
  let state = "idle";
  if (fresh != null) state = fresh < 18 ? "fresh" : "stale";
  dot.setAttribute("data-state", state);
  $("sync-text").textContent = dev.synced ? `synced ${dev.synced} ${dev.synced_hm}` : "no data";
  $("foot-meta").textContent = `${dev.nights || 0} nights · ${(dev.total_events || 0).toLocaleString()} events`;
}

async function load() {
  let d;
  try {
    const res = await fetch("/api/summary");
    d = await res.json();
  } catch (e) {
    $("digest").classList.remove("skeleton", "skeleton-text");
    $("digest").textContent = "Could not reach the local server.";
    return;
  }
  if (d.error) {
    $("digest").classList.remove("skeleton", "skeleton-text");
    $("digest").textContent = d.error;
    return;
  }
  $("digest").classList.remove("skeleton", "skeleton-text");
  $("digest").classList.add("reveal");
  // emphasize the numeric tokens (digest comes from our own Rust, so safe)
  $("digest").innerHTML = (d.digest || "").replace(
    /([+-]?\d[\d.]*\s?(?:%|bpm|ms|m\/s))/g,
    '<span class="metric">$1</span>'
  );
  renderSync(d);
  renderTiles(d);
  renderNights(d);
  renderCardio(d);
  renderSpo2(d);
  renderActivity(d);
  renderDevice(d);
  // gentle stagger on panels
  document.querySelectorAll(".panel").forEach((p, i) => {
    p.classList.add("reveal");
    p.style.setProperty("--d", i * 60 + "ms");
  });
}

load();
