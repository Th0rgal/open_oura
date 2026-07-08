// open_oura DNA explorer — fetches /api/dna/* (Rust parses the .vcf.gz + scores it)
// and renders traits + polygenic scores. Web-only feature; see docs/clients-web-and-ios.md.
"use strict";

const $ = (id) => document.getElementById(id);

// Tiny safe hyperscript. String/number children become text nodes (never HTML),
// so nothing built through h() can be an injection sink — no manual escaping needed.
function h(tag, attrs, ...kids) {
  const n = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === "class") n.className = v;
      else if (k === "html") n.innerHTML = v; // only for trusted static markup
      else if (k.startsWith("on") && typeof v === "function") n.addEventListener(k.slice(2), v);
      else n.setAttribute(k, v);
    }
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    n.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return n;
}
const clear = (node) => { node.replaceChildren(); return node; };
const extLink = (href, label) => h("a", { href, target: "_blank", rel: "noopener" }, label);
// esc is kept only for the few messages that embed trusted <code> markup (empty states).
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const getDash = (url) => fetch(url, { headers: { "X-Oura-Dash": "1" } });
const postDash = (url, body) => fetch(url, { method: "POST", headers: { "X-Oura-Dash": "1", "Content-Type": "application/json" }, body: JSON.stringify(body) });

const KIND = { good: "ok", watch: "warn", neutral: "neutral" };
const kindOf = (m) => KIND[m] || "neutral";
const pill = (label, magnitude) => h("span", { class: `pill ${kindOf(magnitude)}` }, h("i"), label);
const fmtSize = (b) => (b >= 1e9 ? (b / 1e9).toFixed(1) + " GB" : b >= 1e6 ? (b / 1e6).toFixed(0) + " MB" : (b / 1e3).toFixed(0) + " KB");
const fmtNum = (n) => (n == null ? "—" : n.toLocaleString());
const signed = (n) => (n >= 0 ? "+" : "") + n;

const CATEGORY_ORDER = ["nutrition", "fitness", "sleep", "appearance", "behaviour", "pharma", "health", "other"];
const CAT_LABEL = { nutrition: "Nutrition & metabolism", fitness: "Fitness", sleep: "Sleep & circadian", appearance: "Traits & appearance", behaviour: "Mind & behaviour", pharma: "Medication response", health: "Health signals", other: "Other" };

// ── state ────────────────────────────────────────────────────────────────────
const state = {
  genome: null,
  available: [],            // score descriptors from /api/dna/files
  selected: new Set(),      // selected score keys
  reqId: 0,                 // guards against out-of-order report responses
};

// ── boot ─────────────────────────────────────────────────────────────────────
async function boot() {
  await refreshFiles(true);
  $("file").addEventListener("change", () => { state.genome = $("file").value; loadReport(); });
  $("q-btn").addEventListener("click", runLookup);
  $("q").addEventListener("keydown", (e) => { if (e.key === "Enter") runLookup(); });
  $("add-form").addEventListener("submit", (e) => { e.preventDefault(); fetchPgs(); });
}

// Load the genome + score lists. On first call, default-select the built-ins.
async function refreshFiles(initial) {
  try {
    const d = await (await getDash("/api/dna/files")).json();
    const genomes = d.genomes || [];
    const dir = d.genomes_dir;
    state.available = d.scores || [];

    state.genomes = genomes;
    const KIND_TAG = { cnv: " · CNV (not scoreable)", sv: " · SV (not scoreable)" };
    const sel = clear($("file"));
    if (genomes.length) {
      for (const g of genomes) sel.append(h("option", { value: g.name }, `${g.name} · ${fmtSize(g.size)}${KIND_TAG[g.kind] || ""}`));
      // prefer a scoreable (SNP/indel) file; only fall back to a CNV/SV if that's all there is
      const firstScoreable = genomes.find((g) => g.scoreable);
      if (!state.genome || !genomes.some((g) => g.name === state.genome)) state.genome = (firstScoreable || genomes[0]).name;
      sel.value = state.genome;
      $("genome-dir").textContent = dir ? `${genomes.length} in ${dir}` : `${genomes.length} file${genomes.length === 1 ? "" : "s"}`;
    } else {
      sel.append(h("option", null, "no genome files"));
      $("genome-dir").textContent = dir ? `none in ${dir}` : "";
      showEmpty(d.error || (dir
        ? `No <code>*.vcf.gz</code> in <code>${esc(dir)}</code>. Point the explorer elsewhere with <code>oura dashboard --dna-files &lt;dir&gt;</code> or <code>$OURA_DNA_FILES</code>.`
        : "No genome directory."));
    }

    if (initial) {
      // default: apply the illustrative built-ins so the page isn't empty
      state.selected = new Set(state.available.filter((s) => s.source === "builtin").map((s) => s.key));
    } else {
      // keep only still-existing selections
      const keys = new Set(state.available.map((s) => s.key));
      state.selected = new Set([...state.selected].filter((k) => keys.has(k)));
    }
    renderPicker();
    if (genomes.length) loadReport();
  } catch (e) {
    showEmpty("Couldn't reach the DNA API.");
  }
}

// msg may contain trusted <code> markup (empty-state hints). It is set as HTML on a
// fresh node — never appended to existing children — so there is no re-parse hazard.
function showEmpty(msg) {
  clear($("scores"));
  clear($("traits")).append(h("div", { class: "error", html: msg || "No genome files found." }));
}

// ── score picker (the selector) ──────────────────────────────────────────────
function renderPicker() {
  const box = clear($("score-picker"));
  if (!state.available.length) {
    box.append(h("div", { class: "dna-hint" }, "No scores yet — add one from the PGS Catalog below."));
  }
  for (const s of state.available) box.append(pickerRow(s));
  updateHint();
}

function pickerRow(s) {
  const cb = h("input", { type: "checkbox" });
  cb.checked = state.selected.has(s.key);
  cb.addEventListener("change", () => {
    if (cb.checked) state.selected.add(s.key); else state.selected.delete(s.key);
    updateHint();
    loadReport();
  });

  const isPgs = s.source === "pgs";
  const badge = h("span", { class: `dna-badge ${isPgs ? "pgs" : "builtin"}` }, isPgs ? "PGS" : "demo");
  const meta = [];
  if (isPgs && s.id) meta.push(h("a", { href: `https://www.pgscatalog.org/score/${s.id}/`, target: "_blank", rel: "noopener", onclick: (e) => e.stopPropagation() }, s.id));
  if (s.variants != null) meta.push(`${fmtNum(s.variants)} variants`);
  if (s.weight_type && s.weight_type !== "NR") meta.push(s.weight_type);
  if (s.build) meta.push(s.build);

  return h("label", { class: "dna-pick" }, cb,
    h("span", { class: "dna-pick-body" },
      badge,
      h("span", { class: "dna-pick-name" }, s.name || s.id || s.key),
      h("span", { class: "dna-pick-meta" }, interpose(meta, " · "))));
}

// Join a list of nodes/strings with a separator, producing a flat array of children.
function interpose(items, sep) {
  const out = [];
  items.forEach((it, i) => { if (i) out.push(sep); out.push(it); });
  return out;
}

function updateHint() {
  const n = state.selected.size;
  $("scores-hint").textContent = n ? `${n} selected` : "none selected";
}

// ── PGS Catalog fetch ────────────────────────────────────────────────────────
async function fetchPgs() {
  const id = $("pgs-id").value.trim().toUpperCase();
  const status = $("add-status");
  const btn = $("add-btn");
  if (!/^PGS\d{6,9}$/.test(id)) { status.className = "dna-add-status err"; status.textContent = "Enter a PGS Catalog ID like PGS000004."; return; }
  btn.disabled = true;
  status.className = "dna-add-status busy";
  status.textContent = `Fetching ${id} from the PGS Catalog…`;
  try {
    const d = await (await postDash("/api/dna/fetch", { pgs_id: id })).json();
    if (!d.ok) { status.className = "dna-add-status err"; status.textContent = d.message || "Fetch failed."; btn.disabled = false; return; }
    status.className = "dna-add-status ok";
    const v = d.meta && d.meta.variants_number;
    status.textContent = `Added ${id}${d.meta && d.meta.trait_reported ? " — " + d.meta.trait_reported : ""}${v ? ` (${fmtNum(v)} variants)` : ""}.`;
    $("pgs-id").value = "";
    // re-list, auto-select the newly fetched score, and re-score
    await refreshFiles(false);
    const added = state.available.find((s) => s.file === d.file);
    if (added) { state.selected.add(added.key); renderPicker(); loadReport(); }
  } catch (e) {
    status.className = "dna-add-status err";
    status.textContent = "Fetch failed (network or server error).";
  } finally {
    btn.disabled = false;
  }
}

// ── report ───────────────────────────────────────────────────────────────────
const KIND_NAME = { cnv: "copy-number (CNV)", sv: "structural-variant (SV)" };

async function loadReport() {
  if (!state.genome) return;

  // CNV/SV callsets carry no point genotypes — scoring them is meaningless.
  // Explain instead of parsing hundreds of MB for nothing.
  const g = (state.genomes || []).find((x) => x.name === state.genome);
  if (g && g.scoreable === false) {
    $("foot-meta").textContent = "";
    delete $("traits").dataset.loaded;
    clear($("traits"));
    clear($("scores")).append(
      h("div", { class: "dna-notice" },
        h("b", null, `This is a ${KIND_NAME[g.kind] || g.kind} file.`),
        " It lists large rearrangements, not the single-base genotypes that traits and polygenic scores need. ",
        "Pick the ", h("b", null, "SNP / indel"), " file from the Genome menu — that's the one with your per-position calls."));
    return;
  }

  const reqId = ++state.reqId;
  clear($("scores")).append(h("div", { class: "skeleton skeleton-block" }));
  if (!$("traits").dataset.loaded) clear($("traits")).append(h("div", { class: "skeleton skeleton-block" }));
  const scores = [...state.selected].join(",");
  try {
    const url = `/api/dna/report?file=${encodeURIComponent(state.genome)}&scores=${encodeURIComponent(scores)}`;
    const d = await (await getDash(url)).json();
    if (reqId !== state.reqId) return; // a newer request superseded this one
    if (d.error) { showEmpty(esc(d.error)); return; }
    $("build").textContent = d.build || "GRCh38";
    const foundCount = (d.traits || []).filter((t) => t.found && !t.no_call).length;
    $("foot-meta").textContent = `${foundCount}/${d.traits_total} traits · ${d.applied_scores} score${d.applied_scores === 1 ? "" : "s"} · ${state.genome}`;
    renderScores(d.scores || []);
    renderTraits(d.traits || []);
    $("traits").dataset.loaded = "1";
  } catch (e) {
    if (reqId === state.reqId) showEmpty("Couldn't load the report.");
  }
}

function renderScores(scores) {
  const box = clear($("scores"));
  if (!scores.length) {
    box.append(h("div", { class: "dna-hint" }, "No scores selected. Tick one above, or add a PGS Catalog score."));
    return;
  }
  for (const s of scores) box.append(scoreCard(s));
}

function scoreCard(s) {
  const isPgs = s.source === "pgs";

  // header: title + source line, and a qualitative band pill (illustrative only)
  const source = interpose([
    isPgs && s.id ? extLink(`https://www.pgscatalog.org/score/${s.id}/`, s.id) : null,
    isPgs ? "PGS Catalog" : "illustrative",
    s.genome_build || null,
  ].filter(Boolean), " · ");
  const head = h("div", { class: "dna-score-head" },
    h("div", { class: "dna-score-titles" },
      h("div", { class: "dna-score-name" }, s.name),
      h("div", { class: "dna-score-source" }, source)),
    s.band ? pill(s.band.label, s.band.magnitude) : null);

  // headline value
  const scaleNote = s.log_scaled ? "log-odds sum" : (isPgs ? "weighted sum" : "score");
  const value = h("div", { class: "dna-score-value" }, h("b", null, s.value), h("span", null, scaleNote));

  // gauge fill means two different things — label it honestly for each:
  //   PGS         → fraction of the score's variants found in this genome (coverage)
  //   illustrative→ where the value sits on its qualitative band scale (a position)
  const pct = Math.max(2, Math.min(100, (s.fraction || 0) * 100));
  const gauge = h("div", { class: "gauge dna-score-gauge" }, h("i", { style: `width:${pct}%` }));
  const scale = s.metric === "coverage"
    ? h("div", { class: "scale" }, h("span", null, "coverage"), h("span", null, `${fmtNum(s.matched)}/${fmtNum(s.total)} variants read`))
    : h("div", { class: "scale" }, h("span", null, "lower"), h("span", null, "higher"));

  // rigor stats (matched / excluded / weighting) — the SNP counts live here, not the gauge
  const excluded = (s.ambiguous || 0) + (s.mismatched || 0);
  const stats = h("div", { class: "dna-stats" },
    statChip("matched", `${fmtNum(s.matched)}/${fmtNum(s.total)}`),
    excluded ? statChip("excluded", fmtNum(excluded), `${s.ambiguous} strand-ambiguous + ${s.mismatched} allele mismatch`) : null,
    statChip("weights", s.log_scaled ? `${s.weight_type} → ln` : (s.weight_type || "NR")));

  const card = h("div", { class: "dna-score" }, head, value, gauge, scale, stats);

  // contributors
  if (s.contributors && s.contributors.length) {
    const chips = h("div", { class: "dna-contribs" });
    for (const c of s.contributors) {
      chips.append(h("span", { class: "dna-chip " + (c.contribution >= 0 ? "up" : "down") },
        `${c.rsid} ${c.dosage}×${c.effect_allele} ${signed(c.contribution)}`));
    }
    card.append(h("div", { class: "dna-contribs-label" }, "Largest contributors in your genome"), chips);
  }

  if (s.note) card.append(h("div", { class: "dna-score-note" }, s.note));
  if (isPgs) {
    card.append(h("div", { class: "dna-score-caveat" },
      "A raw polygenic sum has no absolute meaning without an ancestry-matched reference distribution (not shipped). Read it as your relative genetic load for this trait, never as a diagnosis or percentile."));
  }
  return card;
}

function statChip(k, v, title) {
  return h("span", title ? { class: "dna-stat", title } : { class: "dna-stat" },
    h("span", { class: "k" }, k), h("span", { class: "v" }, v));
}

// ── traits ───────────────────────────────────────────────────────────────────
function renderTraits(traits) {
  const box = clear($("traits"));
  const groups = {};
  for (const t of traits) (groups[t.category] || (groups[t.category] = [])).push(t);
  const cats = Object.keys(groups).sort((a, b) => {
    const ia = CATEGORY_ORDER.indexOf(a), ib = CATEGORY_ORDER.indexOf(b);
    return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
  });
  for (const cat of cats) {
    const grid = h("div", { class: "dna-grid" });
    for (const t of groups[cat]) grid.append(traitCard(t));
    box.append(h("section", { class: "dna-cat" }, h("h3", { class: "dna-cat-h" }, CAT_LABEL[cat] || cat), grid));
  }
}

function traitCard(t) {
  const interp = t.interp;
  const mag = interp ? interp.magnitude : "neutral";
  const found = t.found && !t.no_call;

  const head = h("div", { class: "dna-card-head" },
    h("div", { class: "dna-card-name" }, t.name),
    h("div", { class: "dna-card-geno" }, found ? t.genotype : "—"));

  let body = null;
  if (found && interp) body = interp.note ? h("div", { class: "dna-card-sub" }, interp.note) : null;
  else if (t.no_call) body = h("div", { class: "dna-card-sub" }, "No call at this position in your file.");
  else if (!t.found) body = h("div", { class: "dna-card-sub" }, "Not present in this genome file.");
  else body = h("div", { class: "dna-card-sub" }, "Genotype not in the catalog table.");

  const rsid = t.source_url ? extLink(t.source_url, t.rsid) : t.rsid;
  const meta = h("div", { class: "dna-card-rs" }, rsid, ` · chr${t.chrom}:${t.pos}`);

  return h("article", { class: "dna-card " + (found ? mag : "neutral") + (found ? "" : " miss") },
    head,
    found && interp ? pill(interp.label, mag) : null,
    body,
    meta);
}

// ── single-variant lookup ────────────────────────────────────────────────────
async function runLookup() {
  const q = $("q").value.trim();
  const out = clear($("q-result"));
  if (!q) return;
  if (!state.genome) { out.append(h("div", { class: "error" }, "Pick a genome first.")); return; }
  out.append(h("div", { class: "dna-hint" }, `Searching ${state.genome}…`));
  try {
    const d = await (await getDash(`/api/dna/lookup?file=${encodeURIComponent(state.genome)}&q=${encodeURIComponent(q)}`)).json();
    clear(out);
    if (d.error) { out.append(h("div", { class: "error" }, d.error)); return; }
    if (!d.found) { out.append(h("div", { class: "dna-hint" }, `No record for “${q}” in this file. (Whole-genome files can omit non-variant sites.)`)); return; }
    out.append(variantCard(d.variant, d.annotation));
  } catch (e) {
    clear(out).append(h("div", { class: "error" }, "Lookup failed."));
  }
}

function variantCard(v, ann) {
  const name = ann && ann.trait ? ann.trait : (v.rsid && v.rsid !== "." ? v.rsid : "Variant");
  const rec = h("div", { class: "dna-rec" },
    h("div", { class: "dna-rec-head" },
      h("div", { class: "dna-rec-name" }, name),
      h("div", { class: "dna-geno" }, v.no_call ? "no call" : v.key)));

  if (ann && ann.interp) rec.append(pill(ann.interp.label, ann.interp.magnitude));
  rec.append(h("div", { class: "dna-rec-meta" }, `chr${v.chrom}:${v.pos} · ${v.rsid} · REF ${v.ref} / ALT ${v.alt}`));

  const note = ann && ann.interp && ann.interp.note ? ann.interp.note : (ann && ann.note) || null;
  if (note) {
    const n = h("div", { class: "dna-rec-note" }, note);
    if (ann && ann.source_url) n.append(" ", extLink(ann.source_url, "SNPedia →"));
    rec.append(n);
  }
  return rec;
}

boot();
