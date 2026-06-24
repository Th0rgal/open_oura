# Firmware Update (DFU/OTA)

How the Oura app updates ring firmware, and whether a custom image could be
flashed. Derived from the decompiled app (`com.ouraring.ourakit.firmware` and
`operations/DFU*`). All multi-byte fields are little-endian.

There are two separate DFU paths:

- **Path A - Oura-protocol DFU** (modern rings): tags `0x0E` + `0x2B` over the
  normal Oura GATT link, orchestrated by `firmware/RingAPIDFU.java`.
- **Path B - Cypress/PSoC bootloader DFU** (legacy rings): the stock Infineon
  CYACD2 bootloader on a dedicated service `00060000-f8ce-11e4-abf4-0002a5d5c51b`
  (`firmware/cypress/CypressFirmwareUpdateService.java`).

## Where the image comes from (cloud OTA download)

Before any BLE flashing, the app fetches the image from Oura's cloud. The whole
chain is **account-authenticated** - every call carries
`Authorization: Bearer <accessToken>` injected for the `@com.ouraring.core.network.a`
annotation from `AccessTokenModel.getAccessTokenBearer()`. That bearer is the Oura
**account** OAuth token (MOI/OAuth login, stored in encrypted prefs) - *not* the
ring BLE auth key. Host is `api.ouraring.com` (`Endpoint.PRODUCTION.url`).

1. **Discover available packages** - `ClientConfigurationService.downloadConfig(...)`
   (authenticated) returns a `ClientConfiguration` whose two relevant fields are:
   - `firmware_updates`: `List<FirmwareLauncherUpdate{hardware_type, type, version}>`
     where `hardware_type ∈ {GEN2, GEN2M, GEN2X, GEN4, NOMAD}` and
     `type ∈ {APPLICATION, BOOTLOADER}` - i.e. "what version is current for your ring".
   - `ota_files` (JSON `ota_files`): `List<OtaDescriptor{type, version, slug}>` - the
     concrete packages to fetch. `type` is a `PackageType.safeName` (table below).
2. **Get the manifest** - `OtaPackageService.getOtaManifest(type, version, slug)`:
   `GET /api/v2/file/{type}/{version}/{slug}` → `OtaPackageManifest`:
   ```
   { type, version, slug, filename, md5, sha256, uploaded_at, size, url }
   ```
3. **Download the bytes** - `OtaPackageService.downloadOtaPackage(@Url url)`:
   `GET <manifest.url>` with `Content-Type: application/octet-stream` →
   `ResponseBody` (the raw CYACD2 / binary OTA file documented below). `url` is an
   absolute (CDN/signed) URL from the manifest. The download is integrity-checked
   against `md5`/`sha256`/`size`, then handed to `DFUProvider.startFirmwareUpdate(
   address, firmwarePath, firmwareType)` for the BLE flash. Orchestration lives in
   `com/ouraring/oura/otapackages/` (`OtaPackageManager`/`l.java`); the retrofit
   service is `com/ouraring/oura/model/backend/OtaPackageService.java`.

`PackageType.safeName` values (the `{type}` path segment):

| safeName | enum constant | notes |
| --- | --- | --- |
| `bootloader_gen2` / `_gen2m` / `_gen2x` / `_oreo` | BootloaderGen2/2m/2x/**Gen4** | bootloader images |
| `firmware_gen2` / `_gen2m` / `_gen2x` | FirmwareGen2/2m/2x | Gen2 ("Heritage") variants |
| `firmware_oreo` | **FirmwareGen4** | note: `oreo` is the Gen4 app image |
| `firmware_cooper`, `firmware_bentley`, `firmware_aston` | FirmwareCooper/Bentley/Aston | model codenames |
| `firmware_nomad`, `firmware_nomad2` | FirmwareNomad/Nomad2 | newer rings |
| `insight_content` | InsightsContent | non-firmware content pack |
| `assa_config` | AssaConfig | non-firmware (ASSA) config |

(Exact codename→retail-model mapping for the Ring 3 Horizon / Ring 5 on hand is not
asserted here - it needs a real `downloadConfig` response or capture correlation.)

**Can we download the latest image right now?** No - not without an Oura account
token. Probing `GET https://api.ouraring.com/api/v2/file/firmware_oreo/1.0.0/test`
returns **HTTP 401 (nginx/CloudFront)**; the endpoint is hard-gated and rejects an
absent or malformed bearer. We hold ring BLE auth keys but no cloud **account**
OAuth token, and the `{version}`/`{slug}` themselves only come from the
authenticated `downloadConfig`. To actually pull an image you need a valid account
access token (capturable from a logged-in app session); given that, the two GETs
above reconstruct the full download.

## Path A opcodes

| Step | Request | Notes |
| --- | --- | --- |
| StartFwUpdate | `0E 01 <flags>` | enter DFU. flags: 1=ignore battery, 2=ignore sleep analysis, 255=force. Resp `0F`. |
| DFUReset | `2B 01 01` | reset DFU state machine |
| DFUStart | `2B 12 02 <appId><maj><mid><min><startAddr:4><imageLen:4><crc32:4><hwType\|blockSizeIdx>` | declares the image (version + CRC32C, no hash/signature) |
| DFUBlockTransfer | `2B 0C 03 <appId><blockType><blockIdx:2><blockSize:2><numPackets><crc32:4>` | then data as `2C`-framed packets; block size 1024, chunk 198 |
| DFUActivate | `2B 06/07 04 <appId><crc32:4>[force]` | commit; CRC32C of whole image |

`DFUActivate` status codes include `IMAGE_VALIDATION_FAILED(2)` and
`DOWNGRADE_NOT_ALLOWED(3)`. `domain/DFUBlockType.java` defines block types
`NONE(0), EIV(1), IMAGE(2), SIGNATURE(3)` - but the app only ever produces EIV and
IMAGE blocks; it never generates a SIGNATURE block.

CRC is **CRC32C (Castagnoli)** via `firmware/Util.java`.

## Path B opcodes (Cypress bootloader)

Framing: `01 <code> <len:2> [data] <checksum:2> 17`. Opcodes
(`firmware/cypress/Operation.java`): EnterBootloader `0x38`, SetAppMetaData `0x4C`,
**SetEIV `0x4D`**, SendData `0x37`, ProgramData `0x49` (addr+crc32+data),
VerifyApp `0x31`, ExitBootloader `0x3B`. Status table is the stock Cypress one
(`ResponseStatus.java`) - there is no "signature invalid" code, only checksum/app
errors.

## OTA file format

`FirmwareOTAFileHeader.java` parses a standard CYACD2 header: siliconId,
siliconRevision, checksum-type selector, appId, productId. There is **no magic,
no image-wide signature, no key, and no embedded IV in the header**. Rows are
`@EIV:` (encryption IV), `:` data rows (`address:4` + bytes, per-row CRC32C), and
`@APPINFO:`. The binary path (`binary/BinaryFirmwareOTAFile.java`) splits raw
bytes into 1024-byte rows and computes a whole-image CRC32C.

## Is the image signed / can a custom image be flashed?

| Protection | Present | Evidence |
| --- | --- | --- |
| Encryption (AES) | yes | EIV transferred both paths (`DFUBlockType.EIV`, `@EIV:`, `SetEIV 0x4D`) |
| Integrity (CRC32C) | yes | whole-image + per-block + per-row; Cypress SUM/CRC16 |
| Downgrade protection | yes | `DOWNGRADE_NOT_ALLOWED(3)`; version triple in DFUStart |
| Asymmetric signature | reserved, unused by app | `SIGNATURE` block type + `IMAGE_VALIDATION_FAILED(2)` |

**The firmware image is delivered encrypted, and the decryption key is not in the
app - it lives in the ring.** A full grep of the firmware package and
`internal/Constants.java` found no embedded keys, public keys, certificates, or AES
material; the app only supplies framing, CRC32C, versioning, and forwarding of
vendor-encrypted rows + the IV.

**Conclusion: a custom/unsigned image cannot be flashed with only what is in the
app.** You would need the device-resident AES key (and possibly a signing key, if
the ring verifies the reserved SIGNATURE block on the decrypted image). What *is*
fully reconstructable is the wire protocol to **replay an official, vendor-encrypted
image** (e.g. re-flash stock firmware) - not to mint a new one.

These opcodes are catalogued as danger-gated in `tools/oura_protocol.py`
(`start_fw_update`, `dfu_reset`, `dfu_start`, `dfu_block`, `dfu_activate`) and are
never sent during normal use.
