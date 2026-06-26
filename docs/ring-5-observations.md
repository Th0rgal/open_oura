# Oura Ring 5 Observations

First-contact BLE findings for the Oura Ring 5, captured on 2026-06-21 in Lisbon
with the paired phone's Bluetooth disabled. macOS CoreBluetooth hides the real BLE
MAC, so the identifiers below are macOS peripheral UUIDs unless noted.

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

## Open items specific to Ring 5

- Characterise the roles of the extra `…0004/0005/0006` characteristics.
