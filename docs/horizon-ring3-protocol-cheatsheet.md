# Oura Ring 3 Horizon BLE Protocol Cheatsheet

Baseline reference: ringverse `oura/BLE.md` for Oura Ring 4 protocol notes.
Live target: Oura Ring 3 Horizon, BLE MAC `aa:bb:cc:dd:ee:ff`, firmware
`3.4.3`, API `2.0.0`, BT stack `5.0.12`.

Captured on 2026-06-21 in Lisbon from macOS/CoreBluetooth. Raw JSONL captures
are under ignored `captures/`.

## Connection

- Advertised service: `98ed0001-a541-11e4-b6a0-0002a5d5c51b`
- Notify/read characteristic: `98ed0003-a541-11e4-b6a0-0002a5d5c51b`
- Write characteristic: `98ed0002-a541-11e4-b6a0-0002a5d5c51b`
- MTU: `203`
- After factory reset, macOS pairing/link encryption is required before notify
  subscription or writes. Before the pairing prompt is approved, CoreBluetooth
  returns `Encryption is insufficient`.
- macOS peripheral UUID remained stable during this capture, but the advertised
  name changed from `Oura XXXXXXXXXXXXXX`/`Oura XXXXXXXXXXXX` to
  `Oura Ring Gen3` after app auth key setup.

Packet format matches ringverse:

- Request/response: `tag length payload...`
- Multi-byte integers observed little-endian.
- Extended operations use outer tag `0x2f`, with the first payload byte as the
  extended operation tag.

## App Auth

Auth is session-scoped. Run it after connecting when a command returns
`2f022f01`.

1. Set a key on a factory-reset ring:
   - Request: `24 10 <16-byte key>`
   - Success response: `250100`
2. Request nonce:
   - Request: `2f012b`
   - Response: `2f102c <15-byte nonce>`
3. Authenticate:
   - Encrypt the 15-byte nonce using `AES/ECB/PKCS5Padding` with the 16-byte
     auth key.
   - Request: `2f112d <16-byte encrypted nonce>`
   - Success response: `2f022e00`
   - Wrong-key response: `2f022e01`

Local test key generated and stored in ignored
`captures/horizon-ring3-auth-key.hex`:

- `4431967d8bacc2659743142b68391d9a`

Do not commit auth keys.

## Confirmed Commands

All rows below were tested on the Horizon unless noted otherwise.

| Name | Request | Response | Notes |
| --- | --- | --- | --- |
| Firmware | `0803000000` | `091202000003040301000105000cffeeddccbbaa` | API `2.0.0`, FW `3.4.3`, bootloader `1.0.1`, BT `5.0.12`, MAC `aa:bb:cc:dd:ee:ff`. |
| Battery before app auth | `0c00` | `2f022f01` | Once an auth key is installed, new connections require app auth for battery. |
| Battery after app auth | `0c00` | `0d06...` | Battery moved from `27%` to `90%` during testing. Payload shape: percent, charging progress, recommended flag, three unknown bytes. |
| Auth nonce | `2f012b` | `2f102c <15-byte nonce>` | Matches ringverse and Android app. |
| Set auth key | `2410 <16-byte key>` | `250100` | Factory-reset Horizon accepts key and returns success. |
| Authenticate | `2f112d <encrypted nonce>` | `2f022e00` | Requires AES/ECB/PKCS5Padding. |
| Authenticate wrong key | same shape | `2f022e01` | Confirms auth failure behavior. |
| Capabilities page 0 | `2f020100` | `2f12020200050102020403030401050107000800` | Two pages reported. Parsed pairs: `(0,5)`, `(1,2)`, `(2,4)`, `(3,3)`, `(4,1)`, `(5,1)`, `(7,0)`, `(8,0)`. |
| Capabilities page 1 | `2f020101` | `2f0c020209000a060b000c000d01` | Parsed pairs: `(9,0)`, `(10,6)`, `(11,0)`, `(12,0)`, `(13,1)`. |
| Product hardware Frodo offset | `1803140010` | `19110036390000424c425f3033000000000000` | Status `00`, data begins with `69` then `BLB_03`. |
| Product hardware | `1803180010` | `191100424c425f303300000000000000000000` | Hardware ID `BLB_03`. |
| Product code | `1803280009` | `190a00060000000a00000006` | Gen2X/BLE product fields; app parser reads design/size/color from this. |
| Product code Frodo offset | `1803340004` | `19050001000199` | Legacy product-code slot still returns bytes. |
| Serial old offset | `1803040010` | `19110007000000585858585858585858585858` | Prefix bytes then `XXXXXXXXXXXX`. |
| Serial | `1803080010` | `19110058585858585858585858585858580000` | Serial `XXXXXXXXXXXXXX`. |
| Get events unauthenticated | `10090000000008ffffffff` | `2f022f01` | Protocol auth required. |
| Get events after auth | same | event packets then `110808009e0e00000300` | Returned 8 packets, summary says 8 events and 3742 bytes left. |
| Sync time | `1209 <u64 timestamp> 00` | `13053d36000000` | Success-shaped response; payload not fully decoded. |
| Set notifications none | `1c0100` | `1d0100` | ACK success. |
| Set notifications all | `1c013f` | `1d0100`, then `1f0420080000` | ACK then notification-state/event packet; restored after testing `0x00`. |
| Set BLE mode normal | `160100` | `170100` | ACK success. |
| Set BLE mode fast HR | `160101` | `170101` | ACK success; restored with `160100`. |
| Set realtime measurements off | `060400000000` | `070100` | ACK success. |
| Set realtime measurements mode 1 | `060401000000` | `070100` | ACK success, but no streaming sensor packets observed over 60s. Restored with off. |
| Set realtime measurements mode 2 | `060402000000` | `070102` | Status `2`; no stream. Restored with off. |
| Set realtime measurements mode 3 | `060403000000` | `070102` | Status `2`; no stream. Restored with off. |
| Set realtime motion guess | `060401010000` | `070102` | Status `2`; no stream. Restored with off. |
| Set realtime HR guess | `060401020000` | `070102` | Status `2`; no stream. Restored with off. |
| Set realtime all bits | `0604ffffffff` | `070102` | Status `2`; no stream. Restored with off. |
| Check sleep analysis | `280100` | `290100` | ACK success. |
| Check sleep analysis force | `280101` | `290100` | ACK success. |
| RData state unauthenticated | `030105` | `2f022f01` | App auth required. |
| RData state after auth | `030105` | `03020503` | Subtag `5` state, status `3` (`INVALID_SUBTAG` per Android enum naming, or equivalent Horizon status). |
| RData state NONE after auth | `03020500` | `03020503` | Same status as no-filter state request. |
| RData page 0 after auth | `0303010000` | `030b0106000000000000000000` | Subtag `1`, status `6` (`NO_DATA`), zero page payload. |
| RData start NONE/zero after auth | `030a02000000000000000000` | `03020203` | Subtag `2` configure/start, status `3`; did not start recording. |
| RData stop after auth | `030103` | `03020308` | Subtag `3`, status `8` (`RECORDING_OFF`). |
| Set user gender empty | `2003020000` | `21020200` | Type `2`, result `0`. |
| Set user height empty | `200403000000` | `21020300` | Type `3`, result `0`. |
| Set user weight empty | `200404000000` | `21020400` | Type `4`, result `0`. |
| Set user unit empty | `2003060000` | `21020600` | Type `6`, result `0`. |
| Set user DOB zero | `200a05000000000000000000` | `21020500` | Type `5`, result `0`. |
| Set ring mode normal | `310400000000` | `320400000000` | Android app uses 4-byte mode value, not 1-byte payload. |
| Set ring mode fast HR | `310401000000` | `320401000000` | Restored with `310400000000`. |
| Sync manufacturing info unauthenticated | `3900` | `2f022f01` | App auth required. |
| Sync manufacturing info after auth | `3900` | `3a0100` | Status `0`. |
| Run self test after auth | `0a04ffffffff` | no notification in 1.5s | Command was written after auth; no `0x0b` response observed. |
| Unknown tag 1 | `0100` | no notification | Matches ringverse no-response note. |
| Unknown tag 2 | `0200` | no notification | Matches ringverse no-response note. |

Feature status requests return `2f022f01` before app auth on a new connection
after an auth key is installed. After app auth, they all returned
`2f0621 <feature> <mode> <status> <state> <subscription>`.

| Feature | Request | Response | Decoded |
| --- | --- | --- | --- |
| `0x00` background DFU | `2f022000` | `2f06210000000002` | mode `0`, status `0`, state `0`, subscription `2` |
| `0x01` research data | `2f022001` | `2f06210100000002` | mode `0`, status `0`, state `0`, subscription `2` |
| `0x02` daytime HR | `2f022002` | `2f06210201000000` | mode `1`, status `0`, state `0`, subscription `0` |
| `0x03` exercise HR | `2f022003` | `2f06210300000000` | mode `0`, status `0`, state `0`, subscription `0` |
| `0x04` SpO2 | `2f022004` | `2f06210400000000` | mode `0`, status `0`, state `0`, subscription `0` |
| `0x05` bundling | `2f022005` | `2f06210500000002` | mode `0`, status `0`, state `0`, subscription `2` |
| `0x06` encrypted API | `2f022006` | `2f06210600000001` | mode `0`, status `0`, state `0`, subscription `1` |
| `0x07` tap to tag | `2f022007` | `2f06210700000000` | mode `0`, status `0`, state `0`, subscription `0` |
| `0x08` resting HR | `2f022008` | `2f06210801000000` | mode `1`, status `0`, state `0`, subscription `0` |
| `0x09` app auth | `2f022009` | `2f06210900000002` | mode `0`, status `0`, state `0`, subscription `2` |
| `0x0a` BLE mode | `2f02200a` | `2f06210a00000002` | mode `0`, status `0`, state `0`, subscription `2` |
| `0x0b` real steps | `2f02200b` | `2f06210b00000000` | mode `0`, status `0`, state `0`, subscription `0` |
| `0x0c` experimental | `2f02200c` | `2f06210c00000000` | mode `0`, status `0`, state `0`, subscription `0` |
| `0x0d` CVA PPG sampler | `2f02200d` | `2f06210d00000000` | mode `0`, status `0`, state `0`, subscription `0` |

Feature latest/parameter requests were auth-gated. After auth:

| Name | Request | Response | Decoded |
| --- | --- | --- | --- |
| Latest daytime HR | `2f022402` | `2f10250200000000000000000080ff00007f` | feature `2`, result `0`, status `0`, state `0`, counter `0`, data `0000000080ff00007f` |
| Latest exercise HR | `2f022403` | `2f0f250302000000000510001d0000ab1b` | feature `3`, result `2` (`NOT_AVAILABLE`), status/state `0`, data `0510001d0000ab1b` |
| Latest SpO2 | `2f022404` | `2f0e2504000000000000000000000000` | feature `4`, result `0`, status/state `0`, zero-like data |
| Latest charging control | `2f02240e` | `2f07250e0100000000` | feature `14`, result `1` (`NOT_SUPPORTED`) |
| Feature param daytime HR id 0 | `2f03460200` | `2f020046` | Horizon returned only extended tag `0x46`; no `0x47` response payload. Treat as unsupported/invalid shape for this param. |

Feature setters tested after auth:

| Name | Request | Response | Follow-up status |
| --- | --- | --- | --- |
| Set daytime HR mode off | `2f03220200` | `2f03230200` | Feature `0x02` then reported mode `0` / off. |
| Set daytime HR mode automatic | `2f03220201` | `2f03230200` | Feature `0x02` restored to mode `1` / automatic. |
| Set daytime HR subscription latest | `2f03260202` | `2f03270200` | ACK success, but feature `0x02` still reported subscription `0` / off. |
| Set daytime HR subscription off | `2f03260200` | `2f03270200` | ACK success, feature `0x02` remained subscription `0` / off. |
| Set daytime HR params payload `00` | `2f03290200` | `2f032a0200` | ACK success. |
| Set bundling off | `2f020300` | `2f020400` | Repeated off returned `0`; left in off state. |
| Set bundling on | `2f020301` | `2f020401` | ACK returned `1`; followed by off restore. |

On-finger realtime/listener follow-up:

- With the ring worn, `060401000000` still only emitted `070100`; no push
  movement, HR, or raw sample packets were observed over 90s.
- On-finger feature polling did change daytime HR status once:
  `2f06210201110200` decoded as feature `2`, mode automatic, status `17`,
  state measuring, subscription off.
- The corresponding latest daytime HR packet changed to
  `2f1025020011020000000003000000990c7f`. Subsequent polls returned idle
  status with stable latest payloads, so this is usable as a quasi-live polling
  path but not a raw stream.
- After a polling disconnect, realtime off was explicitly restored on a fresh
  connection: `060400000000` -> `070100`.

## Horizon Deltas And Issues

- BLE link encryption is mandatory on factory-reset Horizon before any protocol
  command can be written. This is stricter than the earlier Ring 5 observation,
  where firmware was readable without app authentication.
- App-level auth is not required for firmware, product info, capabilities,
  auth nonce, sync time, notifications, BLE mode, realtime-off, user-info empty
  setters, ring mode, or sleep-analysis check after BLE pairing.
- App-level auth is required for battery, basic feature status, history events,
  RData, manufacturing sync, feature-latest values, and feature parameter reads
  once an auth key has been installed. Unauthenticated attempts return
  `2f022f01`.
- App auth must be repeated per BLE connection.
- After `SetAuthKey`, the advertised name changed to generic `Oura Ring Gen3`.
- Capabilities response reports two pages; both page 0 and page 1 were captured.
- Product info uses both legacy and newer offsets. The primary hardware ID is
  `BLB_03`; serial is `XXXXXXXXXXXXXX` (real serial scrubbed; see `local/device-identifiers.md`).
- `0x01` and `0x02` still produce no response, matching ringverse.
- The local runner decodes upstream enum names for auth results, feature
  mode/state/subscription, feature set/latest results, DFU status, firmware
  update status, and event tags. This keeps future JSONL captures readable even
  for packet families that were not run live.
- Realtime tag `0x06` appears to be an enable/disable control, not a complete
  public stream selector. Payload `01000000` ACKs success but did not emit
  movement, HR, or other sensor samples during a 60s listener run. Other simple
  mode guesses returned status `2`.

## Ring 5 Comparison

Current Oura Ring 5 public hardware docs list optical sensing, temperature, and
accelerometer movement tracking. They do not expose a public BLE raw motion
stream or claim a raw gyroscope interface. Ring 5 improves the hardware
architecture versus older rings with smaller packaging, redesigned optical
sensors, two tri-LED emitters, two photodetectors, a digital temperature sensor,
accelerometer movement tracking, and 12 optical signal pathways. From the Ring
3 Horizon protocol side, the relevant commonality is that movement appears to be
accelerometer-based, while raw motion streaming remains unproven.

## Captured Events

Authenticated event fetch returned event tags `0x41` and `0x43` before the
`0x11` summary packet.

| Tag | Meaning | Horizon payload notes |
| --- | --- | --- |
| `0x41` | Ring start | Included firmware/API bytes and boot metadata. |
| `0x43` | Debug event | ASCII debug strings were observed, including `git;ca22327`, `SNH;XXXX`, and `SNL;XXXXXXXXXX` (serial fragments scrubbed). |

## Coverage Matrix

| Packet family | Horizon status |
| --- | --- |
| `0x01`, `0x02` unknown | Tested after auth; no response. |
| `0x03` RData | State, page 0, start-none-zero, and stop tested after auth. Clear shape is in the runner but was not run. **Start is blocked:** Ring 5 returns status `3` (idle) to every `configure` variant tried — with/without `sync_time`, start=now/0, ACM 2g/8g, ±Metadata channel — and never records (see `docs/rdata-capacity-probe.md`). Needs a precondition we haven't replicated. |
| `0x06` realtime measurements | Off and six enable/guess payloads tested. `01000000` ACKed success but did not stream samples; other guesses returned status `2`. |
| `0x08` firmware | Tested, success. |
| `0x0a` self test | Written after auth; no notification observed in 1.5s. |
| `0x0c` battery | Tested unauthenticated failure and authenticated success. |
| `0x0e` firmware update | Packet shape added as danger; not run. |
| `0x10` events | Tested unauthenticated failure and authenticated success. |
| `0x12` sync time | Tested, success-shaped response. |
| `0x16` BLE mode | Tested normal and fast-HR, restored normal. Deep sleep is gated as danger and was not run. |
| `0x18` product info | All six Android/ringverse product slots tested. |
| `0x1a` factory reset | Packet shape added as danger; not run. |
| `0x1c` notification flags | Tested `0x00` and restored `0x3f`, success. |
| `0x20` user info | Empty gender/height/weight/unit and date-of-birth-zero setters tested, success. |
| `0x24` auth key | Tested on factory-reset ring, success. |
| `0x26` flight mode | Packet shape added as danger; not run. |
| `0x28` sleep analysis check | Tested force `0` and force `1`, success. |
| `0x2b` DFU | Reset, zeroed start, zeroed block, and zeroed activate packet shapes added as danger; not run. Real block/activate need image-specific payloads. |
| `0x2f` extended capabilities/status/auth | Capabilities and nonce work before app auth; feature status was tested both unauthenticated failure and authenticated success; auth, latest values, one feature-param read, bundling setter, daytime HR mode/subscription setters, and daytime HR param setter tested. |
| `0x31` ring mode | Tested normal and fast-HR, restored normal. Deep sleep is gated as danger and was not run. |
| `0x37` set manufacturing info | Packet shapes added as danger; not run. |
| `0x39` sync manufacturing info | Tested after auth, success. |

## Untested Or Intentionally Skipped

Skipped because they are destructive, disruptive, or need narrower follow-up
work:

- Factory reset `1a00`
- Firmware update / DFU start / DFU reset / block transfer
- Flight mode `26026027`
- Memory/data clear
- Deep-sleep BLE mode
- Realtime raw sample decoding remains unresolved: no movement/HR/sample packets
  were observed from tested `0x06` modes.
- RData page transfer protocol beyond state/page0/start-none-zero/stop status
  checks
- Set manufacturing info
- Deep-sleep ring mode

## Future Test Commands

Install deps:

```bash
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

List commands:

```bash
.venv/bin/python tools/oura_protocol.py --list
```

Run safe matrix:

```bash
.venv/bin/python tools/oura_protocol.py safe-matrix \
  --name-contains "Oura Ring Gen3" \
  --capture captures/horizon-ring3-safe-matrix.jsonl
```

Run auth-required safe reads:

```bash
.venv/bin/python tools/oura_protocol.py auth-safe-matrix \
  --auth-key-file captures/horizon-ring3-auth-key.hex \
  --name-contains "Oura Ring Gen3" \
  --capture captures/horizon-ring3-safe-extra.jsonl
```

This auth matrix includes battery and basic feature-status reads because those
return `2f022f01` on a fresh connection before `Authenticate`.

Authenticate then fetch events:

```bash
.venv/bin/python tools/oura_protocol.py auth events \
  --auth-key-file captures/horizon-ring3-auth-key.hex \
  --name-contains "Oura Ring Gen3" \
  --capture captures/horizon-ring3-events.jsonl
```

Set a new auth key only when the ring is intentionally factory-reset:

```bash
.venv/bin/python tools/oura_protocol.py auth battery \
  --generate-auth-key \
  --set-auth-key \
  --capture captures/horizon-ring3-auth.jsonl
```

State-changing and destructive packet shapes are hidden from default runs. Use
`--include-state` for non-destructive state changes and `--include-danger` only
when intentionally testing destructive or disruptive behavior.

Listen for realtime notifications:

```bash
.venv/bin/python tools/oura_realtime_listener.py --mode mode1 --seconds 60 \
  --auth-key-file captures/horizon-ring3-auth-key.hex \
  --name-contains "Oura Ring Gen3" \
  --capture captures/horizon-ring3-realtime-mode1-long.jsonl
```

Poll feature status/latest values while the listener is active:

```bash
.venv/bin/python tools/oura_realtime_listener.py --mode mode1 --seconds 40 \
  --poll-latest --poll-interval 5 \
  --auth-key-file captures/horizon-ring3-auth-key.hex \
  --name-contains "Oura Ring Gen3" \
  --capture captures/horizon-ring3-realtime-mode1-onfinger-poll.jsonl
```

Use `--payload-hex <4 bytes>` to test additional `0x06` payloads. The listener
authenticates first and restores realtime off (`060400000000`) on exit by
default.
