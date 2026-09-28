"""Epoch-aware ring_timestamp -> wall-clock mapping.

`ring_timestamp` (ds) is a per-boot relative deciseconds counter: it resets to ~0
every time the ring reboots (battery drain, firmware reset). A single global anchor
therefore scatters older boots to nonsense dates. Recover each boot "epoch" by
walking events in real sync order (captured_unix, then ds) and splitting on any large
backward jump in ds, then anchor each epoch independently: its newest ds is pinned to
that event's capture time and the rest offset by the decisecond delta.

A reboot does not always reset ds: after a battery brownout the ring emits
`ring_start` and keeps counting from where it stopped, so the time it spent off is
invisible in ds. When the caller passes the event rows as `markers`, each epoch is
also split at every `ring_start`, and each piece is anchored by the wall-clock points
that fall inside it: `time_sync` / `rtc_beacon` events and the end of every sync
session (newest ds read at that capture time). A piece with no anchor of its own is
assumed contiguous with the next one.

Without `markers` this mirrors the Rust logic in `crates/oura-summary/src/lib.rs` so
the web model runners and the shared summary brain agree on dates.
"""

import json

# A real reboot drops ds by millions; 6 h of slack absorbs minor out-of-order framing
# within an epoch without ever splitting one.
EPOCH_RESET_SLACK_DS = 6 * 3600 * 10
# A capture-time gap this long separates two sync sessions.
SESSION_GAP_S = 3600

TAG_RING_START = 0x41
TAG_TIME_SYNC = 0x42
TAG_RTC_BEACON = 0x85


def build_epochs(pairs, markers=()):
    """pairs: iterable of (ds, captured_unix). markers: optional event rows
    (ds, tag, decoded_json, captured_unix); only ring_start / time_sync / rtc_beacon
    are used. Returns list of [min_ds, max_ds, anchor_unix], anchor_unix being the
    wall clock at max_ds."""
    boots, clocks = set(), {}
    for ds, tag, js, cu in markers:
        if tag == TAG_RING_START:
            boots.add((cu, ds))
        elif tag in (TAG_TIME_SYNC, TAG_RTC_BEACON) and js:
            clocks[(cu, ds)] = json.loads(js)["unix_time"]

    order = sorted((cu, ds) for ds, cu in pairs)
    epochs = []
    cuts, points = [], []  # per epoch: ring_start ds, (ds, wall-clock unix) anchors
    prev_cu = None
    for cu, ds in order:
        if epochs and prev_cu is not None and cu - prev_cu > SESSION_GAP_S:
            points[-1].append((epochs[-1][1], epochs[-1][2]))
        prev_cu = cu
        if epochs and ds >= epochs[-1][1] - EPOCH_RESET_SLACK_DS:
            e = epochs[-1]
            if ds >= e[1]:
                e[1] = ds
                e[2] = cu
            e[0] = min(e[0], ds)
        else:
            epochs.append([ds, ds, cu])
            cuts.append([])
            points.append([])
        if (cu, ds) in boots:
            cuts[-1].append(ds)
        if (cu, ds) in clocks:
            points[-1].append((ds, clocks[(cu, ds)]))

    out = []
    for e, e_cuts, e_points in zip(epochs, cuts, points):
        e_points.append((e[1], e[2]))
        bounds = sorted({b for b in e_cuts if b > e[0]})
        pieces = list(zip([e[0]] + bounds, [b - 1 for b in bounds] + [e[1]]))
        anchored = []
        for lo, hi in pieces:
            inside = [p for p in e_points if lo <= p[0] <= hi]
            if inside:
                ads, au = max(inside)
                anchored.append([lo, hi, au + (hi - ads) / 10.0])
            else:
                anchored.append([lo, hi, None])
        # The newest piece always holds the epoch's own anchor; fill the rest backwards.
        for i in range(len(anchored) - 2, -1, -1):
            if anchored[i][2] is None:
                nxt = anchored[i + 1]
                anchored[i][2] = nxt[2] - (nxt[1] - anchored[i][1]) / 10.0
        out.extend(anchored)
    return out


def make_unix_s(epochs):
    """Return f(ds) -> wall-clock seconds, choosing the narrowest epoch containing ds
    (exact containment first, then within the slack)."""
    def unix_s(ds):
        best = None
        for e in epochs:
            if e[0] - EPOCH_RESET_SLACK_DS <= ds <= e[1] + EPOCH_RESET_SLACK_DS:
                key = (not e[0] <= ds <= e[1], e[1] - e[0])
                if best is None or key < best[0]:
                    best = (key, e)
        e = best[1] if best else epochs[-1]
        return e[2] - (e[1] - ds) / 10.0
    return unix_s
