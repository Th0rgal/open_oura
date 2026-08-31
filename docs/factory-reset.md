# Factory reset

Two paths are known to work. Neither was documented before; the protocol command
was listed as *"packet shape added as danger; not run"* in
[`horizon-ring3-protocol-cheatsheet.md`](horizon-ring3-protocol-cheatsheet.md),
and no hardware path was recorded at all.

> **A factory reset is the only way back into a ring that no longer talks to
> anything.** All BLE reset paths need a device that is already bonded *and*
> authenticated, so if the last such device is lost, path B below is the only
> remaining option.

## What it erases

- the installed 16-byte app-auth key
- every BLE bond
- the on-ring event buffer — **sync before resetting**
- the anthropometric profile, which returns to firmware defaults
  (`28 4B 02 B0` = age 40, weight 75, sex 2, height 176 on a Ring 4)

**The two paths differ in what happens to the ring's boot-relative event counter**,
which is what `ring_timestamp` and the sync cursor are expressed in:

| path | counter afterwards |
| --- | --- |
| A — `1a00` | keeps running; observed at ~880 000 deciseconds right after the reset |
| B — hardware | **restarts near zero**; observed at 2 743 on the first event synced |

Either way, set `sync_state.next_cursor` to `0` before the first sync afterwards.
`INSERT OR IGNORE` on `UNIQUE(serial, tag, ring_timestamp, body)` makes the re-pull
free, and after path B the old cursor would otherwise sit millions of ticks past
every timestamp the ring can now produce, so no event would ever be returned again.

Path B also emits several `ring_start` (`0x41`) records as the ring reboots. The
first carries `reason: 4`; subsequent ones carry `reason: 0`.

## Path A — protocol command `1a00`

Tag `0x1a`, empty payload. Exposed as `oura factory-reset --yes`, which refuses to
run without the flag.

```
-> 1a 00
<- (nothing observed)
```

`tools/oura_protocol.py:358` parses a tag `0x1B` reply carrying a `u16`
`factory_reset_status`. **No reply was seen in practice** — the ring appears to
reset and drop the link before answering, or faster than the 3 s quiet window used
here. The reset still took effect.

Verified 2026-08-22 on a Ring 4 (`ORE_06`, firmware 2.12.3).

Requires a connected, bonded device. The command is deliberately **not** in
`oura-protocol`, so embedded firmware linking that crate has no way to emit it.

## Path B — hardware, no button

Works on the **Gen3 / Ring 4 charging dock**, which has no button. Useful when the
ring will not connect to anything at all. The documented button-hold procedure
needs the separate *Charging Case*; this one does not.

The dock is flipped 180° with the ring seated on it, and the LED colour confirms
each step:

1. Take the ring **off** the charger if it is already seated.
2. Place the ring on the dock and wait **~2 seconds** — no longer.
3. Flip the dock (with the ring on it) **upside down** → wait for **blue**.
4. Flip it **upside up** → wait for **red**.
5. Flip it **upside down** → wait for **magenta/purple**.
6. Flip it **upside up** → wait for **yellow**.

Yellow means the reset has started. After a few minutes the LED **blinks blue**,
which signals a completed factory reset.

Verified 2026-08-26 on a Ring 4 that would no longer connect through the Oura app,
and on which the "tap the dock" soft reset did nothing — as expected, since that
procedure reboots the ring rather than erasing it.

### Reading the dock LED

Independent of the reset procedure, the dock's RGB LED means:

| colour | meaning |
| --- | --- |
| blinking blue | not paired with the Oura app over Bluetooth |
| solid blue | paired, connection established in the app |
| pulsing white | charging |
| green | fully charged |
| blinking red | Oura's guidance is to contact support |

A ring paired with this project instead of the Oura app sits at **blinking blue**
indefinitely. That is the expected state here, not a fault.

## Afterwards: do not re-onboard with the Oura app

Consumer instructions for a factory reset end with *"open the Oura App → Set up a
new ring"*. **Doing that installs Oura's own auth key and locks this project out**,
and the only way back is another factory reset.

To re-pair with this project instead:

```sh
# reinstalls the existing key file, so the event history stays attributable
# to the same device; mints a new key only if the file is absent
oura --key-file oura-<serial>.key --name "" pair
```

`--name ""` matters: a freshly reset ring does not advertise a local name matching
the CLI's default `Oura` filter.

Bond order is worth thinking about when more than one host is involved. A reset
ring accepts new bonds from anything; once provisioned, a central that is neither
bonded nor able to start encryption gets its link terminated after two or three
connection events with HCI reason `0x15`. Pair every host that will need access
while the ring is still in its post-reset state.
