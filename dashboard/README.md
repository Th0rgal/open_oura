# open_oura web dashboard

A local, single-page health dashboard that turns your synced ring data into the
few numbers that matter. Follows the product vision in
`notes/dashboard-v2-brainstorm.md`: a calm, baseline-aware view across **sleep,
cardiovascular, blood-oxygen, activity**, and a **Device & Data Health** panel.
Everything is computed and served locally; nothing leaves the machine.

## Run

```bash
oura sync                                    # refresh your data first
oura dashboard --tz-offset 1 --age 30 --sex M --height 1.78 --weight 75
# → open http://127.0.0.1:8090
```

Default port `8090`; loopback-only.

## In the dashboard

- **Sync button** (header): drains the ring over Bluetooth without leaving the page
  (runs `oura sync` for you, then refreshes). Pass `--name`/`--key-file` so it can
  reach your ring, e.g. `--name "Oura Ring 5" --key-file captures/ring5.key`.
- **Battery**: read offline from the ring's stored `battery_level_changed` debug
  events; shown in the header and the Device panel.
- **Your details** (the person icon): edit age / sex / height / weight / ring size.
  The ring can't measure these, so they live in an editable, gitignored
  `profile.json` next to `oura.db` and feed the cardiovascular-age model (the runners
  read it too). `--age/--sex/--height/--weight` only seed it on first run.
- **Activity**: detected sessions (Oura's `automatic_activity_detection`) plotted on a
  per-day actogram over a continuous MET "movement ridge"; tap a session for its time,
  duration, model label, and **estimated active calories** (Σ(MET−1)·weight/60).
- **Advanced & debugging** (collapsed, in the Device panel):
  - **Ring auth key — Export / Import.** *Export* shows the 16-byte key (copy, `.key`
    download, or QR) so you can move it to another device — e.g. set up the native iOS
    app without re-pairing. *Import* takes a pasted hex key, an uploaded `.key`, or a
    scanned QR and writes it to the `--key-file` (`0600`, lowercased, validated as
    32 hex chars). Both need `--key-file` set; the key is only read/written locally.
  - **Capabilities — tap to toggle.** Turn on-ring features (Daytime HR, SpO2, Exercise
    HR, Real steps, Cardio PPG) on/off over Bluetooth (`SetFeatureMode`, auth-gated).
    The on/off state is the **real on-ring mode** snapshotted at the last sync (see
    below), not just "events seen recently".
  - **Event stream**: per-type counts of what the ring is actually recording.

On each sync the CLI snapshots the real on-ring feature modes to a gitignored
`feature_modes.json` next to `oura.db`, so the capability toggles reflect the device's
actual state. It's best-effort — a failed read never fails the sync.

## How it's built

- **Rust does the work** (`crates/oura-cli/src/dashboard.rs`): reads `oura.db`, and
  computes per-night HRV / resting-HR / skin-temp, SpO2 % (Oura's R→% calibration),
  baselines + deltas, per-day movement profile + active calories, the
  device/data-health panel, and the one-line digest. It serves the page plus a small
  JSON API: `GET /api/summary`, `GET/POST /api/profile`, `POST /api/sync`,
  `POST /api/feature` (toggle a capability), and `GET/POST /api/ring-key`. Mutating
  endpoints require a same-origin `X-Oura-Dash` header (CSRF guard) and the server is
  loopback-only with a Host-header check.
- **The AI models run via the Python runners**, shelled out exactly like
  `oura sessions`: the sleep hypnogram (`run_sleep_model.py`), activity sessions
  (`run_activity_model.py`), and cardiovascular age (`run_cva_model.py`). Each is
  invoked with `--json`. `/api/summary` runs them **concurrently** (sleep scores every
  night in one batched process) and **caches** the result, recomputing only when
  `oura.db` or `profile.json` changes — so the first load pays the cost and refreshes
  are instant.
- **Frontend** (`dashboard/web/`): vanilla HTML/CSS/JS, no build step, no external
  fonts or libraries. Auto light/dark via `prefers-color-scheme` (force one with
  `?` … set `document.documentElement.dataset.theme = 'dark' | 'light'`). The web
  files are read from disk at request time, so you can edit and refresh without
  recompiling (a copy is also embedded in the binary as a fallback).

## Folder

```
dashboard/
  web/                 # the static UI (served by the Rust server)
    index.html
    styles.css
    app.js
  README.md
```

Models are Oura's proprietary IP and are **not** committed; the runners reference
your own locally-decrypted copies under `notes/models/`.

Icons are from [Phosphor](https://phosphoricons.com) (MIT), vendored under
`dashboard/web/icons/` so the dashboard stays fully offline.
