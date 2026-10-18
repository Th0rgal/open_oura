# Readiness history inputs

`oura-analysis::ported::readiness_history::prepare` ports the Android 7.26.0
(260930084) `bxl.a` history mapper. It converts the original complete caller-order
sleep and activity lists into the six newest-first arrays supplied to daily
readiness: three temperature deviations, 90 highest temperatures, and 14 entries
each of total sleep, walking distance, wear percentage and RMSSD.

## Input contract

The source lists must already be selected and calendar-aligned by the caller's
history queries, including the original missing-day placeholder rows. These are
not simply the latest 90 observed nights. At least 90 sleep and 15 activity rows
are required; incomplete input fails rather than silently fabricating history.
The mapper reverses both lists. Extra older entries are not consumed. The
newest activity is skipped before filling the 14-day arrays, preserving the
sleep/activity day offset used by the original factory.

`SleepRow::timestamp` and the optional `ring_setup_at` are untouched source Long
values in the same source unit. Do not replace them with an inferred bedtime,
saved day or a newly generated timestamp. The setup threshold gates temperature
history only, with equality accepted. It does not zero old sleep duration or HRV.

Missing temperature deviations use `-32768`; missing highest temperature,
duration and HRV use zero. Temperatures are converted from Celsius using the
original Kotlin `Double.roundToInt`: half ties toward positive infinity, Int
saturation, and an error on NaN. Only the first three deviation fields are read,
while highest temperature is read for all 90 admitted rows.

Activity durations are source seconds, converted through the original Kotlin
Duration behavior to minutes and narrowed to Int. The wear calculation preserves
native unsigned arithmetic and Float operation order, then JNI/Java byte
narrowing. Other Long-to-Int fields also preserve their low 32 bits. The output
keeps signed Java representations instead of applying physiological clamps.

## Using it with the metric ports

The reviewed [#32](https://github.com/Th0rgal/open_oura/pull/32) head,
`514acc806734849859aeacf30bb69f2ff1e7644e`, already accepts all six arrays in
`readiness_inputs::History`. Its daily convenience wrapper supplies missing
temperature deviations and a zero-filled 90-day temperature array. This mapper
provides the recovered conversion from actual materialized source history.

For that API, copy `temperature_deviations` to `temp_dev_3`; reinterpret the low
32 bits of the highest-temperature, sleep, distance and RMSSD values for its
unsigned fields, and the low eight bits of signed wear values for `wear_14`.
For example, signed wear `-117` carries the same byte as unsigned `139`.
Do not replace negative fields with zero while adapting the signed arrays.

This complements the per-update record selector in
[#33](https://github.com/Th0rgal/open_oura/pull/33). That selector chooses inputs
for one baseline update; this factory constructs the longer readiness histories.
Neither selects the actual account configuration or changes existing callers.

The port is checked against recorded synthetic original-Android factory outputs,
including nullable fields, setup boundaries, NaN errors, narrowing and duration
saturation. Focused inline unit checks follow the crate's existing style; full
reference captures stay local. Query/materialization, daily-scorer wiring,
persistence and real multiday readiness parity remain integration work.
