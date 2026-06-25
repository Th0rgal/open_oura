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

## Downloading firmware from the cloud (reconstructed, working)

With a mobile session token, the OTA download is fully working:

1. `POST /api/v2/client/config` with the ring described in `rings[]` (key fields:
   `hardware_type` = the **codename** e.g. `oreo`/`gen2x`/`cooper`, `firmware_version`,
   `serial_number`, `mac_address`, `capabilities`; also a valid `device.device_uid`
   and a `components` block, else 400 Invalid payload). Response includes
   `ota_files: [{type, version, slug}]`.
2. `GET /api/v2/file/{type}/{version}/{slug}` -> manifest `{filename, size, md5,
   sha256, url}` with a signed `cdn-updates.ouraring.com` URL. The `slug` is a
   server-issued secret; the manifest endpoint 404s for any other slug (no
   enumeration).
3. Download the signed URL; verify md5/sha256.

Hardware-code -> codename (`GetProductInfoKt.fromId`), and codename -> ring:
- `BLB_` -> `gen2x` = Ring 3 (Horizon), latest **3.4.3**
- `ORE_`/`JAD_` -> `oreo` (GEN4) = Ring 4, latest **2.11.0**
- `COR_` -> `cooper` = Ring 5, on **2.1.3**
- others: `GPS_M`->gen2m, `KTH_`->gen4k, `BEN_/BEM_`->bentley, `AST_`->aston,
  `PRC_`->nomad, `NMC_`->nomad2 (Nomad* = charger accessories).

### The "claim outdated to get the latest image" trick (partial)
Claiming an old `firmware_version` makes the server offer the latest for that
codename: `oreo`@1.0.0 -> offers `firmware_oreo 2.11.0`; `gen2x`@1.0.0 -> `3.4.3`.
This works for the **legacy** rings with any (even fake) serial. It does NOT work
for the newer rings: `cooper`/`bentley`/`aston` return only `assa_config`, never a
firmware entry, at any claimed version/serial/format. So either no Ring 5 update
newer than 2.1.3 has shipped yet, or newer-ring firmware uses a gated/staged
channel only activated for genuinely eligible registered devices. Either way the
slug (hence the download) is not obtainable for Cooper from a synthetic request.

We have downloaded and integrity-verified `firmware_oreo 2.11.0` (Ring 4) locally
(notes/firmware/), and analyzed it (below).

## Per-device encryption status: every Oura device firmware is encrypted
Checked the bundled images in the `ring_firmware` split and the downloaded 2.11.0:
all carry `@EIV`, ~8.0/8 byte entropy, and the same PSoC6 header `01002108E221`
(SiliconID `0x08210001`): gen2 (`2.36.1`), gen2x (`3.0.2`), nomad (`2.0.4`),
nomad2 (`0.5.0`), oreo (`2.7.0`/`2.11.0`), both bootloaders. **None are plaintext.**
So there is a decryption key for every Oura device's software (rings AND the
charger accessories), and in every case it is device-resident. Same silicon +
same scheme across the whole line suggests a shared/product key (extracting it
once could decrypt the family), but it still requires getting the key off a chip.

### CYACD2 analysis of `firmware_oreo 2.11.0` (Ring 4)
PSoC6 CYACD2: header SiliconID `0x08210001`, rev `0xE2`, checksum `0x21`;
`@APPINFO:0x10003c00,0x7ddfc` (app at flash `0x10003C00`, ~515 KB); 1007 rows;
`@EIV` = 128-bit AES IV; payload entropy 7.9997/8 -> fully AES-encrypted, no
plaintext strings. Downloadable + integrity-checkable, but not disassemblable
without the device key.

## Can the firmware key be retrieved or broken?
- **In software: no.** It is not in the app, the OTA package, or any API. There is
  a `model-encryption-keys` endpoint for the ML models, but **no equivalent for
  firmware**. The DFU protocol is write-only for images (push encrypted rows + IV;
  no command reads the key or flash).
- **Brute force: no, ever.** AES-128 = 2^128, AES-256 = 2^256. Even a fleet of
  AI/GPU boxes (e.g. an NVIDIA DGX Spark) at a generous 1e12 AES/s would need
  ~1e19 years for AES-128 and ~1e57 for AES-256; the Landauer thermodynamic limit
  rules it out regardless of hardware. Quantum (Grover) only square-roots the work
  and still leaves AES-256 safe and AES-128 impractical.
- **Only realistic avenue: hardware extraction** from the ring's PSoC6 (SWD readout
  if unlocked, fault injection/glitching, or side-channel DPA/CPA). All are
  equipment-heavy, destructive, and not guaranteed; the bottleneck is physical chip
  access, not compute (a GPU box would only speed the side-channel statistics).
