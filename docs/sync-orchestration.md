# Sync Orchestration

How the Oura Android app decides *when* to use each ring data channel, from the
decompiled state machine (`com/ouraring/oura/data/device/ring/`). This is the
behavior an independent client should replicate.

## Channels and when each fires

| Channel | Wire | When the app uses it | Needed to mirror app data? |
| --- | --- | --- | --- |
| History events (NORMAL buffer) | `GetEvent` 0x10 / `ExtGetEvent` 0x2f, bufferId 0 | every sync (connect, foreground, background) | yes -- this is the whole game |
| Live / realtime | `SetFeatureMode CONNECTED_LIVE`, realtime 0x06 | only while a specific UI screen is open | only for a live HR readout |
| RData bulk raw | `RDataStart`/`GetPage` 0x03, RAW_DATA buffer | never by default (`r_data_autosync`=false) | no, unless you want raw waveforms |

Everything the user sees (sleep stages, last-night HR/HRV/SpO2, MET/steps) flows
through the history-event drain. RData is a research/diagnostics opt-in.

## Connect handshake (ordered)

Driven by `RingStateMachine` / `DefaultRingStateMachine$Operations`
(states: CONNECTING -> AUTHENTICATING -> CHECK_CAPABILITIES ->
APP_LEVEL_AUTHENTICATING -> FOREGROUND_SYNC | BACKGROUND_SYNC):

1. CONNECT (BLE connect + bond)
2. AUTHENTICATE (nonce -> AES -> `Authenticate`)
3. GET_CAPABILITIES (decides extended vs legacy event sync)
4. APP_LEVEL_AUTHENTICATE
5. SYNC_TIMESTAMPS (`SyncTime`, write phone UTC)
6. ENABLE_NOTIFICATION (`SetNotification`)
7. BATTERY_LEVEL / PRODUCT_INFO / RING_VERSION (metadata)
8. feature ENABLE_* toggles (conditional on capabilities + user flags)
9. SYNC_EVENTS (main drain)
10. SYNC_R_DATA (only if `r_data_autosync`)

Load-bearing for a client: time-sync, notification-enable, and capabilities all
precede event sync.

## Event-drain loop

```
cursor = store.get_next_event_to_sync()        # deciseconds (100 ms units)
loop:
    if extended_supported:
        summary = ExtGetEvent(start_ms = cursor*100, max_events = 65535, buffer = NORMAL)
    else:
        summary = GetEvent(start_deciseconds = cursor, max_events = 255, flags = -1)
    decode + persist events from the response
    if summary.events_received > 0:
        cursor = cursor + 1
        store.set_next_event_to_sync(cursor)    # incremental-sync bookmark
    if summary.bytes_left > 0:                   # ring has more data
        repeat
    else:
        done
```

The ring reports `bytes_left` in the `0x11` / extended confirmation packet; loop
until it reaches 0. The persisted cursor (`nextEventToSync`) makes sync
incremental. `sleepAnalysisProgress` is surfaced as progress only, not a block.

## Scheduling

- On connect: full handshake then SYNC_EVENTS automatically.
- Foreground: user-triggered `triggerForegroundSync()`.
- Background: on app backgrounded.
- A small periodic worker refreshes battery only; the real data sync is
  connection-/lifecycle-triggered, not a fixed timer.

## Gating

- Skips if a sync is already ongoing.
- Routes around *_SYNC during onboarding.
- No hard low-battery block on the event path; RData (heavier) is the gated one.

## Minimal client sync recipe

1. Connect + bond; subscribe to the notify characteristic.
2. Authenticate with the stored 16-byte key.
3. GetCapabilities -> choose extended vs legacy event path.
4. SyncTime + SetNotification.
5. (optional) firmware / product / battery for metadata.
6. Drain history events from the persisted cursor; persist each event + advance
   the cursor; stop when `bytes_left == 0`.

Do not issue any RData (0x03) for a normal pull.
