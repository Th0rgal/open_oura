# Open Oura — native iOS app

A native SwiftUI app that talks to the Oura ring directly over Bluetooth (no Oura
cloud), reusing the project's Rust protocol core. Tabs: **Today** (resting HR / HRV
/ temp / battery + history), **Live** (real-time HR / HRV / motion), **Sleep**
(on-device hypnogram + stage distribution), **Settings** (key + connection).

## Architecture

```
SwiftUI views  ──►  OuraRing (CoreBluetooth + protocol orchestration, Swift)
                         │  request/response, auth, live drain  (OuraProtocol.swift)
                         ▼
                    OuraFFI.xcframework  ──►  crates/oura-ffi (Rust staticlib)
                         (C ABI: AES auth + event decoders)   reuses oura-protocol
```

The genuinely hard, byte-level parts — AES-128/ECB/PKCS7 auth and the event-body
decoders — are the tested Rust from `crates/oura-protocol`, exposed over a tiny C
ABI (`crates/oura-ffi`). BLE transport, packet framing, request builders, and the
connect/sync/live orchestration are native Swift. Nothing async crosses the FFI
boundary.

## Build the Rust core (run after any Rust change)

```bash
./ios/build-rust.sh          # builds OuraFFI.xcframework (device + simulator)
```

## Generate & open the Xcode project

```bash
brew install xcodegen        # one-time
cd ios && xcodegen generate
open OpenOura.xcodeproj
```

Set `DEVELOPMENT_TEAM` in `project.yml` to your Apple Developer Team ID (Xcode →
Settings → Accounts → your team). A free "Personal Team" works for development; a
paid team avoids the 7-day app expiry. Find your id with:
`security find-identity -v -p codesigning` or in Xcode's Signing settings.

## Run on a device

The iOS Simulator has **no Bluetooth**, so the ring features only work on a real
device. Build/install from the CLI:

```bash
xcodebuild -project ios/OpenOura.xcodeproj -scheme OpenOura \
  -destination 'id=<your-iphone-udid>' -allowProvisioningUpdates \
  build
```

…or just press ⌘R in Xcode with your iPhone selected.

## First use

The app shows a **connection guide** on launch (`ConnectGuideView`) — reopen it any
time by tapping the status banner or Settings → "Connection guide".

1. Pair once (either path):
   - **In-app:** factory-reset the ring, then **Pair ring** (generates + installs a
     key, stores it in the Keychain) — the real-app flow.
   - **Import:** if already paired via the CLI (`oura pair`), Settings → "Import
     existing key", paste `cat key.hex`.
2. **Put the ring on its charging pad, next to the phone**, then **Connect**.
3. Use **Live** (wear the ring) / **Sync history**.

### Connecting gotchas (learned the hard way)

- **The ring must be awake to connect.** Off the charger and not worn, it drops into
  deep-sleep advertising, so iOS sees a stray advert ("discovered") but `connect()`
  hangs with no `didConnect`. **On the charging pad (or worn) it's actively
  connectable.** This is the #1 cause of "connect doesn't work".
- **One central at a time.** If this Mac has the ring bonded, macOS auto-reconnects
  it the moment it advertises, stealing the slot from the phone. For phone testing,
  turn the Mac's Bluetooth off (`blueutil -p 0`) or "Forget" the ring on the Mac.
  Re-enable with `blueutil -p 1` to use the CLI again.
- Quit the CLI `live`/`viz` before connecting from the phone, and vice-versa.
