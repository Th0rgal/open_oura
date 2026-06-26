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
