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
const icon = (name, cls = "") => `<span class="ic ${cls}" style="--i:url(/icons/${name}.svg)"></span>`;
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const cap = (s) => esc(s).replace(/^./, (c) => c.toUpperCase());
const kfmt = (n) => (n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k" : String(Math.round(n)));

let CURRENT_PROFILE = null;

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

    const comp = el("div", "breakdown");
    const seg = (l, v) => `<span>${l} <b>${num(v)}%</b></span>`;
    comp.innerHTML = seg("Deep", n.deep_pct) + seg("Light", n.light_pct) + seg("REM", n.rem_pct) + seg("Awake", n.wake_pct);
    right.append(comp);

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
  box.append(el("div", "sub", `${relAge(cv.vascular_age - cv.chronological_age).long} your age (${cv.chronological_age})`));
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
  box.append(el("div", "sub", "Calibrated from the ring's R-ratio (Oura's own curve)."));
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

const WD = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const hhmm = (min) => `${String((min / 60) | 0).padStart(2, "0")}:${String(min % 60).padStart(2, "0")}`;

// activity type → vendored hugeicons glyph (defaults to a generic activity icon)
const ACT_ICON = {
  cycling: "act-cycling", running: "act-running", walking: "act-walking", hiking: "act-walking",
  swimming: "act-swimming", strengthtraining: "act-strength", coreexercise: "act-strength",
  crosstraining: "act-strength", yoga: "act-yoga", pilates: "act-yoga",
};
const actIcon = (label) => ACT_ICON[(label || "").toLowerCase()] || "act-default";

// session detail popover (opened by clicking an actogram mark)
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
        ${kv("Time", `${hhmm(s.start)}–${hhmm(s.endTrue ?? s.end)}`)}
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

// Actogram: each day is a 24h time axis, sessions plotted as marks — you read the
// circadian rhythm at a glance. Monochrome; accent reserved for workouts.
function renderActivity(d) {
  const box = $("activity");
  box.innerHTML = "";
  const all = d.activity || [];
  if (!all.length) {
    box.append(el("div", "error", "No activity sessions detected."));
    return;
  }
  const toMin = (s) => { const [h, m] = (s || "0:0").split(":").map(Number); return (h || 0) * 60 + (m || 0); };
  const byDay = new Map();
  let minStart = 1440, maxEnd = 0;
  for (const s of all) {
    const [ymd, hm] = (s.start || "").split(" ");
    if (!ymd) continue;
    // endTrue is the real end (may cross midnight); end is clamped to the day so the
    // bar geometry stays on this day's lane. The window is sized from the clamped end.
    const start = toMin(hm), endTrue = start + (s.duration_min || 0), end = Math.min(1440, endTrue);
    minStart = Math.min(minStart, start); maxEnd = Math.max(maxEnd, end);
    if (!byDay.has(ymd)) byDay.set(ymd, []);
    byDay.get(ymd).push({ ...s, start, end, endTrue });
  }
  const days = [...byDay.keys()].sort().reverse().slice(0, 8);

  // shared whole-hour window (≥ 9h span) so days are comparable
  let h0 = Math.floor(minStart / 60), h1 = Math.ceil(maxEnd / 60);
  if (h1 - h0 < 9) { h0 = Math.max(0, h0 - Math.ceil((9 - (h1 - h0)) / 2)); h1 = Math.min(24, h0 + 9); }
  const winStart = h0 * 60, span = (h1 - h0) * 60;
  const pct = (min) => ((min - winStart) / span) * 100;
  const step = h1 - h0 <= 12 ? 2 : 3;
  const ticks = [];
  for (let h = h0; h <= h1; h += step) ticks.push(h);

  // continuous movement ridge (15-min MET buckets), shared scale across days
  const profiles = d.activity_profile || {};
  const RH = 28; // ridge SVG height units
  let maxMet = 2;
  days.forEach((y) => (profiles[y] || []).forEach((v, b) => {
    const c = b * 15 + 7.5;
    if (c >= winStart && c <= winStart + span) maxMet = Math.max(maxMet, v);
  }));
  const ridgeArea = (prof) => {
    const pts = [];
    (prof || []).forEach((v, b) => {
      const c = b * 15 + 7.5;
      if (c < winStart || c > winStart + span) return;
      pts.push([((c - winStart) / span) * 100, RH - Math.min(1, (v || 0) / maxMet) * RH]);
    });
    if (pts.length < 2) return "";
    return `${smoothPath(pts)} L${pts[pts.length - 1][0].toFixed(1)} ${RH} L${pts[0][0].toFixed(1)} ${RH} Z`;
  };

  const acto = el("div", "acto");
  const lanes = el("div", "acto-lanes");
  const grid = el("div", "acto-grid");
  ticks.forEach((h) => { const i = el("i"); i.style.left = pct(h * 60) + "%"; grid.append(i); });
  lanes.append(grid);

  const dailyStats = d.activity_daily || {};
  const dVals = days.map((y) => dailyStats[y]).filter(Boolean);
  const maxSteps = Math.max(1, ...dVals.map((v) => v.steps || 0));
  const maxKcal = Math.max(1, ...dVals.map((v) => v.active_kcal || 0));
  days.forEach((ymd) => {
    const p = ymd.split("-");
    const dt = new Date(+p[0], +p[1] - 1, +p[2]);
    const day = el("div", "acto-day");
    day.append(el("div", "acto-label", `${WD[dt.getDay()]} ${fmtDay(ymd)}`));
    const track = el("div", "acto-track");
    const area = ridgeArea(profiles[ymd]);
    track.innerHTML =
      (area ? `<svg class="acto-ridge" viewBox="0 0 100 ${RH}" preserveAspectRatio="none"><path d="${area}"/></svg>` : "") +
      `<i class="acto-base"></i>`;
    byDay.get(ymd).sort((a, b) => a.start - b.start).forEach((s) => {
      const work = s.is_workout >= 0.5;
      const bar = el("div", "acto-bar" + (work ? " workout" : ""));
      bar.style.left = pct(s.start) + "%";
      // width tracks the visible (day-clamped) segment so a past-midnight session
      // isn't drawn wider than its lane; the tooltip still reports the true duration.
      bar.style.width = Math.max(0.5, ((s.end - s.start) / span) * 100) + "%";
      bar.title = `${s.label || "activity"} · ${s.duration_min} min · ${hhmm(s.start)}–${hhmm(s.endTrue)} · tap for details`;
      // build via DOM (textContent) so the label can't inject markup; actIcon()
      // only ever returns a fixed basename, so the icon URL is safe.
      const ico = el("span", "ic");
      ico.style.setProperty("--i", `url(/icons/${actIcon(s.label)}.svg)`);
      const name = el("span", "acto-name");
      name.textContent = s.label || "";
      bar.append(ico, name);
      bar.addEventListener("click", () => openActDetail(s));
      track.append(bar);
    });
    day.append(track);

    // subtle per-day totals: steps (est.) + active calories
    const ds = dailyStats[ymd];
    const stat = el("div", "acto-stats");
    if (ds) {
      const sp = Math.max(4, (ds.steps / maxSteps) * 100);
      const kc = Math.max(4, (ds.active_kcal / maxKcal) * 100);
      const mini = (iconf, w, val, tip) =>
        `<span class="astat" title="${tip}"><span class="ic" style="--i:url(/icons/${iconf}.svg)"></span><span class="ab"><i style="width:${w}%"></i></span><b>${val}</b></span>`;
      stat.innerHTML =
        mini("act-walking", sp, kfmt(ds.steps), `${Math.round(ds.steps).toLocaleString()} estimated steps`) +
        mini("act-kcal", kc, Math.round(ds.active_kcal), `${Math.round(ds.active_kcal)} active · ${Math.round(ds.total_kcal)} total kcal`);
    }
    day.append(stat);
    lanes.append(day);
  });
  acto.append(lanes);

  const axis = el("div", "acto-axis");
  axis.append(el("div", "acto-label", ""));
  const ticksEl = el("div", "acto-ticks");
  ticks.forEach((h) => { const t = el("span", null, String(h).padStart(2, "0")); t.style.left = pct(h * 60) + "%"; ticksEl.append(t); });
  axis.append(ticksEl);
  axis.append(el("div", "acto-stats-head", "steps · kcal"));
  acto.append(axis);

  box.append(acto);
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
    const j = await (await fetch("/api/feature", {
      method: "POST",
      headers: { "X-Oura-Dash": "1", "Content-Type": "application/json" },
      body: JSON.stringify({ feature, mode: turnOn ? "automatic" : "off" }),
    })).json();
    if (j.ok) {
      toast(`${name} turned ${turnOn ? "on" : "off"}. Wear the ring; data appears on the next sync.`, "ok");
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
    const r = await fetch("/api/profile", { method: "POST", headers: { "X-Oura-Dash": "1", "Content-Type": "application/json" }, body: JSON.stringify(body) });
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
    const j = await (await fetch("/api/sync", { method: "POST", headers: { "X-Oura-Dash": "1" } })).json();
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

async function load() {
  let d;
  try {
    d = await (await fetch("/api/summary")).json();
  } catch (e) {
    showLoadError("Could not reach the local server.");
    return;
  }
  if (d.error) {
    showLoadError(d.error);
    return;
  }
  CURRENT_PROFILE = d.profile || null;
  $("digest").classList.remove("skeleton", "skeleton-text");
  $("digest").classList.add("reveal");
  $("digest").innerHTML = (d.digest || "").replace(/([+-]?\d[\d.]*\s?(?:%|bpm|ms|m\/s))/g, '<span class="metric">$1</span>');
  renderActions(d);
  renderTiles(d);
  renderNights(d);
  renderCardio(d);
  renderSpo2(d);
  renderActivity(d);
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
