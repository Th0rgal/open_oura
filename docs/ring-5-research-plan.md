# Oura Ring 5 Research Plan

## Questions

- What BLE services and characteristics does Ring 5 expose before pairing,
  after pairing, and while the official app is closed?
- Does Ring 5 still use the tag/length/payload framing described by ringverse
  for Ring 4?
- Are the known Ring 4 read-only commands accepted by Ring 5?
- What authentication or nonce flow gates user data access?

## Initial passive capture

1. Scan nearby BLE advertisements and identify the ring name/address.
2. Connect and enumerate services and characteristics.
3. Subscribe to notify characteristics without sending protocol commands.
4. Record handles, UUIDs, properties, MTU, and notification payloads.

## First Ring 5 observations

Captured on 2026-06-21 in Lisbon after disabling Bluetooth on the paired phone.
macOS CoreBluetooth does not expose the real BLE MAC address, so the addresses
below are macOS peripheral UUIDs rather than the ring MAC.

Known device details from Oura app:

- Model: Oura Ring 5
- Serial: `50380B2617647259`
- BLE MAC: `c9:bc:a2:5d:ac:56`

Advertisements observed:

- Ring:
  - macOS UUID: `F928A493-157D-B2B5-0D19-F43F8DB5680E`
  - Name: `Oura Ring 5`
  - RSSI: `-81`
  - Service UUID: `98ed0001-a541-11e4-b6a0-0002a5d5c51b`
  - Manufacturer data: `02b2:04706b01`
- Charging case:
  - macOS UUID: `724CE68A-F69F-B641-B08E-DD251A0EF3F9`
  - Name: `Oura Ring 5 Charging Case`
  - RSSI: `-84`
  - Service UUID: `8bc5888f-c577-4f5d-857f-377354093f13`
  - Manufacturer data: `02b2:04a00b00`

Direct service enumeration attempts against both macOS UUIDs timed out with
Bleak/CoreBluetooth. Next steps are to repeat while the ring is on the charger
and the phone Bluetooth remains off, then try a longer connection timeout.

After placing the ring on the charger, the ring re-advertised with a different
macOS UUID:

- macOS UUID: `5B1AAA7A-7FC7-815D-873F-95FFD7E184B7`
- Name: initially empty, then `Oura Ring 5`
- RSSI: `-66` to `-68`
- Service UUID: `98ed0001-a541-11e4-b6a0-0002a5d5c51b`
- Manufacturer data: `02b2:04766b01`

Connecting by a `BLEDevice` returned from the same Bleak scan succeeded, while
connecting by a previously observed macOS UUID string was unreliable.

Enumerated GATT surface:

- MTU: `247`
- Service `98ed0001-a541-11e4-b6a0-0002a5d5c51b`, handle `16`
  - Characteristic `98ed0003-a541-11e4-b6a0-0002a5d5c51b`, handle `17`,
    properties `read,notify`, CCCD handle `19`
  - Characteristic `98ed0002-a541-11e4-b6a0-0002a5d5c51b`, handle `20`,
    properties `write-without-response,write`
  - Characteristic `98ed0004-a541-11e4-b6a0-0002a5d5c51b`, handle `22`,
    properties `read,write-without-response,write,notify,indicate`, CCCD handle
    `24`
  - Characteristic `98ed0005-a541-11e4-b6a0-0002a5d5c51b`, handle `25`,
    properties `write-without-response,notify`, CCCD handle `27`
  - Characteristic `98ed0006-a541-11e4-b6a0-0002a5d5c51b`, handle `28`,
    properties `write-without-response,notify`, CCCD handle `30`

Read-only characteristic reads returned empty values:

- `98ed0003-a541-11e4-b6a0-0002a5d5c51b`: empty
- `98ed0004-a541-11e4-b6a0-0002a5d5c51b`: empty

## First active probes

Captured on 2026-06-21 with phone Bluetooth off and the ring on its charger.
The client subscribed to all notify-capable characteristics, then wrote requests
to `98ed0002-a541-11e4-b6a0-0002a5d5c51b`.

### Battery request

- Request: `0c00`
- Response characteristic: `98ed0003-a541-11e4-b6a0-0002a5d5c51b`
- Response: `2f022f01`
- Parsed: extended/auth response, `auth_state=0x01`

This did not return the expected Ring 4 battery response tag `0x0d`. Based on
the Ring 4 notes, `auth_state=0x01` means authentication error, so battery data
appears to be auth-gated on this Ring 5 state.

### Firmware request

- Request: `0803000000`
- Response characteristic: `98ed0003-a541-11e4-b6a0-0002a5d5c51b`
- Response: `091202010002010301000109032956ac5da2bcc9`
- Parsed:
  - Tag: `0x09`
  - Length: `18`
  - API version: `2.1.0`
  - Firmware version: `2.1.3`
  - Bootloader version: `1.0.1`
  - Bluetooth stack version: `9.3.41`
  - MAC bytes: `56 ac 5d a2 bc c9`, matching real BLE MAC
    `c9:bc:a2:5d:ac:56` when reversed

Firmware metadata is readable without completing app authentication.

## Low-risk active probes

These should only be attempted after service discovery confirms the likely
control characteristic.

- `0C00` - Get battery level.
- `0803000000` - Get firmware version.
- Product info reads with known Ring 4 request shapes.

Avoid reset, DFU, factory reset, flight mode, auth mutation, and user-info writes
until the Ring 5 protocol is better understood.

## Capture format

Each experiment should record:

- Date, timezone, OS, Bluetooth adapter, ring firmware if known.
- Pairing state and whether the official Oura app was running.
- Command hex, characteristic UUID/handle, response hex, and timing.
- Any visible side effect on the ring or the Oura app.
