# Two clients, one core — keep them in sync

open_oura has **two user-facing clients that render the same health data**. When you
add or change a feature, you almost always have to touch **both**. This is the map.

| | Web dashboard | Native iOS app |
| --- | --- | --- |
| Where | `dashboard/web/` (vanilla HTML/CSS/JS) served by `crates/oura-cli/src/dashboard.rs` | `apps/ios/OuraApp/` (SwiftUI) on `crates/oura-core` (UniFFI) |
| Entry | `oura dashboard` → `http://127.0.0.1:8090` | `apps/ios/OuraApp/build_run.sh` (model-free) / `build_run_torch.sh` (on-device models) |
| Render code | `app.js`, `styles.css`, `index.html` | `OuraApp.swift`, `Theme.swift` |
| Models run via | Python torch runners (`tools/run_*_model.py`) | on-device `.ptl` (`TorchBridge.{h,mm}` + `SleepStaging`/`CvaModel`/`ActivityModel.swift`) |

## The one shared brain: `crates/oura-summary`

`oura_summary::build_summary()` computes **the summary JSON both clients render** — vitals,
per-night stats, the digest, the MET activity profile, steps/kcal, device health. The web
calls it in `dashboard.rs`; iOS calls it through `oura-core`'s `summary_json()` FFI. The
models are injected via the `ModelRunner` trait (web: `PythonRunner`; iOS: `NoModelRunner`
+ the on-device torch code).

The non-model math is the **ported ecore ground truth** from `crates/oura-analysis`
(`ported::{spo2, temperature, metabolic, baseline}`): SpO₂ calibration, nightly skin
temperature, Schofield BMR (→ `total_kcal`), Jackson VO₂max, steps→distance, and the
annealing-EMA personal baseline behind each vital's `delta_pct`. Add a new derived
metric there once and both clients receive it in the JSON.

**So the rule of thumb:**

- **A new computed metric / field** → add it once in `oura-summary` (`build_summary`). Both
  clients receive it in the JSON. Then render it in **both** `app.js` and `OuraApp.swift`.
- **A new visualization / UI** (no new data) → do it in **both** `app.js` and `OuraApp.swift`.
- **A new model** → wire **both** runners: a `tools/run_*_model.py` (used by `PythonRunner`)
  **and** an `oura_*` function in `TorchBridge.mm` + a Swift `*Model.swift` that builds the
  same input tensors and folds the result into the summary.

## Feature ↔ feature correspondence

| Feature | Web (`app.js`) | iOS (`OuraApp.swift`) | Data (JSON key) | Model |
| --- | --- | --- | --- | --- |
| Digest headline | `load()` digest | `RootView` digest | `digest` | — |
| Vitals (HRV/RHR/temp/SpO₂) | `renderTiles` / `VitalCell`-like | `VitalCell` | `vitals`, `nights[]` | — |
| **Unified day (night + activity)** | `renderDay`, `dayCard` | `TodayCard` | `nights[]`, `activity*` | — |
| **Full-page sleep report** (polysomnograph + clinical metrics + interpretation) | `openDayPage`→`sleepReport`, `polysomnograph`, `hypnoSvg` | `DayReportView`→`SleepReport`, `Polysomnograph` (Reports.swift) | `nights[].{stages_full,series,metrics}`, `sleep_debt` | SleepNet |
| **Full-page activity report** (24h MET profile + intensity metrics) | `openDayPage`→`activityReport`, `metProfileSvg` | `DayReportView`→`ActivityReport`, `MetProfile` (Reports.swift) | `activity_profile`, `activity_daily`, `activity` | AAD |
| Stage breakdown | `stageBar` | `StageBreakdown` | `nights[].{deep,light,rem,wake}_pct` | SleepNet |
| **Autonomic recovery by stage** (mean HR/HRV in deep/light/REM) | `sleepReport` autonomic grid | `SleepReport` `autonomicGrid` | `nights[].autonomic` | SleepNet (needs hypnogram) |
| **Cardiovascular age** | `renderCardio` | Cardio section | `cardio` | CVA (web: Python · iOS: `CvaModel`) |
| **VO₂max estimate** | `renderCardio` | Fitness section | `fitness.vo2max` | — (Jackson, model-free) |
| Movement ridge | `ridgeSvg` | `MovementRidge` | `activity_profile` | — (MET, model-free) |
| **Activity sessions / workouts** | `openActDetail` (session) | workouts section | `activity` | AAD (web: Python · iOS: `ActivityModel`) |
| Steps / active calories / **distance** | activity report stats | activity day stats | `activity_daily` (incl. `distance_m`) | — |
| Previous days browser | `openDaysBrowser` → `openDayPage` | `AllDaysView` → `DayDetailView` | day keys | — |
| Device & data health | `renderDevice` | device section | `device`, `streams` | — |

## The day is one unit — pair night + activity by *wake date*

Both clients render **one "day" = last night's sleep + that day's activity**, drillable
into either half and browsable back through previous days. The hero on each home screen is
the most recent day; "show all N days" (web: `openDaysBrowser`; iOS: `AllDaysView`) opens
the rest, each as a combined night+activity detail.

The **pairing rule matters and must stay identical across clients**: nights are labelled by
their **onset** date (the evening you went to bed), so an overnight sleep that crosses
midnight belongs to the *next* day's morning. A day `D` pairs with the sleep you *woke from*
on the morning of `D` — the night whose **wake date** is `D`, not whose onset date is `D`.
This lives in `wakeYmd()` (web `app.js`) and `Summary.wakeYmd` (iOS `Models.swift`); keep the
two in lockstep. `nightForDay`/`night(forDay:)` pick the longest in-bed night for a morning so
a nap doesn't shadow the real sleep.

## Where the two clients diverge

- **Home layout**: same day-unit model on both, but the iOS "Observatory" theme floats data on
  a black canvas (no panels) while the web uses bordered cards/dialogs. Match *data/features*,
  not pixel-for-pixel layout. iOS opens details as sheets; the web as stacked `<dialog>`s.
- **BLE sync**: iOS syncs **natively** — `RingSync.swift` (CoreBluetooth `BLETransport`)
  drives the Rust `RingSession` FFI (`oura-core`) to authenticate + drain into a writable
  DB. The web dashboard has **no** BLE; it reads a DB produced by the desktop `oura sync`.
  Both ultimately run the SAME `oura-link` `OuraClient<T: Transport>` over a different
  transport (btleplug on desktop, CoreBluetooth-over-FFI on iOS).

## Sleep metrics: two code paths, one algorithm — keep them in sync

The clinical sleep metrics (onset/REM latency, WASO, awakenings, cycles, fragmentation) and
sleep debt are computed **twice** and must stay identical: once in Rust (`oura-summary`
`sleep_metrics` / `smooth_stages` / `count_bouts` / `count_periods`, and the `sleep_debt`
port) for the web, and once in Swift (`Reports.swift` `Sleep.metrics` / `Sleep.smooth` +
`Summary.sleepDebt`) for iOS. The web reads them from the FFI JSON; iOS recomputes from the
**on-device** SleepNet hypnogram (`NightRow.stages`), because iOS runs `build_summary` with
`NoModelRunner` (no server-side staging), so the FFI `stages_full`/`metrics` are empty there.
The raw signal series (`nights[].series`) DO come from the FFI on both. If you change the
smoothing window or a metric definition, change **both** implementations.

**Autonomic-by-stage** (mean HR/HRV per sleep stage) is the same story: Rust
`autonomic_by_stage` fills `nights[].autonomic` for the web; iOS recomputes in Swift
(`Sleep.autonomic`) from its on-device hypnogram since that FFI field is null under
`NoModelRunner`. One deliberate difference: the web maps each HRV/HR sample to a stage by its
**true timestamp** (`hrv_event` gives `interval_min`-spaced samples), while iOS only has the
even-spread downsampled `series`, so it aligns by **index fraction** — the two can differ by a
hair. We expose per-stage means (esp. deep-sleep HRV) rather than an overnight HRV "slope":
nocturnal HRV is stage-driven (deep ↑, REM ↓), so a slope tracks stage order, not recovery —
which is why Oura's own app has no per-night HRV trend either.

## Known gaps (web-only, not yet on iOS)

- **Advanced & debugging**: on-ring feature toggles (`/api/feature`), the per-type event
  stream, profile editing.
- **Polysomnograph crosshair**: web has a hover crosshair; iOS uses a touch scrubber
  (drag across the lanes) — same idea, adapted to the input.
- **DNA explorer** (`/dna`): reads genome `*.vcf.gz` files and scores single-SNP **traits**
  against the editable `dna/catalog.json`, plus **polygenic scores** — the illustrative
  built-ins in the catalog *and* real [PGS Catalog](https://www.pgscatalog.org/) scoring
  files (`dna/scores/*.txt.gz`). Parsing/scoring is the `crates/oura-dna` crate
  (`vcf`/`catalog`/`pgs`/`score` modules); the server glue is `crates/oura-cli/src/dna.rs`
  → `dashboard/web/dna.{html,js,css}`. A genome + which scores to apply are chosen with
  selectors; a PGS ID can be fetched on demand (`POST /api/dna/fetch` → EBI) into
  `dna/scores/`. Genomes are read from a **configurable directory** — keep your large,
  private files anywhere via `oura dashboard --dna-files <dir>` (or `$OURA_DNA_FILES`);
  it defaults to the repo's `dna/files/`, while the catalog + fetched PGS scores always
  live in the repo `dna/`. PGS scoring is strict: effect+other-allele matching, strand-flip
  resolution, palindromic-ambiguous exclusion, `weight_type` (OR/HR → `ln`), and coverage
  stats — a raw sum is reported honestly (no population reference is shipped, so no
  percentile). Trait interpretation is **strand-aware** too (reverse-complement fallback for
  non-palindromic SNPs), since a GRCh38 VCF stores e.g. `rs4988235` as A/G while catalogs
  write the classic C/T. **Deliberately web-only** — it has nothing to do with ring data, so
  it does not go through `oura-summary` and is not mirrored on iOS. If it's ever wanted on
  iOS, the `oura-dna` crate is the reusable brain.

  *Whole-genome (gVCF) support:* the reader handles 30x WGS **genomic VCFs** — most of the
  genome is stored as `END=` **reference blocks**, so a single streaming pass resolves any
  trait/PGS locus inside a hom-ref block as homozygous-reference (a per-chromosome merge-join
  cursor). Without this, coverage would collapse to only the sites where the sample carries a
  variant. One 298 MB / 30x gVCF parses in ~6 s (then cached); a `pos_set` gate lets the
  ~tens-of-millions of non-target records skip all lookups. A `.snp-indel` file is the one to
  use — `list_files` classifies each `*.vcf.gz` (`snp-indel` vs `cnv`/`sv`) so the UI prefers
  the scoreable one and explains the copy-number / structural-variant files instead of
  scoring them to noise.

  *Network note:* this is the **only** outbound request in the whole app. It fetches
  **public** PGS score *definitions* on explicit user action; the genome never leaves the
  machine.

- **Blood panel** (`/blood`): tracks lab-test markers over time, reads each against its
  reference range, and surfaces the ones worth attention with plain-language advice. The
  compute — status (in/out of range), trend across draws, which side is "concerning" per
  marker, and the attention list — is real and lives in `crates/oura-cli/src/blood.rs`;
  the front-end is `dashboard/web/blood.{html,js,css}` (each marker card is a
  reference-band sparkline in the main dashboard's graph idiom, with a full time-series +
  advice in the detail dialog). **Currently the *inputs* are mocked** — a real SYNLAB draw
  series, hand-transcribed — so import/extraction is not yet wired. The planned shape:
  `import` parses an uploaded lab PDF locally, **dedupes by content hash** (re-importing the
  same file is a no-op), and caches to a small local **SQLite `blood.db`, separate from the
  ring's `oura.db`**. **Deliberately web-only** — like the DNA explorer it has nothing to do
  with ring data, does not go through `oura-summary`, and is not mirrored on iOS. When
  extraction is built, `blood.rs`'s marker model is the reusable brain.

When you close one of these gaps, update this section.

## Ring clock resets → epoch-aware time mapping (all three code paths)

`ring_timestamp` (ds) is a **per-boot relative deciseconds counter**: it resets to ~0
every time the ring reboots (battery drain, firmware reset). Naively anchoring every ds
to one global `max_ds`/`captured_unix` scatters older boots to wildly wrong dates (a boot
can land months in the past). The fix segments events into boot **epochs** — walk in real
sync order `(captured_unix, then ds)`, split on any large backward jump in ds, anchor each
epoch's newest ds to that event's capture time — and maps ds→wall-clock per epoch. This
lives in **three places that must stay in sync**:

- `crates/oura-summary/src/lib.rs` — the shared brain (`unix_s`); fixes night/activity/
  movement **dates for both clients** at once.
- `tools/epoch_time.py` (helper) used by `tools/run_activity_model.py` and
  `tools/run_sleep_model.py` — the **web** on-model session/hypnogram times.
- `apps/ios/OuraApp/EventStore.swift` (`epochs` / `unixSeconds`) used by
  `ActivityModel.swift` and `SleepStaging.swift` — the **iOS** on-device model times.
  iOS must be rebuilt to pick this up.

Operational gap (not yet fixed): `oura sync` keys incremental pulls off the PC-side cursor
(`sync_state.next_cursor`, deciseconds). After a reboot the ring's ds restarts low, so a
cursor left at the old (high) value matches nothing and sync silently returns 0 events
until the cursor is reset. A robust sync should detect "0 events but a from-0 probe has
data below the cursor" and rebase the cursor.
