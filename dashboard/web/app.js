// open_oura dashboard — fetch /api/summary (Rust computes it) and render.
//
// SIBLING CLIENT: the native iOS app (apps/ios/OuraApp/OuraApp.swift) renders the SAME
// summary JSON. A user-facing change here usually belongs there too — see the feature
// map in docs/clients-web-and-ios.md. New computed fields go in crates/oura-summary.
"use strict";

const $ = (id) => document.getElementById(id);
const el = (tag, cls, html) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
};
const num = (v, d = "—") => (v == null || Number.isNaN(v) ? d : v);
const icon = (name, cls = "") => `<span class="ic ${cls}" style="--i:url(/icons/${name}.svg)"></span>`;
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const cap = (s) => esc(s).replace(/^./, (c) => c.toUpperCase());
const kfmt = (n) => (n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k" : String(Math.round(n)));

let CURRENT_PROFILE = null;
let LAST_DEVICE_SERIAL = null;

// ── local dashboard fetch helpers ──────────────────────────────────────────
// Every mutating endpoint is gated by the X-Oura-Dash header; these centralize it
// (and the JSON POST envelope) so the call sites can't drift. They return the raw
// Response, so each caller keeps its own r.ok / body / error handling.
const DASH_HEADERS = { "X-Oura-Dash": "1" };
function postDash(url, body) {
  const opts = { method: "POST", headers: { ...DASH_HEADERS } };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  return fetch(url, opts);
}
function getDash(url) {
  return fetch(url, { headers: { ...DASH_HEADERS } });
}

// Smooth curve THROUGH the points using a monotone cubic Hermite spline
// (Fritsch–Carlson). Monotone = the curve never overshoots past a data point, so it
// won't invent peaks/valleys the data doesn't have — the right call for real metrics.
function smoothPath(pts) {
  const n = pts.length;
  if (n < 2) return "";
  const x = pts.map((p) => p[0]), y = pts.map((p) => p[1]);
  if (n === 2) return `M${x[0].toFixed(1)} ${y[0].toFixed(1)} L${x[1].toFixed(1)} ${y[1].toFixed(1)}`;
  const dx = [], dy = [], dd = []; // secant slopes
  for (let i = 0; i < n - 1; i++) { dx[i] = x[i + 1] - x[i]; dy[i] = y[i + 1] - y[i]; dd[i] = dy[i] / dx[i]; }
  const m = new Array(n);
  m[0] = dd[0];
  m[n - 1] = dd[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = dd[i - 1] * dd[i] <= 0 ? 0 : (dd[i - 1] + dd[i]) / 2;
  for (let i = 0; i < n - 1; i++) {
    if (dd[i] === 0) { m[i] = 0; m[i + 1] = 0; continue; }
    const a = m[i] / dd[i], b = m[i + 1] / dd[i], s2 = a * a + b * b;
    if (s2 > 9) { const t = 3 / Math.sqrt(s2); m[i] = t * a * dd[i]; m[i + 1] = t * b * dd[i]; }
  }
  let d = `M${x[0].toFixed(1)} ${y[0].toFixed(1)}`;
  for (let i = 0; i < n - 1; i++) {
    const h = dx[i] / 3;
    d += ` C${(x[i] + h).toFixed(1)} ${(y[i] + m[i] * h).toFixed(1)} ` +
         `${(x[i + 1] - h).toFixed(1)} ${(y[i + 1] - m[i + 1] * h).toFixed(1)} ` +
         `${x[i + 1].toFixed(1)} ${y[i + 1].toFixed(1)}`;
  }
  return d;
}

// monochrome, thin, with a faint area fill — subtle and elegant
function sparkline(series) {
  const s = (series || []).filter((x) => x != null);
  if (s.length < 2) return "";
  const w = 100, h = 26, min = Math.min(...s), max = Math.max(...s);
  const rng = max - min || 1;
  const pts = s.map((v, i) => [(i / (s.length - 1)) * w, h - ((v - min) / rng) * (h - 5) - 3]);
  const d = smoothPath(pts);
  const area = `${d} L${w.toFixed(1)} ${h} L0 ${h} Z`;
  const last = pts[pts.length - 1];
  // the SVG is stretched non-uniformly (preserveAspectRatio=none), which would
  // squash an in-SVG <circle> into an ellipse — so the end dot is a separate,
  // unstretched element positioned at the last point (vertical axis is 1:1 with px).
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
    <path d="${area}" fill="var(--spark-fill)" stroke="none"/>
    <path d="${d}" fill="none" stroke="var(--spark)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/>
  </svg><i class="spark-dot" style="top:${last[1].toFixed(1)}px"></i>`;
}

function deltaTag(pct, { good = "up" } = {}) {
  if (pct == null) return el("span", "delta flat", "");
  const flat = pct === 0;
  const cls = flat ? "flat" : (good === "up" ? pct > 0 : pct < 0) ? "up" : "down";
  return el("span", "delta " + cls, `${pct > 0 ? "+" : ""}${pct}% vs baseline`);
}

function tile(label, value, unit, deltaPct, series, opts = {}) {
  const t = el("article", "tile");
  t.append(el("div", "label", label));
  t.append(el("div", "value", `${num(value)}<span class="unit">${unit || ""}</span>`));
  if (deltaPct !== undefined) t.append(deltaTag(deltaPct, opts));
  if (series) t.append(el("div", "spark", sparkline(series)));
  return t;
}

const relAge = (diff) => {
  const a = Math.abs(Math.round(diff * 10) / 10);
  if (diff < -0.05) return { short: `${a} yr younger`, long: `${a} ${a === 1 ? "year" : "years"} younger than` };
  if (diff > 0.05) return { short: `${a} yr older`, long: `${a} ${a === 1 ? "year" : "years"} older than` };
  return { short: "in line", long: "in line with" };
};

function renderTiles(d) {
  const box = $("tiles");
  box.innerHTML = "";
  box.classList.add("reveal");
  const hv = d.vitals?.hrv || {}, rh = d.vitals?.rhr || {};
  const n0 = (d.nights || [])[0] || {};
  const effSeries = [...(d.nights || [])].reverse().map((n) => n.efficiency).filter((x) => x != null);
  box.append(tile("HRV (rmssd)", hv.latest, " ms", hv.delta_pct, hv.series, { good: "up" }));
  box.append(tile("Resting HR", rh.latest, " bpm", rh.delta_pct, rh.series, { good: "down" }));
  box.append(tile("Sleep efficiency", n0.efficiency, "%", undefined, effSeries));
  const cv = d.cardio;
  if (cv && cv.vascular_age != null) {
    const t = tile("Vascular age", cv.vascular_age, " yr");
    t.append(el("div", "sub", relAge(cv.vascular_age - cv.chronological_age).short));
    box.append(t);
  } else {
    box.append(tile("Vascular age", "—", ""));
  }
}

function hypnogram(stages) {
  const wrap = el("div", "hyp");
  (stages || []).forEach((s) => wrap.append(el("i", "s" + s)));
  return wrap;
}

// ── the unified "day" (night + activity of the same date) ──────────────────
// A day is keyed by YYYY-MM-DD. The most recent one is the hero card; the rest
// live behind "Show all N days". This mirrors the iOS app's home + AllDaysView.

// The calendar date you WOKE from a night. Nights are labelled by onset date (the
// evening you went to bed), so an overnight sleep that crosses midnight belongs to
// the next day's "morning". Pairing a day with the sleep you woke from — not the
// sleep you started that evening — is what makes "night + activity of the day" read
// as one coherent day. Kept identical to the iOS Summary.wakeYmd.
function wakeYmd(n) {
  if (!n || !n.ymd) return null;
  if (n.start && n.end && n.end < n.start) {
    const [y, m, dd] = n.ymd.split("-").map(Number);
    const t = new Date(y, m - 1, dd + 1);
    return `${t.getFullYear()}-${String(t.getMonth() + 1).padStart(2, "0")}-${String(t.getDate()).padStart(2, "0")}`;
  }
  return n.ymd;
}

// every date that has a night (by wake date), a movement profile, daily totals, or a
// session — newest first
function dayKeys(d) {
  const set = new Set();
  (d.nights || []).forEach((n) => { const w = wakeYmd(n); if (w) set.add(w); });
  Object.keys(d.activity_profile || {}).forEach((k) => set.add(k));
  Object.keys(d.activity_daily || {}).forEach((k) => set.add(k));
  (d.activity || []).forEach((s) => { const y = (s.start || "").split(" ")[0]; if (y) set.add(y); });
  return [...set].filter(Boolean).sort().reverse();
}

// the primary sleep you woke from on the morning of `ymd` — the longest in-bed night
// wins over same-morning naps. Falls back to a MM-DD match for older data lacking ymd.
function nightForDay(d, ymd) {
  const cands = (d.nights || []).filter((n) => wakeYmd(n) === ymd);
  if (cands.length) return cands.reduce((a, b) => ((b.in_bed_h || 0) > (a.in_bed_h || 0) ? b : a));
  return (d.nights || []).find((n) => n.ymd == null && (n.date || "").endsWith(ymd.slice(5))) || null;
}

// this day's sessions, start rewritten to minutes-past-midnight so openActDetail()
// (which formats s.start with hhmm()) renders them correctly.
function sessionsForDay(d, ymd) {
  return (d.activity || [])
    .filter((s) => (s.start || "").startsWith(ymd))
    .map((s) => {
      const [h, m] = ((s.start || "").split(" ")[1] || "0:0").split(":").map(Number);
      return { ...s, start: (h || 0) * 60 + (m || 0) };
    })
    .sort((a, b) => a.start - b.start);
}

const dayTitle = (ymd) => {
  const p = (ymd || "").split("-");
  if (p.length !== 3) return ymd;
  const dt = new Date(+p[0], +p[1] - 1, +p[2]);
  return `${WD[dt.getDay()]} · ${fmtDay(ymd)}`;
};

// continuous movement ridge (96 × 15-min MET buckets) as a filled SVG area
const RIDGE_H = 30;
function ridgeSvg(profile) {
  const prof = (profile || []).map((v) => v || 0);
  if (prof.length < 2) return "";
  const peak = Math.max(0.5, ...prof);
  const pts = prof.map((v, i) => [(i / (prof.length - 1)) * 100, RIDGE_H - Math.min(1, v / peak) * RIDGE_H]);
  const d = `${smoothPath(pts)} L100 ${RIDGE_H} L0 ${RIDGE_H} Z`;
  return `<svg class="day-ridge" viewBox="0 0 100 ${RIDGE_H}" preserveAspectRatio="none"><path d="${d}"/></svg>`;
}

// the combined day card: a clickable Sleep region (→ sleep detail) above a clickable
// Activity region (→ activity detail). Used as the hero on the home panel.
function dayCard(d, ymd) {
  const card = el("div", "day-card");
  card.append(el("div", "day-date", dayTitle(ymd)));

  const n = nightForDay(d, ymd);
  if (n) {
    const sp = el("button", "day-part");
    sp.type = "button";
    sp.append(el("div", "dp-head",
      `<span class="dp-tag">Sleep</span><span class="dp-meta">${esc(n.start || "—")}–${esc(n.end || "—")} · ${num(n.in_bed_h)}h</span><span class="dp-chev"></span>`));
    if (n.stages && n.stages.length) sp.append(hypnogram(n.stages));
    const comp = el("div", "breakdown");
    const seg = (l, v) => `<span>${l} <b>${num(v)}%</b></span>`;
    comp.innerHTML = seg("Deep", n.deep_pct) + seg("Light", n.light_pct) + seg("REM", n.rem_pct) + seg("Awake", n.wake_pct);
    sp.append(comp);
    sp.addEventListener("click", () => openSleepDetail(n));
    card.append(sp);
  }

  const ap = el("button", "day-part");
  ap.type = "button";
  const ds = (d.activity_daily || {})[ymd];
  const stat = ds ? `${kfmt(ds.steps)} steps · ${Math.round(ds.active_kcal)} kcal` : "no activity totals";
  ap.append(el("div", "dp-head",
    `<span class="dp-tag">Activity</span><span class="dp-meta">${stat}</span><span class="dp-chev"></span>`));
  const prof = (d.activity_profile || {})[ymd];
  if (prof && prof.length > 1) ap.insertAdjacentHTML("beforeend", ridgeSvg(prof));
  const sessions = sessionsForDay(d, ymd);
  if (sessions.length) {
    const chips = el("div", "day-sessions");
    sessions.forEach((s) => {
      const chip = el("span", "day-chip" + (s.is_workout >= 0.5 ? " workout" : ""));
      const ico = el("span", "ic");
      ico.style.setProperty("--i", `url(/icons/${actIcon(s.label)}.svg)`);
      const nm = el("span", "day-chip-name");
      nm.textContent = s.label || "activity";
      chip.append(ico, nm);
      chips.append(chip);
    });
    ap.append(chips);
  }
  ap.addEventListener("click", () => openActivityDetail(d, ymd));
  card.append(ap);
  return card;
}

function renderDay(d) {
  const box = $("day");
  box.innerHTML = "";
  const days = dayKeys(d);
  if (!days.length) {
    box.append(el("div", "error", "No days yet. Wear the ring and sync."));
    $("sleep-legend").hidden = true;
    return;
  }
  const top = days[0];
  $("sleep-legend").hidden = !(nightForDay(d, top) || {}).stages;
  box.append(dayCard(d, top));
  if (days.length > 1) {
    const btn = el("button", "more-toggle");
    btn.textContent = `Show all ${days.length} days`;
    btn.addEventListener("click", () => openDaysBrowser(d, days));
    box.append(btn);
  }
}

function renderCardio(d) {
  const box = $("cardio");
  const cv = d.cardio;
  const vo2 = d.fitness?.vo2max;
  box.innerHTML = "";
  // VO₂max is model-free (from demographics), so it shows even without the CVA model.
  const vo2Kv = vo2 != null ? el("div", "kv", `<div class="k">VO₂max estimate</div><div class="v">${vo2} ml/kg/min</div>`) : null;
  if (!cv || cv.vascular_age == null) {
    box.append(el("div", "error", "Cardiovascular age needs the cva_ppg feature on. Enable it, then sync overnight."));
    if (vo2Kv) { const kvs = el("div", "kvs"); kvs.append(vo2Kv); box.append(kvs); }
    return;
  }
  box.append(el("div", "big-metric", `<span class="n">${cv.vascular_age}</span><span class="u">years vascular age</span>`));
  box.append(el("div", "sub", `${relAge(cv.vascular_age - cv.chronological_age).long} your age (${cv.chronological_age})`));
  const kvs = el("div", "kvs");
  kvs.append(el("div", "kv", `<div class="k">Pulse-wave velocity</div><div class="v">${cv.pwv_ms != null ? cv.pwv_ms + " m/s" : "—"}</div>`));
  kvs.append(el("div", "kv", `<div class="k">Segments analysed</div><div class="v">${num(cv.segments)}</div>`));
  if (vo2Kv) kvs.append(vo2Kv);
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
  // SpO2 gauge scale: clamp the reading into [SPO2_MIN, 100] and map to 0–100% fill.
  const SPO2_MIN = 85, SPO2_HEALTHY = 95;
  box.append(el("div", "big-metric", `<span class="n">${n0.spo2_mean}</span><span class="u">% avg, last night</span>`));
  box.append(el("div", "sub", "Calibrated from the ring's R-ratio (Oura's own curve)."));
  const pct = Math.max(0, Math.min(100, ((n0.spo2_mean - SPO2_MIN) / (100 - SPO2_MIN)) * 100));
  const g = el("div", "gauge");
  const fill = el("i");
  fill.style.width = pct + "%";
  g.append(fill);
  box.append(g);
  box.append(el("div", "scale", `<span>${SPO2_MIN}</span><span>healthy ≥ ${SPO2_HEALTHY}</span><span>100</span>`));
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const fmtDay = (ymd) => {
  const p = (ymd || "").split("-");
  return p.length === 3 ? `${MONTHS[+p[1] - 1]} ${+p[2]}` : ymd;
};

const WD = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const hhmm = (min) => `${String((min / 60) | 0).padStart(2, "0")}:${String(min % 60).padStart(2, "0")}`;

// activity type → vendored hugeicons glyph (defaults to a generic activity icon)
const ACT_ICON = {
  cycling: "act-cycling", running: "act-running", walking: "act-walking", hiking: "act-walking",
  swimming: "act-swimming", strengthtraining: "act-strength", coreexercise: "act-strength",
  crosstraining: "act-strength", yoga: "act-yoga", pilates: "act-yoga",
};
const actIcon = (label) => ACT_ICON[(label || "").toLowerCase()] || "act-default";

// session detail popover (opened from a day's session row)
function openActDetail(s) {
  let dlg = $("act-dialog");
  if (!dlg) {
    dlg = el("dialog", "dialog act-dialog");
    dlg.id = "act-dialog";
    document.body.append(dlg);
    dlg.addEventListener("click", (e) => { if (e.target === dlg) dlg.close(); });
  }
  const work = s.is_workout >= 0.5;
  const conf = s.label_confidence != null ? Math.round(s.label_confidence * 100) + "%" : "—";
  const kv = (k, v) => `<div class="kv"><div class="k">${k}</div><div class="v">${v}</div></div>`;
  const top3 = (s.top3 || [])
    .map(([n, p]) => `<div class="t3"><span class="t3n">${cap(n)}</span><span class="t3bar"><i style="width:${Math.round(p * 100)}%"></i></span><span class="t3p">${Math.round(p * 100)}%</span></div>`)
    .join("");
  dlg.innerHTML =
    `<form method="dialog">
      <div class="ad-head">
        <span class="ic" style="--i:url(/icons/${actIcon(s.label)}.svg)"></span>
        <h3>${cap(s.label || "activity")}</h3>
        ${work ? '<span class="ad-tag">workout</span>' : ""}
      </div>
      <div class="ad-grid">
        ${kv("Time", `${hhmm(s.start)}–${s.end}`)}
        ${kv("Duration", `${s.duration_min} min`)}
        ${s.active_kcal != null ? kv("Active calories", `${Math.round(s.active_kcal).toLocaleString()} kcal`) : ""}
        ${kv("Confidence", conf)}
        ${kv("Workout", work ? "yes" : "no")}
      </div>
      <p class="subhead">Model's guesses</p>
      <div class="t3list">${top3 || '<div class="ad-muted">no alternates</div>'}</div>
      <p class="ad-foot">Oura automatic_activity_detection — best guess from MET / motion / HR / temp.</p>
      <div class="dialog-actions"><button class="btn-primary">Close</button></div>
    </form>`;
  dlg.showModal();
}

// ── day-detail dialogs (sleep / activity / combined) ───────────────────────

// the night's hypnogram + stage breakdown + that night's vitals, as a detachable
// block shared by the sleep-only and the combined day detail.
function sleepDetailBody(n) {
  const wrap = el("div", "dd-section");
  wrap.append(el("div", "dd-sub", `${esc(n.start || "—")}–${esc(n.end || "—")} · ${num(n.in_bed_h)} h in bed`));
  if (n.stages && n.stages.length) {
    wrap.append(hypnogram(n.stages));
    const comp = el("div", "breakdown");
    const seg = (l, v) => `<span>${l} <b>${num(v)}%</b></span>`;
    comp.innerHTML = seg("Deep", n.deep_pct) + seg("Light", n.light_pct) + seg("REM", n.rem_pct) + seg("Awake", n.wake_pct);
    wrap.append(comp);
  } else {
    wrap.append(el("div", "ad-muted", "No hypnogram for this night yet — the SleepNet model runs on sync."));
  }
  const grid = el("div", "ad-grid");
  const kv = (k, v) => `<div class="kv"><div class="k">${k}</div><div class="v">${v}</div></div>`;
  grid.innerHTML =
    kv("Efficiency", n.efficiency != null ? n.efficiency + "%" : "—") +
    kv("HRV", n.hrv_ms != null ? n.hrv_ms + " ms" : "—") +
    kv("Resting HR", n.rhr != null ? n.rhr + " bpm" : "—") +
    kv("Skin temp", n.skin_temp != null ? n.skin_temp.toFixed(1) + " °C" : "—") +
    kv("Blood O₂", n.spo2_mean != null ? n.spo2_mean + "%" : "—");
  wrap.append(grid);
  return wrap;
}

// the day's movement ridge + step/kcal totals + a clickable session list (each row
// opens the per-session model detail). Shared by the activity-only + combined detail.
function activityDetailBody(d, ymd) {
  const wrap = el("div", "dd-section");
  const prof = (d.activity_profile || {})[ymd];
  if (prof && prof.length > 1) wrap.insertAdjacentHTML("beforeend", ridgeSvg(prof));
  const ds = (d.activity_daily || {})[ymd];
  if (ds) {
    const grid = el("div", "ad-grid");
    const kv = (k, v) => `<div class="kv"><div class="k">${k}</div><div class="v">${v}</div></div>`;
    grid.innerHTML =
      kv("Steps", Math.round(ds.steps || 0).toLocaleString()) +
      (ds.distance_m != null ? kv("Distance", (ds.distance_m / 1000).toFixed(1) + " km") : "") +
      kv("Active energy", Math.round(ds.active_kcal || 0) + " kcal") +
      kv("Total energy", Math.round(ds.total_kcal || 0) + " kcal");
    wrap.append(grid);
  }
  const sessions = sessionsForDay(d, ymd);
  if (sessions.length) {
    wrap.append(el("p", "subhead", "Sessions"));
    const list = el("div", "dd-sessions");
    sessions.forEach((s) => {
      const row = el("button", "dd-session" + (s.is_workout >= 0.5 ? " workout" : ""));
      row.type = "button";
      const ico = el("span", "ic");
      ico.style.setProperty("--i", `url(/icons/${actIcon(s.label)}.svg)`);
      const nm = el("span", "dd-s-name");
      nm.textContent = s.label || "activity";
      const meta = el("span", "dd-s-meta");
      meta.textContent = `${s.duration_min} min · ${hhmm(s.start)}`;
      row.append(ico, nm, meta);
      row.addEventListener("click", () => openActDetail(s));
      list.append(row);
    });
    wrap.append(list);
  } else {
    wrap.append(el("div", "ad-muted", "No sessions detected this day."));
  }
  return wrap;
}

// a reusable detail dialog (id-scoped so the days browser can stack on top of a day
// detail without a showModal() clash). `bodies` are pre-built DOM nodes.
function openDetail(id, title, bodies) {
  let dlg = $(id);
  if (!dlg) {
    dlg = el("dialog", "dialog day-dialog");
    dlg.id = id;
    dlg.addEventListener("click", (e) => { if (e.target === dlg) dlg.close(); });
    document.body.append(dlg);
  }
  const form = el("form");
  form.method = "dialog";
  const head = el("div", "dd-head");
  const h = el("h3");
  h.textContent = title;
  const close = el("button", "dd-close", "✕");
  close.type = "button";
  close.setAttribute("aria-label", "Close");
  close.addEventListener("click", () => dlg.close());
  head.append(h, close);
  form.append(head, ...bodies.filter(Boolean));
  dlg.replaceChildren(form);
  dlg.showModal();
}

function openSleepDetail(n) {
  const w = wakeYmd(n);
  openDetail("day-dialog", `Sleep · ${w ? dayTitle(w) : (n.date || "")}`, [sleepDetailBody(n)]);
}
function openActivityDetail(d, ymd) {
  openDetail("day-dialog", `Activity · ${dayTitle(ymd)}`, [activityDetailBody(d, ymd)]);
}
function openDayDetail(d, ymd) {
  const n = nightForDay(d, ymd);
  const bodies = [];
  if (n) bodies.push(el("p", "subhead", "Sleep"), sleepDetailBody(n));
  bodies.push(el("p", "subhead", "Activity"), activityDetailBody(d, ymd));
  openDetail("day-dialog", dayTitle(ymd), bodies);
}

// the "previous days" page: every day as a row (date, mini-hypnogram, totals) that
// opens its combined night+activity detail. Uses its own dialog id so opening a
// day's detail stacks on top instead of clobbering this list.
function openDaysBrowser(d, days) {
  let dlg = $("days-dialog");
  if (!dlg) {
    dlg = el("dialog", "dialog day-dialog");
    dlg.id = "days-dialog";
    dlg.addEventListener("click", (e) => { if (e.target === dlg) dlg.close(); });
    document.body.append(dlg);
  }
  const form = el("form");
  form.method = "dialog";
  const head = el("div", "dd-head");
  const h = el("h3");
  h.textContent = `All ${days.length} days`;
  const close = el("button", "dd-close", "✕");
  close.type = "button";
  close.setAttribute("aria-label", "Close");
  close.addEventListener("click", () => dlg.close());
  head.append(h, close);
  form.append(head);

  const list = el("div", "daylist");
  days.forEach((ymd) => {
    const n = nightForDay(d, ymd);
    const ds = (d.activity_daily || {})[ymd];
    const row = el("button", "daylist-row");
    row.type = "button";
    const left = el("div", "dl-left");
    left.append(el("div", "dl-date", dayTitle(ymd)));
    left.append(el("div", "dl-sub", ds ? `${kfmt(ds.steps)} steps · ${Math.round(ds.active_kcal)} kcal` : (n ? "sleep only" : "—")));
    row.append(left);
    if (n && n.stages && n.stages.length) {
      const hyp = hypnogram(n.stages);
      hyp.classList.add("dl-hyp");
      row.append(hyp);
    }
    row.append(el("span", "dp-chev"));
    row.addEventListener("click", () => openDayDetail(d, ymd));
    list.append(row);
  });
  form.append(list);
  dlg.replaceChildren(form);
  dlg.showModal();
}

// capability → glyph (mix of vendored phosphor + hugeicons)
const CAP_ICON = {
  "Daytime HR": "heartbeat", "SpO2": "wind", "Exercise HR": "person-simple-run",
  "Real steps": "act-walking", "Cardio PPG (CVA)": "heartbeat",
};
const capIcon = (name) => CAP_ICON[name] || "cpu";

async function doFeature(feature, name, currentOn, row) {
  if (row.classList.contains("busy")) return;
  const turnOn = !currentOn;
  row.classList.add("busy");
  row.classList.toggle("on", turnOn); // optimistic
  try {
    const j = await (await postDash("/api/feature", { feature, mode: turnOn ? "automatic" : "off" })).json();
    if (j.ok) {
      toast(`${name} turned ${turnOn ? "on" : "off"}. Wear the ring; data appears on the next sync.`, "ok");
      load(); // refresh dev.measuring so a second tap toggles from the real state
    } else {
      toast(syncHint(j.message), "error");
      row.classList.toggle("on", currentOn); // revert
    }
  } catch (e) {
    toast("Couldn't reach the local server.", "error");
    row.classList.toggle("on", currentOn);
  }
  row.classList.remove("busy");
}

function renderDevice(d) {
  const box = $("device");
  const dev = d.device || {};
  box.innerHTML = "";

  const stats = el("div", "dh-stats");
  const stat = (k, v, u) => el("div", "dh-stat", `<div class="k">${k}</div><div class="v">${v}<span class="u">${u || ""}</span></div>`);
  const bpct = dev.battery_pct;
  const bstat = stat("Battery", bpct != null ? bpct : "—", "%");
  if (bpct != null && bpct < 20) bstat.classList.add("low");
  stats.append(bstat);
  const fresh = dev.fresh_hours != null ? (dev.fresh_hours < 1 ? "<1" : Math.round(dev.fresh_hours)) : "—";
  stats.append(stat("Last sync", fresh, " h ago"));
  stats.append(stat("History", num(dev.days_of_data), " days"));
  stats.append(stat("Events", (dev.total_events || 0).toLocaleString()));
  box.append(stats);

  // left = data streams (what the ring is recording)
  const left = el("div");
  const streams = dev.streams || [];
  if (streams.length) {
    left.append(el("p", "subhead", "Data captured"));
    const max = Math.max(...streams.map((s) => s.count), 1);
    const sc = el("div", "streams");
    streams.forEach((s) => {
      const row = el("div", "stream");
      const nm = el("span", "s-name");
      nm.textContent = s.name;
      const bar = el("span", "s-bar");
      const fill = el("i");
      fill.style.width = Math.max(3, (s.count / max) * 100) + "%";
      bar.append(fill);
      const val = el("span", "s-val");
      val.textContent = s.count.toLocaleString();
      row.append(nm, bar, val);
      sc.append(row);
    });
    left.append(sc);
  }
  box.append(left);

  // right = insights
  const right = el("div");
  right.append(el("p", "subhead", "Insights available"));
  const ins = el("div", "insights");
  (dev.insights || []).forEach((i) => {
    const row = el("div", "insight");
    row.append(el("div", null, `${i.name}${i.status === "gated" && i.why ? `<span class="why"> · ${i.why}</span>` : ""}`));
    const st = el("span", "status " + i.status);
    st.innerHTML = `<i></i>${i.status}`;
    row.append(st);
    ins.append(row);
  });
  right.append(ins);
  box.append(right);

  // ── advanced / debugging (collapsed by default) ──────────────────────────
  const adv = el("details", "dh-advanced");
  const sum = el("summary");
  sum.innerHTML = `<span class="ic" style="--i:url(/icons/cpu.svg)"></span>Advanced &amp; debugging<span class="chev"></span>`;
  adv.append(sum);
  const ab = el("div", "adv-body");

  // device identity + sync internals
  ab.append(el("p", "subhead", "Device"));
  const kv = el("div", "adv-kv");
  const kvItem = (k, v) => `<div><i>${k}</i><b>${v}</b></div>`;
  kv.innerHTML =
    kvItem("Ring ID", esc(dev.serial || "—")) +
    kvItem("Firmware", esc(dev.firmware || "—")) +
    kvItem("API", esc(dev.api_version || "—")) +
    kvItem("MAC", esc(dev.mac || "—")) +
    kvItem("Hardware", esc(dev.hardware_id || "—")) +
    kvItem("Battery", dev.battery_v != null ? dev.battery_v + " V" : "—") +
    kvItem("Last sync", `${esc(dev.synced || "—")} ${esc(dev.synced_hm || "")}`) +
    kvItem("Sync cursor", dev.next_cursor != null ? dev.next_cursor.toLocaleString() : "—") +
    kvItem("History", `${num(dev.days_of_data)} days`);
  ab.append(kv);

  // local auth key portability
  ab.append(el("p", "subhead", "Ring auth key"));
  const keyTools = el("div", "key-tools");
  const exportBtn = el("button", "btn-text key-btn", "Export / QR");
  exportBtn.type = "button";
  exportBtn.title = "Show copy, download, and QR options for the local ring auth key.";
  exportBtn.addEventListener("click", exportRingKey);
  const importBtn = el("button", "btn-text key-btn", "Import / scan");
  importBtn.type = "button";
  importBtn.title = "Paste, upload, or scan a ring auth key.";
  importBtn.addEventListener("click", openImportKeyDialog);
  keyTools.append(exportBtn, importBtn);
  ab.append(keyTools);

  // capability toggles
  ab.append(el("p", "subhead", "Capabilities · tap to toggle"));
  const caps = el("div", "caps");
  (dev.measuring || []).forEach((m) => {
    const row = el("div", "cap" + (m.on ? " on" : ""));
    const ic = el("span", "ic");
    ic.style.setProperty("--i", `url(/icons/${capIcon(m.name)}.svg)`);
    const nm = el("span", "cap-name");
    nm.textContent = m.name;
    const sw = el("span", "switch", "<i></i>");
    row.append(ic, nm, sw);
    if (m.feature) {
      row.classList.add("interactive");
      row.title = `Tap to turn ${m.on ? "off" : "on"} (connects to the ring)`;
      row.addEventListener("click", () => doFeature(m.feature, m.name, m.on, row));
    }
    caps.append(row);
  });
  ab.append(caps);

  const ev = dev.event_counts || [];
  if (ev.length) {
    ab.append(el("p", "subhead", `Event stream · ${ev.length} types`));
    const emax = Math.max(...ev.map((e) => e.count), 1);
    const tbl = el("div", "ev-table");
    ev.forEach((e) => {
      const row = el("div", "ev-row");
      const nm = el("span", "ev-n");
      nm.textContent = e.name;
      const bar = el("span", "ev-bar");
      const fi = el("i");
      fi.style.width = Math.max(2, (e.count / emax) * 100) + "%";
      bar.append(fi);
      const c = el("span", "ev-c");
      c.textContent = e.count.toLocaleString();
      row.append(nm, bar, c);
      tbl.append(row);
    });
    ab.append(tbl);
  }
  adv.append(ab);
  box.append(adv);
}

function ringKeyFilename() {
  const serial = (LAST_DEVICE_SERIAL || "oura-ring").replace(/[^A-Za-z0-9_.-]+/g, "-");
  return `${serial}.key`;
}

async function exportRingKey() {
  try {
    const j = await fetchRingKey();
    if (!j) return;
    if (!j.ok) {
      toast(j.message || "No key file is configured. Start the dashboard with --key-file.", "error");
      return;
    }
    openExportKeyDialog(j.key);
  } catch {
    toast("Couldn't export the ring auth key.", "error");
  }
}

async function fetchRingKey() {
  const r = await getDash("/api/ring-key");
  if (!r.ok) {
    toast("Restart the dashboard server to enable key export.", "error");
    return null;
  }
  return await r.json();
}

async function copyText(text, ok = "Copied.") {
  try {
    await navigator.clipboard.writeText(text);
    toast(ok, "ok");
  } catch {
    toast("Couldn't write to the clipboard.", "error");
  }
}

function downloadRingKey(key) {
  const blob = new Blob([key + "\n"], { type: "text/plain" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = ringKeyFilename();
  document.body.append(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(a.href);
  toast("Ring auth key downloaded.", "ok");
}

function openExportKeyDialog(key) {
  let dlg = $("key-export-dialog");
  if (!dlg) {
    dlg = el("dialog", "dialog key-dialog");
    dlg.id = "key-export-dialog";
    dlg.addEventListener("click", (e) => { if (e.target === dlg) dlg.close(); });
    document.body.append(dlg);
  }
  dlg.innerHTML = `
    <form method="dialog">
      <h3>Ring auth key</h3>
      <p class="dialog-sub">Use this on another computer running open_oura with the same ring.</p>
      <div class="qr-wrap"><canvas id="key-qr" width="232" height="232" aria-label="Ring auth key QR code"></canvas></div>
      <input id="key-export-value" class="key-field" readonly value="${esc(key)}" />
      <div class="dialog-actions key-actions">
        <button type="button" id="key-copy" class="btn-primary">Copy</button>
        <button type="button" id="key-download" class="btn-text">Download</button>
        <button class="btn-text">Close</button>
      </div>
    </form>`;
  dlg.querySelector("#key-copy").addEventListener("click", () => copyText(key, "Ring auth key copied."));
  dlg.querySelector("#key-download").addEventListener("click", () => downloadRingKey(key));
  dlg.showModal();
  drawQr($("key-qr"), key.toUpperCase());
}

function openImportKeyDialog() {
  let dlg = $("key-import-dialog");
  if (!dlg) {
    dlg = el("dialog", "dialog key-dialog");
    dlg.id = "key-import-dialog";
    dlg.addEventListener("click", (e) => { if (e.target === dlg) closeImportKeyDialog(); });
    document.body.append(dlg);
  }
  const canScan = "BarcodeDetector" in window && navigator.mediaDevices && navigator.mediaDevices.getUserMedia;
  dlg.innerHTML = `
    <form method="dialog">
      <h3>Import ring key</h3>
      <p class="dialog-sub">Paste a 32-character hex key, upload a .key file, or scan the export QR code.</p>
      <textarea id="key-import-value" class="key-field key-textarea" spellcheck="false" autocomplete="off" placeholder="32 hex characters"></textarea>
      <video id="key-scan-video" class="key-video" playsinline muted hidden></video>
      <div class="dialog-actions key-actions">
        <button type="button" id="key-import-save" class="btn-primary">Import</button>
        <button type="button" id="key-import-file" class="btn-text">File</button>
        <button type="button" id="key-import-scan" class="btn-text"${canScan ? "" : " disabled"}>Scan</button>
        <button type="button" id="key-import-close" class="btn-text">Close</button>
      </div>
      <input id="key-import-file-input" class="key-input" type="file" accept=".key,text/plain" />
    </form>`;
  dlg.querySelector("#key-import-save").addEventListener("click", () => importRingKeyText($("key-import-value").value));
  dlg.querySelector("#key-import-file").addEventListener("click", () => $("key-import-file-input").click());
  dlg.querySelector("#key-import-file-input").addEventListener("change", (e) => importRingKeyFile(e.target));
  dlg.querySelector("#key-import-scan").addEventListener("click", startKeyScan);
  dlg.querySelector("#key-import-close").addEventListener("click", closeImportKeyDialog);
  dlg.showModal();
}

function closeImportKeyDialog() {
  stopKeyScan();
  const dlg = $("key-import-dialog");
  if (dlg) dlg.close();
}

let KEY_SCAN_STREAM = null;
let KEY_SCAN_STOP = false;

async function startKeyScan() {
  try {
    const video = $("key-scan-video");
    const detector = new BarcodeDetector({ formats: ["qr_code"] });
    KEY_SCAN_STOP = false;
    KEY_SCAN_STREAM = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" } });
    video.srcObject = KEY_SCAN_STREAM;
    video.hidden = false;
    await video.play();
    const scan = async () => {
      if (KEY_SCAN_STOP) return;
      const codes = await detector.detect(video).catch(() => []);
      const raw = codes[0] && codes[0].rawValue;
      if (raw) {
        $("key-import-value").value = raw.trim();
        stopKeyScan();
        toast("QR code scanned.", "ok");
        return;
      }
      requestAnimationFrame(scan);
    };
    scan();
  } catch {
    toast("Camera QR scan is not available in this browser.", "error");
    stopKeyScan();
  }
}

function stopKeyScan() {
  KEY_SCAN_STOP = true;
  if (KEY_SCAN_STREAM) KEY_SCAN_STREAM.getTracks().forEach((t) => t.stop());
  KEY_SCAN_STREAM = null;
  const video = $("key-scan-video");
  if (video) {
    video.pause();
    video.srcObject = null;
    video.hidden = true;
  }
}

async function importRingKeyFile(input) {
  const file = input.files && input.files[0];
  input.value = "";
  if (!file) return;
  try {
    await importRingKeyText(await file.text());
  } catch {
    toast("Couldn't read that key file.", "error");
  }
}

async function importRingKeyText(text) {
  const key = (text || "").trim();
  if (!/^[0-9a-fA-F]{32}$/.test(key)) {
    toast("Auth key must be exactly 32 hex characters.", "error");
    return;
  }
  try {
    const j = await (await postDash("/api/ring-key", { key })).json();
    if (j.ok) {
      closeImportKeyDialog();
      toast("Ring auth key imported.", "ok");
    }
    else toast(j.message || "Couldn't import the ring auth key.", "error");
  } catch {
    toast("Couldn't reach the local dashboard server.", "error");
  }
}

// Fixed QR Code version 2-L generator, enough for this 32-char hex key.
function drawQr(canvas, text) {
  const n = 25, modules = Array.from({ length: n }, () => Array(n).fill(false));
  const reserved = Array.from({ length: n }, () => Array(n).fill(false));
  const set = (x, y, v, r = true) => { if (x >= 0 && y >= 0 && x < n && y < n) { modules[y][x] = v; if (r) reserved[y][x] = true; } };
  const finder = (x, y) => {
    for (let dy = -1; dy <= 7; dy++) for (let dx = -1; dx <= 7; dx++) {
      const xx = x + dx, yy = y + dy;
      const on = dx >= 0 && dy >= 0 && dx <= 6 && dy <= 6 && (dx === 0 || dy === 0 || dx === 6 || dy === 6 || (dx >= 2 && dx <= 4 && dy >= 2 && dy <= 4));
      set(xx, yy, on);
    }
  };
  finder(0, 0); finder(n - 7, 0); finder(0, n - 7);
  for (let i = 8; i < n - 8; i++) { set(i, 6, i % 2 === 0); set(6, i, i % 2 === 0); }
  for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) set(18 + dx, 18 + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
  set(8, n - 8, true);
  reserveFormatAreas(reserved);
  const data = qrDataCodewords(text), ecc = qrRs(data, 10), bits = [];
  data.concat(ecc).forEach((b) => { for (let i = 7; i >= 0; i--) bits.push(((b >>> i) & 1) === 1); });
  let k = 0, up = true;
  for (let x = n - 1; x > 0; x -= 2) {
    if (x === 6) x--;
    for (let yy = 0; yy < n; yy++) {
      const y = up ? n - 1 - yy : yy;
      for (let dx = 0; dx < 2; dx++) {
        const xx = x - dx;
        if (reserved[y][xx]) continue;
        let bit = bits[k++] || false;
        if ((xx + y) % 2 === 0) bit = !bit;
        set(xx, y, bit, false);
      }
    }
    up = !up;
  }
  placeFormat(modules, reserved, 1, 0);
  const ctx = canvas.getContext("2d"), scale = Math.floor(canvas.width / (n + 8)), off = Math.floor((canvas.width - n * scale) / 2);
  ctx.fillStyle = "#fff"; ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#111";
  for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) if (modules[y][x]) ctx.fillRect(off + x * scale, off + y * scale, scale, scale);
}

function reserveFormatAreas(reserved) {
  const n = reserved.length;
  for (let i = 0; i <= 5; i++) reserved[i][8] = true;
  reserved[7][8] = true; reserved[8][8] = true; reserved[8][7] = true;
  for (let i = 9; i < 15; i++) reserved[8][14 - i] = true;
  for (let i = 0; i < 8; i++) reserved[8][n - 1 - i] = true;
  for (let i = 8; i < 15; i++) reserved[n - 1 - (14 - i)][8] = true;
}

function qrDataCodewords(text) {
  const alpha = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";
  const bits = [];
  const push = (v, n) => { for (let i = n - 1; i >= 0; i--) bits.push((v >>> i) & 1); };
  push(2, 4); push(text.length, 9);
  for (let i = 0; i < text.length; i += 2) {
    const a = alpha.indexOf(text[i]), b = alpha.indexOf(text[i + 1]);
    if (b >= 0) push(a * 45 + b, 11); else push(a, 6);
  }
  push(0, Math.min(4, 272 - bits.length));
  while (bits.length % 8) bits.push(0);
  const out = [];
  for (let i = 0; i < bits.length; i += 8) out.push(bits.slice(i, i + 8).reduce((a, b) => (a << 1) | b, 0));
  for (let p = 0; out.length < 34; p++) out.push(p % 2 ? 0x11 : 0xec);
  return out;
}

function qrRs(data, count) {
  const mul = (x, y) => { let z = 0; for (; y; y >>>= 1) { if (y & 1) z ^= x; x = (x << 1) ^ (x & 0x80 ? 0x11d : 0); } return z & 255; };
  let gen = [1];
  for (let i = 0, root = 1; i < count; i++, root = mul(root, 2)) {
    const next = Array(gen.length + 1).fill(0);
    gen.forEach((c, j) => { next[j] ^= mul(c, root); next[j + 1] ^= c; });
    gen = next;
  }
  const rem = Array(count).fill(0);
  data.forEach((b) => {
    const factor = b ^ rem.shift();
    rem.push(0);
    gen.slice(0, count).forEach((c, i) => { rem[i] ^= mul(c, factor); });
  });
  return rem;
}

function placeFormat(modules, reserved, ecl, mask) {
  let data = (ecl << 3) | mask, rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  const bits = ((data << 10) | rem) ^ 0x5412;
  const set = (x, y, i) => { modules[y][x] = ((bits >>> i) & 1) === 1; reserved[y][x] = true; };
  for (let i = 0; i <= 5; i++) set(8, i, i);
  set(8, 7, 6); set(8, 8, 7); set(7, 8, 8);
  for (let i = 9; i < 15; i++) set(14 - i, 8, i);
  for (let i = 0; i < 8; i++) set(24 - i, 8, i);
  for (let i = 8; i < 15; i++) set(8, 24 - (14 - i), i);
}

function renderActions(d) {
  const dev = d.device || {};
  const b = $("batt");
  if (dev.battery_pct != null) {
    b.innerHTML = icon("battery-high") + `<span>${dev.battery_pct}%</span>`;
    b.classList.toggle("low", dev.battery_pct < 20);
    b.hidden = false;
    b.title = `Ring battery ${dev.battery_pct}%${dev.battery_v ? " · " + dev.battery_v + " V" : ""}`;
  } else {
    b.hidden = true;
  }
  $("foot-meta").textContent = `${dev.nights || 0} nights · ${(dev.total_events || 0).toLocaleString()} events`;
}

// ── profile dialog ──────────────────────────────────────────
function openProfile() {
  const p = CURRENT_PROFILE || {};
  $("f-sex").value = p.sex || "M";
  $("f-age").value = p.age ?? 30;
  $("f-height").value = p.height_m ?? 1.78;
  $("f-weight").value = p.weight_kg ?? 75;
  $("profile-dialog").showModal();
}
async function saveProfile(e) {
  e.preventDefault();
  const body = {
    sex: $("f-sex").value,
    age: +$("f-age").value,
    height_m: +$("f-height").value,
    weight_kg: +$("f-weight").value,
    ring_size: (CURRENT_PROFILE && CURRENT_PROFILE.ring_size) || 10, // not on the ring; kept default
  };
  $("profile-save").disabled = true;
  try {
    const r = await postDash("/api/profile", body);
    const j = await r.json().catch(() => ({}));
    // the server replies 200 with an { error } body on write failures — surface it
    // and keep the dialog open instead of pretending the save succeeded.
    if (!r.ok || j.error) {
      toast(j.error || "Couldn't save profile.", "error");
      return;
    }
    $("profile-dialog").close();
    await load(); // re-runs CVA with the new demographics
  } catch {
    toast("Couldn't reach the local server.", "error");
  } finally {
    $("profile-save").disabled = false;
  }
}

// ── sync ────────────────────────────────────────────────────
function toast(msg, kind = "info") {
  let t = $("toast");
  if (!t) { t = el("div", "toast"); t.id = "toast"; document.body.append(t); }
  t.className = "toast " + kind;
  // status dot + message (textContent on the span keeps the message injection-safe)
  const dot = el("span", "toast-dot");
  const text = el("span", "toast-msg");
  text.textContent = msg;
  t.replaceChildren(dot, text);
  requestAnimationFrame(() => t.classList.add("show"));
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.remove("show"), kind === "error" ? 8000 : 3800);
}

// turn a backend sync error into something actionable
function syncHint(msg) {
  msg = msg || "";
  if (/no matching|not found|no device|no ring/i.test(msg))
    return "Couldn't find your ring. Take it off the charger, keep it nearby, and try again.";
  if (/key|auth|unauthor/i.test(msg))
    return "The ring needs its auth key. Start the dashboard with --key-file.";
  if (/timed out|timeout/i.test(msg))
    return "Bluetooth timed out. Make sure the ring is awake and close, then retry.";
  return "Sync failed: " + msg;
}

async function doSync() {
  const btn = $("sync-btn");
  if (btn.classList.contains("syncing")) return;
  btn.classList.add("syncing");
  $("sync-label").textContent = "Syncing";
  btn.title = "Connecting to the ring over Bluetooth…";
  try {
    const j = await (await postDash("/api/sync")).json();
    if (j.ok) {
      $("sync-label").textContent = "Synced";
      toast(j.message && !/^synced$/i.test(j.message) ? j.message : "Ring synced.", "ok");
      await load();
    } else {
      $("sync-label").textContent = "Failed";
      toast(syncHint(j.message), "error");
    }
  } catch (e) {
    $("sync-label").textContent = "Failed";
    toast("Couldn't reach the local dashboard server.", "error");
  }
  btn.classList.remove("syncing");
  setTimeout(() => { $("sync-label").textContent = "Sync"; btn.title = "Sync the ring over Bluetooth"; }, 3000);
}

// ── load ────────────────────────────────────────────────────
// show the error in the headline and stop every panel's loading shimmer, so the
// page reads as "errored" rather than stuck mid-load.
function showLoadError(msg) {
  document.querySelectorAll(".skeleton").forEach((el) => {
    if (el.id === "digest") return; // handled below — keep it for the message
    el.remove();
  });
  const dg = $("digest");
  dg.classList.remove("skeleton", "skeleton-text");
  dg.classList.add("reveal");
  dg.textContent = msg;
}

let LOAD_SEQ = 0;
async function load() {
  // guard against overlapping loads (sync/profile-save during an in-flight build):
  // a slower earlier response must not overwrite a newer one.
  const seq = ++LOAD_SEQ;
  let d;
  try {
    d = await (await fetch("/api/summary")).json();
  } catch (e) {
    if (seq === LOAD_SEQ) showLoadError("Could not reach the local server.");
    return;
  }
  if (seq !== LOAD_SEQ) return; // a newer load() superseded this response — drop it
  if (d.error) {
    showLoadError(d.error);
    return;
  }
  CURRENT_PROFILE = d.profile || null;
  LAST_DEVICE_SERIAL = d.device && d.device.serial;
  $("digest").classList.remove("skeleton", "skeleton-text");
  $("digest").classList.add("reveal");
  $("digest").innerHTML = (d.digest || "").replace(/([+-]?\d[\d.]*\s?(?:%|bpm|ms|m\/s))/g, '<span class="metric">$1</span>');
  renderActions(d);
  renderTiles(d);
  renderDay(d);
  renderCardio(d);
  renderSpo2(d);
  renderDevice(d);
  document.querySelectorAll(".panel").forEach((p, i) => {
    p.classList.add("reveal");
    p.style.setProperty("--d", i * 60 + "ms");
  });
}

$("sync-btn").addEventListener("click", doSync);
$("profile-btn").addEventListener("click", openProfile);
$("profile-form").addEventListener("submit", saveProfile);
$("profile-cancel").addEventListener("click", () => $("profile-dialog").close());
load();
