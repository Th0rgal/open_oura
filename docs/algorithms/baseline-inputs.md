# Baseline initialization and saved-history inputs

The baseline update math needs the actual initialized limits and the right
previous records. `oura-analysis::ported::baseline` now exposes the recovered
age curves, and `ported::baseline::history` selects the saved records that the
Android host supplies before a long-term update. Both are portable Rust with no
database, native-library or model dependency.

## Age and feature-dependent sleep duration

Source: Oura Android 7.26.0 (260930084), `libappecore.so`:

- Initial mean getter at ELF `0xe2340`: feature 27 off uses
  `round(486 - 1.77 * age + 20) * 8`; on uses
  `round(468 - 0.87 * age) * 8`.
- Acceptable minimum getter at `0xe23b0`: feature 27 off uses
  `trunc((486 - 1.77 * age) * 8)`; on uses
  `trunc((408 - 0.87 * age) * 8)`.

The implementation preserves the fused floating-point operation and different
rounding positions. At age 30 the initial mean/minimum are `3624/3463` with the
feature off and `3536/3055` with it on. These are minutes multiplied by eight.
The native lower-limit table stores the minimum narrowed to signed 16 bits.

The caller must supply the account's actual feature-27 setting. The initial
mean belongs to a newly initialized sleep-duration baseline; it must not replace
an already accumulated mean on every night. The minimum configures the
sleep-balance item (index 1), independently of the previous-night item (index 0).
These helpers do not initialize all nine items or choose other feature flags.

## Saved records used before an update

Source: the Android 7.26.0 `x7h` host selection and its saved baseline,
longest-sleep and previous-activity queries. `baseline::history::select_update`
applies these decisions in order:

1. Select the latest baseline strictly before the current sleep day with the
   same persisted sleep-algorithm version. An absent current version defaults
   to `nssa`. On equal days the last encountered baseline wins. With no match,
   the host creates a default baseline for the current day.
2. Select the latest preceding day containing a `long_sleep`, then its longest
   sleep. Equal durations keep the first encounter; every nonnull duration
   sorts before null, even `i64::MIN`. This query does not filter sleep by the
   baseline's algorithm version.
3. Require that sleep's highest temperature. Do not skip it in favor of an
   older sleep merely because the older one has a temperature.
4. Reject a baseline whose day is strictly newer than the selected sleep day.
   An equal day is allowed by this gate.
5. Require activity on the calendar day immediately before the selected sleep.
   Select its greatest saved timestamp, preserving first encounter on ties.

The public result distinguishes every admission boundary and returns indices
into the input slices. It performs no mutation. Callers must project **active**
saved rows in their original encounter order and supply validated saved calendar
days as signed days since 1970-01-01. Project canonical saved `YYYY-MM-DD` dates;
this typed interface does not reproduce lexical ordering of noncanonical date
strings. These values are not timestamps: do not
infer the day from bedtime or shift it using the machine's current timezone.
Calendar gaps stay gaps. The selector does not create missing activity or
temperature, reset history, run the numeric update or persist its result.

## Relationship to the pending metric ports

This complements [#30](https://github.com/Th0rgal/open_oura/pull/30) and
[#32](https://github.com/Th0rgal/open_oura/pull/32), which provide the configured
numeric update. The latter's reviewed head, `514acc806734849859aeacf30bb69f2ff1e7644e`,
uses a fixed age-28 sleep-balance minimum in its daily wrapper. Its lower-level
updater already accepts a limit table; callers can put the actual age/feature
minimum in that table and use the selector's admitted records. No replacement
update formula or automatic change to existing callers is introduced here.

Recorded synthetic Android controls cover both age curves and the saved-query
ordering boundaries. The small unit checks follow the crate's existing inline
test style. Full application dispatch, persistence, ring-reset policy and final
readiness-score parity remain separate integration work.
