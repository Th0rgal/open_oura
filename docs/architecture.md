# Architecture

The Rust client is split by concern into three layers - **fetch → interpret →
apply** - across a small workspace of focused crates. The rule of thumb: each
crate owns one job and depends only "downward".

```
                ┌───────────────────────────────────────────────┐
   FETCH        │ oura-link   BLE transport, connect+auth, sync   │
                │             drain, live HR/ACM, features, RData │
                └───────────────┬───────────────────────────────┘
                                │ raw frames / events
                ┌───────────────▼───────────────────────────────┐
   INTERPRET    │ oura-protocol  framing · commands · auth crypto │  (low level)
   (decode)     │                device parsers · event decoders  │
                │                bytes → typed samples            │
                └───────────────┬───────────────────────────────┘
                                │ typed samples
                ┌───────────────▼───────────────────────────────┐
   INTERPRET    │ oura-analysis  ecore-ported algorithms:         │  (high level)
   (compute)    │                HRV, SpO2, baselines, sleep      │
                │                summary, sleep/readiness/activity │
                │                scores; SleepNet model wrapper    │
                └───────────────┬───────────────────────────────┘
                                │ daily metrics
                ┌───────────────▼───────────────────────────────┐
   APPLY        │ oura-store   SQLite: raw events (lossless),     │
                │              readings, daily metrics, cursor    │
                └────────────────────────────────────────────────┘

   oura-cli  orchestrates: fetch → store(raw) → decode → analyse → store(metrics)
             + the viz / game web UIs (motion_server)
```

## Crates and what goes where

| Crate | Layer | Owns | Depends on | I/O? |
| --- | --- | --- | --- |
| `oura-protocol` | interpret (decode) | packet framing, request builders, app-auth AES, `device` parsers, `events` decoders + typed sample/event structs | - | none (pure) |
| `oura-link` | fetch | `Transport` trait, `btleplug` `BleTransport`, `OuraClient` (firmware/battery/auth/sync/live/features/rdata) | oura-protocol | BLE, async |
| `oura-analysis` | interpret (compute) | ecore-derived metric algorithms; daily-metric structs; `sleepnet` model wrapper (feature-gated) | oura-protocol | none (pure compute) |
| `oura-store` | apply | SQLite schema + read/write, sync cursor, `redecode` | oura-protocol | SQLite |
| `oura-cli` | wiring | command dispatch, the viz/game servers | all of the above | everything |

**Where to add things**
- A new **wire decoder** (new event body) → `oura-protocol::events::decode_body`
  (+ a test with captured bytes). Never needs a re-sync; run `oura redecode`.
- A new **BLE command / capability** → `oura-protocol::protocol` (request builder)
  + a method on `OuraClient` in `oura-link`.
- A new **metric/algorithm** (score, sleep, baseline…) → a module in
  `oura-analysis`, with a doc under `docs/algorithms/` (see below).
- A new **persisted table / query** → `oura-store::storage`.
- A new **command or UI** → `oura-cli`.

## Errors

Each layer carries its own error type (no shared god-enum): `oura-link::Error`
(BLE/auth/protocol), `oura-store::Error` (SQLite). `oura-protocol` decoders are
infallible (return `Option`). `oura-cli` collapses everything to `anyhow`.

## Data flow (a sync)

1. `oura-link` connects, authenticates, and drains history events (raw frames).
2. `oura-protocol` decodes each event body into typed samples (or leaves it raw).
3. `oura-store` persists raw events + a per-device cursor (idempotent, incremental).
4. `oura-analysis` turns stored samples into daily metrics (HRV, scores, …).
5. `oura-store` persists the metrics; `oura-cli`/UIs read them back.

## Maintaining algorithm docs

Every ported algorithm in `oura-analysis` is documented one-file-per-metric under
`docs/algorithms/`, each recording: **source** (ecore function `@ addr` or model
file + version), **inputs**, the **formula/constants**, the **Rust impl location**,
and a **status/confidence + validation** note. The module's doc-comment links to
its markdown. `docs/algorithms/README.md` holds the index + status table. Update
the status when an algorithm moves from best-effort → validated (e.g. against the
trends CSV or captured data).
