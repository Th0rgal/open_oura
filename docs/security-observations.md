# Security observations (findings only)

What this interoperability project observed about Oura's on-device protections,
stated as **results**. The methods, keys, endpoints, and tooling that produced
them are deliberately omitted — this page is the "what", not the "how".

## On-device ML models
- Shipped as encrypted TorchScript (`.pt.enc`) bundled in the Android app.
- Encryption: AES-256-GCM (Google Tink), 32-byte key, per-file nonce + auth tag.
- The key is delivered by Oura's cloud to an authenticated session — it is **not**
  embedded in the app. Consequence: the models are decryptable by an authenticated
  account holder (unlike the firmware key, which is not).
- The decrypted models are inference-only: frozen weights + graph, no training
  data, optimizer state, or personal data inside.

## Ring firmware
- Encrypted with AES-128-CBC; the key is device-resident and is **not** recoverable
  over BLE and **not** brute-forceable (AES-128 keyspace).
- Integrity is a CRC32C checksum (non-cryptographic), not a MAC.
- Observed reuse of a small set of fixed init vectors across products/versions,
  implying a static, shared key per device family.
- No weak/default/derivable key: standard guess candidates all fail.
- The only avenues with any chance of success are physical, equipment-heavy
  side-channel attacks (matching the published Ledger/Jade-style result) — nothing
  reachable remotely.

---

*Scope note: this is a defensive/interoperability writeup. It intentionally does
not include any cryptographic key, key identifier, server endpoint, extraction or
side-channel procedure, or working code. Those remain unpublished.*
