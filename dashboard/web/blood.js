// open_oura blood panel — fetches /api/blood/report (Rust computes status/trend/flags
// from a mocked-but-real panel) and renders it: reference-range trends per marker, the
// markers worth attention, and a per-marker detail chart. Web-only; see docs.
"use strict";

const $ = (id) => document.getElementById(id);
const SVGNS = "http://www.w3.org/2000/svg";

// Safe hyperscript (same idiom as dna.js). String/number children → text nodes.
function h(tag, attrs, ...kids) {
  const n = document.createElement(tag);
  applyAttrs(n, attrs);
  appendKids(n, kids);
  return n;
}
// SVG variant (namespaced element).
function s(tag, attrs, ...kids) {
  const n = document.createElementNS(SVGNS, tag);
  applyAttrs(n, attrs, true);
  appendKids(n, kids);
  return n;
}
function applyAttrs(n, attrs, svg) {
  if (!attrs) return;
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") { if (svg) n.setAttribute("class", v); else n.className = v; }
    else if (k.startsWith("on") && typeof v === "function") n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v);
  }
}
function appendKids(n, kids) {
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    n.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
}
const clear = (node) => { node.replaceChildren(); return node; };

const KIND = { watch: "warn", good: "ok", neutral: "neutral" };
const pill = (label, kind) => h("span", { class: `pill ${kind}` }, h("i"), label);
const signed = (n) => (n >= 0 ? "+" : "") + n;
const fmtDate = (iso) => { const [y, m, d] = iso.split("-"); return `${d}/${m}/${y.slice(2)}`; };
const fmtSize = (b) => (b >= 1e6 ? (b / 1e6).toFixed(1) + " MB" : (b / 1e3).toFixed(0) + " KB");

// ── monotone-cubic path (ported from the main dashboard's app.js) ─────────────
function smoothPath(pts) {
  const n = pts.length;
  if (n < 2) return "";
  const x = pts.map((p) => p[0]), y = pts.map((p) => p[1]);
  if (n === 2) return `M${x[0].toFixed(2)} ${y[0].toFixed(2)} L${x[1].toFixed(2)} ${y[1].toFixed(2)}`;
  const dx = [], dy = [], dd = [];
  for (let i = 0; i < n - 1; i++) { dx[i] = x[i + 1] - x[i]; dy[i] = y[i + 1] - y[i]; dd[i] = dy[i] / dx[i]; }
  const m = new Array(n);
  m[0] = dd[0]; m[n - 1] = dd[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = dd[i - 1] * dd[i] <= 0 ? 0 : (dd[i - 1] + dd[i]) / 2;
  for (let i = 0; i < n - 1; i++) {
    if (dd[i] === 0) { m[i] = 0; m[i + 1] = 0; continue; }
    const a = m[i] / dd[i], b = m[i + 1] / dd[i], s2 = a * a + b * b;
    if (s2 > 9) { const t = 3 / Math.sqrt(s2); m[i] = t * a * dd[i]; m[i + 1] = t * b * dd[i]; }
  }
  let d = `M${x[0].toFixed(2)} ${y[0].toFixed(2)}`;
  for (let i = 0; i < n - 1; i++) {
    const hh = dx[i] / 3;
    d += ` C${(x[i] + hh).toFixed(2)} ${(y[i] + m[i] * hh).toFixed(2)} ` +
         `${(x[i + 1] - hh).toFixed(2)} ${(y[i + 1] - m[i + 1] * hh).toFixed(2)} ` +
         `${x[i + 1].toFixed(2)} ${y[i + 1].toFixed(2)}`;
  }
  return d;
}

// A y-scale over the data AND the reference bounds, so the healthy band is always
// visible. Returns { y(v), band: {top,bottom} in svg units } for height h.
function makeScale(values, low, high, h, pad = 0.16) {
  const all = values.slice();
  if (low != null) all.push(low);
  if (high != null) all.push(high);
  let mn = Math.min(...all), mx = Math.max(...all);
  if (mn === mx) { mn -= 1; mx += 1; }
  const p = (mx - mn) * pad;
  mn -= p; mx += p;
  const y = (v) => h - ((v - mn) / (mx - mn)) * h;
  return {
    y,
    bandTop: high != null ? y(high) : 0,
    bandBottom: low != null ? y(low) : h,
  };
}

// ── boot ──────────────────────────────────────────────────────────────────────
let REPORT = null;

async function boot() {
  $("import-btn").addEventListener("click", onImportClick);
  $("marker-dialog").addEventListener("click", (e) => { if (e.target.id === "marker-dialog") $("marker-dialog").close(); });
  try {
    const d = await (await fetch("/api/blood/report", { headers: { "X-Oura-Dash": "1" } })).json();
    REPORT = d;
    if (d.error) { showError(d.error); return; }
    renderSummary(d.summary);
    renderImports(d.imports);
    renderAttention(d.markers);
    renderPanels(d.panels, d.markers);
    $("foot-meta").textContent = `${d.summary.markers_total} markers · ${d.summary.imports} reports · ${fmtDate(d.summary.first_date)}–${fmtDate(d.summary.latest_date)}`;
  } catch (e) {
    showError("Couldn't reach the blood API.");
  }
}

function showError(msg) {
  clear($("markers")).append(h("div", { class: "error" }, msg));
  clear($("imports"));
  clear($("summary"));
}

function onImportClick() {
  $("import-hint").textContent =
    "Preview: this panel is bundled. Once wired up, import parses the lab PDF locally, skips duplicates by content hash, and caches to a local blood.db — separate from the ring's oura.db.";
}

// ── summary strip ─────────────────────────────────────────────────────────────
function renderSummary(sm) {
  const box = clear($("summary"));
  const cell = (v, k, cls) => h("div", { class: "bl-ss" },
    h("div", { class: "bl-ss-v" + (cls ? " " + cls : "") }, v), h("div", { class: "bl-ss-k" }, k));
  box.append(
    cell(sm.markers_total, "markers tracked"),
    cell(sm.flagged, "out of range", sm.flagged ? "flag" : "clean"),
    cell(fmtDate(sm.latest_date), "latest draw"),
    cell(sm.imports, "reports imported"),
  );
}

// ── imported reports ──────────────────────────────────────────────────────────
function renderImports(imports) {
  const box = clear($("imports"));
  imports.forEach((im, i) => {
    box.append(h("div", { class: "bl-imp" + (i === 0 ? " latest" : "") },
      h("span", { class: "ic", style: "--i:url(/icons/test-tube.svg)" }),
      h("span", { class: "bl-imp-name" }, im.file),
      h("span", { class: "bl-imp-date" }, fmtDate(im.date)),
      h("span", { class: "bl-imp-meta" }, `${im.markers} markers · ${fmtSize(im.size)}`)));
  });
}

// ── attention (flagged) ───────────────────────────────────────────────────────
function renderAttention(markers) {
  const flagged = markers.filter((m) => m.flagged);
  const panel = $("attention-panel");
  if (!flagged.length) { panel.hidden = true; return; }
  panel.hidden = false;
  $("attention-count").textContent = `${flagged.length} of ${markers.length} markers`;
  const box = clear($("attention"));
  for (const m of flagged) {
    box.append(h("button", { class: "bl-flag", type: "button", onclick: () => openMarker(m) },
      h("div", { class: "bl-flag-head" },
        h("div", { class: "bl-flag-name" }, m.name),
        h("div", { class: "bl-flag-val" }, m.latest, h("span", { class: "u" }, m.unit))),
      h("div", { class: "bl-flag-note" }, m.advice || `${m.status === "high" ? "Above" : "Below"} the reference range (${m.ref_text} ${m.unit}).`)));
  }
}

// ── all panels ────────────────────────────────────────────────────────────────
function renderPanels(panels, markers) {
  const box = clear($("markers"));
  for (const panel of panels) {
    const inPanel = markers.filter((m) => m.panel === panel);
    if (!inPanel.length) continue;
    const grid = h("div", { class: "bl-grid" });
    for (const m of inPanel) grid.append(markerCard(m));
    box.append(h("section", { class: "bl-cat" },
      h("h3", { class: "bl-cat-h" }, panel, h("span", { class: "n" }, `${inPanel.length}`)), grid));
  }
}

function severityClass(m) { return m.severity === "watch" ? "watch" : m.severity === "good" ? "good" : "neutral"; }

function statusPill(m) {
  if (m.severity === "watch") return pill(m.status === "high" ? "High" : "Low", "warn");
  if (m.status === "normal") return pill("In range", "ok");
  return pill(m.status === "high" ? "High" : "Low", "ok"); // out of range but on the healthy side
}

function deltaChip(m) {
  if (m.delta_pct == null || m.trend === "flat") return h("span", { class: "bl-delta flat mut" }, "steady");
  const tone = m.trend_good ? "good" : "bad";
  return h("span", { class: `bl-delta ${m.trend} ${tone}` }, `${signed(m.delta_pct)}%`);
}

function markerCard(m) {
  return h("button", { class: `bl-card ${severityClass(m)}`, type: "button", onclick: () => openMarker(m) },
    h("div", { class: "bl-card-head" },
      h("div", { class: "bl-card-name" }, m.name),
      statusPill(m)),
    h("div", { class: "bl-card-val" },
      h("b", null, m.latest), h("span", { class: "u" }, m.unit), deltaChip(m)),
    sparkCard(m),
    h("div", { class: "bl-card-foot" },
      h("span", { class: "bl-ref" }, `ref ${m.ref_text}`),
      h("span", { class: "bl-ref" }, `${m.points.length} draws`)));
}

// compact card sparkline: reference band + trend line (no dots — the SVG is stretched)
function sparkCard(m) {
  const vals = m.points.map((p) => p.value);
  const W = 100, H = 34;
  const sc = makeScale(vals, m.low, m.high, H);
  const pts = vals.map((v, i) => [(i / Math.max(1, vals.length - 1)) * W, sc.y(v)]);
  const svg = s("svg", { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none" },
    s("rect", { class: "bl-band", x: 0, y: Math.max(0, sc.bandTop).toFixed(2), width: W, height: Math.max(0, sc.bandBottom - sc.bandTop).toFixed(2) }),
    s("path", { class: "bl-spark-line", d: smoothPath(pts) }));
  return h("div", { class: "bl-spark" }, svg);
}

// ── marker detail dialog ──────────────────────────────────────────────────────
function openMarker(m) {
  const watch = m.severity === "watch";
  $("dd-name").textContent = m.name;
  clear($("dd-sub")).append(
    h("b", { style: "color:var(--text);font-weight:600" }, `${m.latest} ${m.unit}`),
    `  ·  ref ${m.ref_text}  ·  `, statusPill(m));

  const body = clear($("dd-body"));
  body.append(detailChart(m, watch));
  body.append(h("p", { class: "bl-dd-about" }, m.about));
  if (m.advice) body.append(h("div", { class: "bl-dd-advice" + (watch ? " watch" : "") }, m.advice));

  // stat chips
  const stats = h("div", { class: "bl-dd-stats" },
    statChip("latest", `${m.latest} ${m.unit}`),
    m.delta != null ? statChip("since last", `${signed(m.delta)} (${signed(m.delta_pct)}%)`) : null,
    statChip("reference", `${m.ref_text} ${m.unit}`));
  body.append(stats);

  // values table
  const table = h("div", { class: "bl-values" }, h("h4", { class: "bl-values-h" }, "Every draw"));
  [...m.points].reverse().forEach((p) => {
    const oob = (m.high != null && p.value > m.high) ? "high" : (m.low != null && p.value < m.low) ? "low" : "in";
    const tag = oob === "in" ? pill("in range", "ok")
      : m.severity === "watch" ? pill(oob === "high" ? "high" : "low", "warn")
      : pill(oob === "high" ? "high" : "low", "ok");
    table.append(h("div", { class: "bl-vrow" },
      h("span", { class: "bl-vdate" }, fmtDate(p.date)),
      h("span", { class: "bl-vval" }, `${p.value} ${m.unit}`),
      h("span", { class: "bl-vtag" }, tag)));
  });
  body.append(table);

  $("marker-dialog").showModal();
}

function statChip(k, v) {
  return h("span", { class: "bl-stat" }, h("span", { class: "k" }, k), h("span", { class: "v" }, v));
}

// full-size time-series: shaded reference band + dashed edges + line + dated dots
function detailChart(m, watch) {
  const vals = m.points.map((p) => p.value);
  const W = 100, H = 100;
  const sc = makeScale(vals, m.low, m.high, H);
  const xs = vals.map((_, i) => (i / Math.max(1, vals.length - 1)) * W);
  const pts = vals.map((v, i) => [xs[i], sc.y(v)]);

  const kids = [];
  const bandY = Math.max(0, sc.bandTop), bandH = Math.max(0, sc.bandBottom - sc.bandTop);
  kids.push(s("rect", { class: "bl-band", x: 0, y: bandY.toFixed(2), width: W, height: bandH.toFixed(2) }));
  if (m.high != null) kids.push(s("line", { class: "bl-band-edge", x1: 0, y1: sc.y(m.high).toFixed(2), x2: W, y2: sc.y(m.high).toFixed(2) }));
  if (m.low != null) kids.push(s("line", { class: "bl-band-edge", x1: 0, y1: sc.y(m.low).toFixed(2), x2: W, y2: sc.y(m.low).toFixed(2) }));
  kids.push(s("path", { class: "bl-spark-line", d: smoothPath(pts) }));

  const svg = s("svg", { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none" }, ...kids);

  const chart = h("div", { class: "bl-chart" + (watch ? " watch" : "") }, svg);
  // dots as positioned HTML so they stay round under the non-uniform SVG stretch
  pts.forEach((pt, i) => {
    const last = i === pts.length - 1;
    chart.append(h("span", {
      class: "bl-dot-html",
      style: `position:absolute;left:${pt[0]}%;top:${(pt[1] / H * 100).toFixed(1)}%;width:${last ? 9 : 6}px;height:${last ? 9 : 6}px;`
        + `margin:${last ? -4.5 : -3}px 0 0 ${last ? -4.5 : -3}px;border-radius:50%;`
        + `background:${last ? (watch ? "var(--warn)" : "var(--accent)") : "var(--surface)"};`
        + `border:1.6px solid ${watch ? "var(--warn)" : "var(--accent)"};`,
    }));
  });

  const axis = h("div", { class: "bl-xaxis" });
  m.points.forEach((p) => axis.append(h("span", null, fmtDate(p.date))));
  return h("div", null, chart, axis);
}

boot();
