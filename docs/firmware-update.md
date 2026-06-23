# Firmware Update (DFU/OTA)

How the Oura app updates ring firmware, and whether a custom image could be
flashed. Derived from the decompiled app (`com.ouraring.ourakit.firmware` and
`operations/DFU*`). All multi-byte fields are little-endian.

There are two separate DFU paths:

- **Path A — Oura-protocol DFU** (modern rings): tags `0x0E` + `0x2B` over the
  normal Oura GATT link, orchestrated by `firmware/RingAPIDFU.java`.
- **Path B — Cypress/PSoC bootloader DFU** (legacy rings): the stock Infineon
  CYACD2 bootloader on a dedicated service `00060000-f8ce-11e4-abf4-0002a5d5c51b`
  (`firmware/cypress/CypressFirmwareUpdateService.java`).

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
`NONE(0), EIV(1), IMAGE(2), SIGNATURE(3)` — but the app only ever produces EIV and
IMAGE blocks; it never generates a SIGNATURE block.

CRC is **CRC32C (Castagnoli)** via `firmware/Util.java`.

## Path B opcodes (Cypress bootloader)

Framing: `01 <code> <len:2> [data] <checksum:2> 17`. Opcodes
(`firmware/cypress/Operation.java`): EnterBootloader `0x38`, SetAppMetaData `0x4C`,
**SetEIV `0x4D`**, SendData `0x37`, ProgramData `0x49` (addr+crc32+data),
VerifyApp `0x31`, ExitBootloader `0x3B`. Status table is the stock Cypress one
(`ResponseStatus.java`) — there is no "signature invalid" code, only checksum/app
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
app — it lives in the ring.** A full grep of the firmware package and
`internal/Constants.java` found no embedded keys, public keys, certificates, or AES
material; the app only supplies framing, CRC32C, versioning, and forwarding of
vendor-encrypted rows + the IV.

**Conclusion: a custom/unsigned image cannot be flashed with only what is in the
app.** You would need the device-resident AES key (and possibly a signing key, if
the ring verifies the reserved SIGNATURE block on the decrypted image). What *is*
fully reconstructable is the wire protocol to **replay an official, vendor-encrypted
image** (e.g. re-flash stock firmware) — not to mint a new one.

These opcodes are catalogued as danger-gated in `tools/oura_protocol.py`
(`start_fw_update`, `dfu_reset`, `dfu_start`, `dfu_block`, `dfu_activate`) and are
never sent during normal use.
