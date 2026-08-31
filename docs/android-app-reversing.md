# Android App Reverse Engineering Notes

Source APK bundle:

- Package: `com.ouraring.oura`
- Download source used locally: `apkeep -d apk-pure`
- XAPK manifest version: `7.15.0`, version code `260521094`
- Current Google Play listing observed on 2026-06-21: `7.18.0`

The APKPure source returned an older bundle than the current Play listing, but
the BLE/auth code is first-party `ourakit` code and matches the live Ring 5
behavior observed locally.

## Tools

```bash
brew install jadx apktool apkeep
apkeep -a com.ouraring.oura -d apk-pure reverse/android
unzip reverse/android/com.ouraring.oura.xapk -d reverse/android/xapk
apktool d -f -o reverse/android/apktool-main reverse/android/xapk/com.ouraring.oura.apk
jadx -d reverse/android/jadx-main reverse/android/xapk/com.ouraring.oura.apk
```

The decompiled APK/XAPK outputs are intentionally ignored by git.

## BLE Constants

From `com/ouraring/ourakit/internal/Constants`:

- Ring service: `98ED0001-A541-11E4-B6A0-0002A5D5C51B`
- Read/notify characteristic: `98ED0003-A541-11E4-B6A0-0002A5D5C51B`
- Write characteristic: `98ED0002-A541-11E4-B6A0-0002A5D5C51B`
- Charger service: `8BC5888F-C577-4F5D-857F-377354093F13`
- Oura manufacturer ID: `0x02b2`

## Auth Operations

The app has explicit first-party operation classes under
`com/ouraring/ourakit/operations`.

### GetAuthNonce

Request construction:

- Tag: `0x2f`
- Length: `0x01`
- Extended request tag: `0x2b`
- Hex request: `2f012b`

Response parsing:

- Outer response tag must be `0x2f`
- Extended response tag must be `0x2c`
- Nonce bytes are copied from response indexes `[3, 18)`, so the app expects a
  15-byte nonce.

### Authenticate

Request construction:

- Tag: `0x2f`
- Length: `0x11`
- Extended request tag: `0x2d`
- Payload: `0x2d` followed by 16-byte encrypted nonce
- Hex shape: `2f112d <16 encrypted bytes>`

Response parsing:

- Outer response tag must be `0x2f`
- Extended response tag must be `0x2e`
- Auth result is response byte index `3`

### SetAuthKey

Request construction:

- Tag: `0x24`
- Length: `0x10`
- Payload: 16-byte key
- Hex shape: `2410 <16 key bytes>`

Response parsing:

- Response tag: `0x25`
- Result byte: response index `2`
- `0x00` is success
- `0x05` is treated specially as production tests missing during setup

## Key Generation

Production auth keys are generated locally by `RingOperations.i()[B`:

1. `UUID.randomUUID()`
2. Allocate 16 bytes
3. Wrap with `ByteBuffer`
4. Set byte order to little endian
5. Write UUID most-significant bits as `long`
6. Write UUID least-significant bits as `long`

In non-production app builds, the method returns a static test key from
`com/ouraring/core/features/ringconfiguration/r0`.

## Nonce Encryption

Both ring and charger auth paths use:

- Algorithm: `AES`
- Cipher: `AES/ECB/PKCS5Padding`
- Key: stored 16-byte `authKey`
- Input: 15-byte nonce from `GetAuthNonce`
- Output: 16-byte encrypted nonce passed to `Authenticate`

This matches the Ringverse Ring 4 notes and explains why a 15-byte nonce becomes
a 16-byte encrypted payload.

## Key Storage

The key is stored in Realm model fields:

- Ring: `DbRingConfiguration.authKey`, serialized as `auth_key`
- Charger: `DbAccessoryConfiguration.authKey`, serialized as `auth_key`

The ring authenticate path reads `DbRingConfiguration.getAuthKey()`, pairs it
with the nonce, encrypts the nonce, and executes `Authenticate`.

## Current Implication

For a ring already onboarded to another device, our Mac can connect and read
firmware metadata, but app-gated requests such as battery require the existing
16-byte `auth_key`. To authenticate without resetting/re-onboarding the ring, we
need to extract `auth_key` from an existing Oura app database or capture it
during a fresh onboarding flow.

## Android Extraction on a Rooted Phone

On a rooted Android phone the official app database can be inspected directly.
The Oura package may live in a secondary Android user/profile, not necessarily
`/data/data`; on the local Pixel 8 it was under Android user `10`:

```bash
adb devices -l
adb shell 'su -c id'
adb shell 'pm list users'
adb shell 'pm list packages | grep oura'
python3 tools/android_oura_key_extract.py --serial 50380B2617647259
```

The helper pulls the app log and `assa-store.realm` into `captures/android/`
with `0600` permissions. It does not print auth keys. Current Oura Android
builds store `DbRingConfiguration.auth_key` as binary Realm data, not as a
printable 32-character hex string. If the helper can identify exactly one key it
writes `captures/android/oura-android-auth.key`; otherwise it writes binary
candidates to `captures/android/oura-android-auth-candidates.txt` for BLE
nonce/auth verification.

### Local Pixel 8 result, 2026-07-01

The official app data was present at:

- `/data/user/10/com.ouraring.oura`
- Realm: `/data/user/10/com.ouraring.oura/files/oura/55775a50-c3fd-40f5-ba14-0a5eb68e7756/assa-store.realm`

The first pull only contained account metadata, not a local paired ring config:

- `RingConfigurationObserver new emission null`
- `hasRingConfiguration: false`
- `No paired Ring (no bond address)`
- `hasActiveRings=false`
- scanner started and finished with no devices

That state has no local Android `auth_key` to share. This is expected when the
account has ring metadata but the phone has not locally paired/bound to the ring
over BLE.

After restoring phone internet, factory-resetting the ring from the Mac, and
onboarding it in the official Android app, the app reached local ring
configuration:

- Serial: `50380B2617647259`
- Hardware type reported by the scanner: `COOPER`
- Bond address: `59:27:76:8D:59:13`
- App-level MAC/private identifier in logs: `C9BCA25DAC56`
- Log state: `AUTHENTICATING -> CHECK_CAPABILITIES -> APP_LEVEL_AUTHENTICATING`
- Launcher state: `hasRingConfiguration: true`

A post-onboarding backup was saved locally as
`captures/android/oura_post_onboarding_ce_20260701-220348.tar`. A later fresh
pull from 2026-07-02 opened cleanly with Realm JS and exposed two
`DbRingConfiguration` rows. The active Ring 5 row was identified by:

- `serial_number = 50380B2617647259`
- `mac_address = C9BCA25DAC56`
- `android_ble_identifier = 5927768D5913`
- `auth_key` length: 16 bytes

That active row's auth key was written to
`captures/android/oura-android-auth.key` with `0600` permissions. The key is not
printed in logs/docs. Mac-side verification is still blocked: after disabling
Android Bluetooth, the Ring 5 advertised strongly to the Mac around `-59 dBm`,
but CoreBluetooth timed out connecting to
`65C8DB8E-A76D-3273-F46E-91B425558B6D`.

Reproducible direct extraction from a pulled Realm file:

```bash
cd /tmp/openoura-realm-read
npm install realm@20
/Users/thomas/work/open_oura/tools/extract_oura_realm_key.js \
  /Users/thomas/work/open_oura/captures/android/current/assa-store.realm \
  50380B2617647259 \
  /Users/thomas/work/open_oura/captures/android/oura-android-auth.key
```

The official app ANR observed after onboarding was not an APK mismatch. The ANR
trace `captures/android/anr_2026-07-01-21-00-50-929.txt` shows
`DashboardActivity` input dispatch timed out while Oura worker threads were
busy in Realm query/upload paths, especially `AppServerSyncUploader` and
timeseries upload flows. App logs then show a foreground BLE operation timeout:
`SetRealtimeMeasurements` timed out after 60 seconds. Force-stopping and
relaunching the app displayed `DashboardActivity` in about 0.5 seconds and did
not immediately reproduce the ANR.

### Invasive Android patch attempts

Frida was tested first because it can patch `RxBleRingBondConnector` at runtime
without replacing the APK. It is not currently viable against this app build:
attaching Frida 17.15.3 to `com.ouraring.oura` caused a native `SIGBUS` in the
Frida agent path while Crashlytics/Sentry signal handlers were loaded.

Static patching confirmed the important app gates:

- `RxBleRingBondConnector.isBonded(String)` can be patched to return `true`.
- `RxBleRingBondConnector.connect(String, Function1)` can be patched to emit
  `RingBondSuccess` immediately.
- `RingOperations.i()[B` is the production key generator called immediately
  before `SetAuthKey`; replacing this output with a captured local key is the
  right direction for an already-onboarded ring, because generating a fresh key
  cannot match the key already installed on the ring.

Two APK replacement strategies were tested:

- Root-copying a full apktool rebuilt `base.apk` into `/data/app/...` launched
  far enough to load code but crashed on rebuilt resource IDs.
- Root-copying only patched `classes5.dex`/`classes6.dex` into the original
  `base.apk` also crashed before app init on this Android build.

A complete split reinstall with a local test signature was also tested. It can
be installed with `adb install-multiple --no-incremental`, but the patched build
still failed before app init with `ClassNotFoundException:
com.ouraring.oura.App`. The official split set was restored afterwards, and the
app was verified running again after restoring data ownership and SELinux MCS
labels.

Current conclusion: the ring-side hypothesis is unchanged: a paired second app
must reuse the existing 16-byte app auth key, not generate a new one. After
factory reset and official Android onboarding, the key can be extracted from
Realm directly; static APK replacement is no longer the preferred route. The
remaining blocker is Mac BLE connection reliability, not key extraction.

To refresh the pulled Android artifacts, rerun:

```bash
python3 tools/android_oura_key_extract.py --serial 50380B2617647259 \
  --key-out captures/android/oura-android-auth.key
cargo run -p oura-cli -- --key-file captures/android/oura-android-auth.key info
```

If that key authenticates on the Mac, replace or import the local key file used
by the dashboard/iOS app. Keep the old Mac key backed up until the Android key
has been verified.

## Live Ring 5 Verification

Captured on 2026-06-21 against the local Ring 5:

- Request: `2f012b`
- Response: `2f102c490a55be3b8169e3f24aa279f1e55a`
- Parsed:
  - Outer tag: `0x2f`
  - Length: `0x10`
  - Extended response tag: `0x2c`
  - Nonce: `490a55be3b8169e3f24aa279f1e55a`

This confirms Ring 5 uses the same app-level nonce challenge shape as the
decompiled Android app and the Ringverse Ring 4 notes.

During scanning, another nearby Oura device also appeared:

- Name: `Oura XXXXXXXXXXXXXX`
- It rejected notification/write attempts with CoreBluetooth
  `Encryption is insufficient`.

The local probe defaults to a `Ring 5` name filter to avoid accidentally probing
other nearby Oura devices.
