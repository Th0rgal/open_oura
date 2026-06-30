# Oura Ring 5 Observations

First-contact BLE findings for the Oura Ring 5, captured on 2026-06-21 in Lisbon
with the paired phone's Bluetooth disabled. Follow-up protocol probes were run on
2026-06-30 against the same Ring 5 after comparing `LogosIsLife/open_ring`.
macOS CoreBluetooth hides the real BLE MAC, so the identifiers below are macOS
peripheral UUIDs unless noted.

The takeaway: Ring 5 uses the **same** GATT layout, framing, and app-auth flow as
the Ring 3 (see `horizon-ring3-protocol-cheatsheet.md`) and the ringverse Ring 4
notes - so the client is shared across generations. The differences are additional
characteristics and a larger MTU.

Device details (from the Oura app):

- Model: Oura Ring 5 · Serial `YYYYYYYYYYYYYYYY` · BLE MAC `11:22:33:44:55:66`

## Advertisements

- Ring: name `Oura Ring 5`, service `98ed0001-a541-11e4-b6a0-0002a5d5c51b`,
  manufacturer data `02b2:04766b01`.
- Charging case: name `Oura Ring 5 Charging Case`, service
  `8bc5888f-c577-4f5d-857f-377354093f13`, manufacturer data `02b2:04a00b00`.

Connecting via a `BLEDevice` from the live scan was reliable; connecting by a
previously observed macOS UUID string was not (the UUID changes between scans, and
the ring connects more readily while on its charger).

## GATT surface

- MTU `247` (vs `203` on Ring 3).
- Service `98ed0001-a541-11e4-b6a0-0002a5d5c51b`:
  - `…0003` read,notify - responses / notifications
  - `…0002` write - protocol requests
  - `…0004` read,write,notify,indicate - additional (not on Ring 3)
  - `…0005` write,notify - additional
  - `…0006` write,notify - additional

The client subscribes to every notify/indicate characteristic in the service, so
the extra Ring 5 characteristics are handled automatically.

## Framing Detail

Control responses use the same `tag | length | payload` framing as Ring 3/4.
Ring 5 history notifications can contain **multiple framed events concatenated in
one BLE notification**. A parser must walk the entire notification, consuming
`2 + length` bytes repeatedly; parsing only the first `tag|len` frame drops most
events in a packed notification.

Ring 5 history events observed through `GetEvent` still use the local
`tag | length | <u32 ring timestamp> | body` envelope. The Ring 4 `open_ring`
driver describes this as an inner record stream with `(counter, session)` fields;
for Ring 5 the same four bytes are treated as the monotonic ring timestamp cursor
by this project.

## First active probes (ring on charger, not worn)

- **Firmware** `0803000000` → `0912020100020103010001090329665544332211`:
  API `2.1.0`, firmware `2.1.3`, bootloader `1.0.1`, BT stack `9.3.41`, MAC
  `11:22:33:44:55:66`. Readable **without** app authentication.
- **Battery** `0c00` → `2f022f01` (`auth_state=0x01`): auth-gated, as on Ring 3.
- **Auth nonce** `2f012b` → `2f102c490a55be3b8169e3f24aa279f1e55a`: same nonce
  challenge shape as the Ring 4 notes and the decompiled app
  (see `android-app-reversing.md`).

## Status

Ring 5 has since been factory-reset and paired with a client-generated key
(`oura pair`), and the full flow is verified: app-auth, battery, feature enable,
event sync, and live ACM (~50 Hz). With a key installed, control commands
(battery, features, realtime) require per-connection auth, while firmware and
serial still read unauthenticated. Connect from a fresh scan and keep the ring on
its charger for reliable advertising.

## 2026-06-30 `open_ring` Gap Probes

Tested with `captures/ring5.key` and captured in ignored
`captures/ring5-openring-gap-probes.jsonl`. Destructive commands such as factory
reset, DFU reset, soft reset, and flight mode were **not** run.

Confirmed accepted on Ring 5:

- App-style time sync shape: `12 09 <token> <unix/256:u24 LE> 00 00 00 00 f6`.
  The zero-counter probe wrote successfully; the Rust library now exposes
  `req_sync_time_counter` and `OuraClient::sync_time_app`.
- Stream registration: `16 01 02`.
- Event category subscription shapes:
  `18 03 <category> <flags:u16 LE>`; the app-observed categories are
  `(0x14,0x1000)`, `(0x18,0x1000)`, `(0x28,0x0900)`, `(0x34,0x0400)`,
  `(0x04,0x1000)`, `(0x08,0x1000)`.
- App setup parameter sweep:
  `2f022002`, `2f022004`, `2f020301`, `2f02200b`, `2f02200d`, `2f022003`,
  `2f02200b`, `2f022010`.
- Data flush / sleep-analysis opcode shape: `28 01 00` ACKed with `29 01 00`
  and released buffered history events.
- History ack-fetch shape: `10 09 <cursor:u32 LE> 00 ff ff ff ff`. When sent
  from cursor `0`, the ring still streamed buffered history; use the current
  max cursor, not zero, for a real acknowledgement.
- DHR burst parameter writes: `2f03220203` and `2f03260202` ACKed and caused
  dense HR/CVA-related event output. The ring was restored afterward; follow-up
  `feature-status` showed daytime HR back at `AUTOMATIC` and subscription `0`.

Code changes from these probes:

- `Packet::parse_many` now parses every framed packet in one notification.
- `OuraClient::setup_app_stream` runs the confirmed app-style stream/category
  registration and parameter sweep before sync.
- Event draining now sends `req_data_flush()` before fetches and
  `req_get_event_ack(cursor)` after successful progress.
- CVA raw PPG (`0x81`) now treats `0x80` as a 24-bit absolute-sample marker
  instead of a normal signed delta byte.
- RTC beacon (`0x85`) is named and decoded as a 1-second wall-clock anchor.

## Open items specific to Ring 5

- Characterise the roles of the extra `…0004/0005/0006` characteristics.
- Implement a stateful streaming CVA decoder that carries the `0x81` accumulator
  across adjacent records. The current event-body decoder is correct for
  per-record absolute markers but intentionally stateless.
