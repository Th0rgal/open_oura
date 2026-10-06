"""Epoch-aware ring deciseconds -> wall clock, anchored by time-sync and RTC events."""

import json

EPOCH_RESET_SLACK_DS = 6 * 3600 * 10
FUTURE_SLACK_S = 6 * 3600
# Two anchors of one boot must agree on the counter rate (10 ds/s plus drift). A fresh
# ring's first days ran the counter erratically (weeks of ds in an hour), so nothing
# between two disagreeing anchors has a calendar day. Mirrors ANCHOR_AGREEMENT_* in
# crates/oura-summary/src/ring_time.rs and EventStore.RingClock on iOS.
ANCHOR_AGREEMENT_S = 30 * 60
ANCHOR_AGREEMENT_FRACTION = 0.02


def bracket(anchors, ds):
    """How the anchors on either side of `ds` relate: `("consistent", None)`, `("stalled",
    (before, after))` when the counter lost time between them (ring off; the later
    anchor's offset applies from the stall on), or `("erratic", None)` when the counter
    ran faster than wall time (nothing between them is datable). Outside the anchored
    range the nearest anchor extrapolates as usual."""
    anchors = sorted(anchors)
    idx = next((i for i, (a, _) in enumerate(anchors) if a >= ds), len(anchors))
    if idx == 0 or idx >= len(anchors):
        return "consistent", None
    prev, nxt = anchors[idx - 1], anchors[idx]
    if nxt[0] == ds:
        return "consistent", None
    wall_s = nxt[1] - prev[1]
    counter_s = (nxt[0] - prev[0]) / 10.0
    tolerance = max(ANCHOR_AGREEMENT_S, counter_s * ANCHOR_AGREEMENT_FRACTION)
    if wall_s < counter_s - tolerance:
        return "erratic", None
    if wall_s > counter_s + tolerance:
        return "stalled", (prev, nxt)
    return "consistent", None


def build_epochs(rows):
    """Build boot epochs from `(ds, captured)` pairs or DB `(ds, tag, json, captured)` rows.

    Epoch layout stays list-based for existing callers:
    `[min_ds, max_ds, fallback_unix, capture_min, capture_max, [(ds, unix), ...],
    [ring_start ds, ...]]`.
    """
    normalized = []
    for row in rows:
        if len(row) >= 4:
            ds, tag, js, cu = row[0], row[1], row[2], row[3]
        else:
            ds, cu = row
            tag = js = None
        normalized.append((cu, ds, tag, js))
    epochs = []
    # Callers query by `(captured_unix, id)`. Do not sort by ds here: thousands of
    # rows share one capture second, and sorting those rows would erase reboot jumps.
    for cu, ds, tag, js in normalized:
        if epochs and ds >= epochs[-1][1] - EPOCH_RESET_SLACK_DS:
            e = epochs[-1]
            if ds >= e[1]:
                e[1], e[2] = ds, cu
            e[0], e[3], e[4] = min(e[0], ds), min(e[3], cu), max(e[4], cu)
        else:
            epochs.append([ds, ds, cu, cu, cu, [], []])
        if tag == 0x41:
            epochs[-1][6].append(ds)
        if tag in (0x42, 0x85) and js:
            try:
                unix = json.loads(js).get("unix_time")
                if unix is not None:
                    epochs[-1][5].append((ds, int(unix)))
            except (ValueError, TypeError):
                pass
    return epochs


def make_unix_s(epochs):
    """Return `f(ds, captured_unix=None)` using time-sync, with capture fallback."""
    def unix_s(ds, captured_unix=None):
        candidates = [e for e in epochs
                      if e[0] - EPOCH_RESET_SLACK_DS <= ds <= e[1] + EPOCH_RESET_SLACK_DS]
        if captured_unix is not None and candidates:
            def capture_distance(e):
                if captured_unix < e[3]:
                    return e[3] - captured_unix
                if captured_unix > e[4]:
                    return captured_unix - e[4]
                return 0
            e = min(candidates, key=capture_distance)
        elif candidates:
            e = min(candidates, key=lambda x: x[1] - x[0])
        else:
            e = epochs[-1]
        if e[5]:
            anchor_ds, anchor_unix = min(e[5], key=lambda a: abs(a[0] - ds))
            predicted = anchor_unix + (ds - anchor_ds) / 10.0
            kind, pair = bracket(e[5], ds)
            if kind == "stalled":
                before, after = pair
                late = after[1] - (after[0] - ds) / 10.0
                early = before[1] + (ds - before[0]) / 10.0
                # A ring_start between the anchors marks the stall exactly.
                boots = [b for b in e[6] if before[0] < b <= after[0]]
                if boots:
                    if min(boots) <= ds < max(boots):
                        return None  # lost time cannot be assigned to one of several reboots
                    return late if ds >= max(boots) else early
                if captured_unix is None or late <= captured_unix + FUTURE_SLACK_S:
                    return late
                return early
            if kind == "consistent" and (captured_unix is None or predicted <= captured_unix + FUTURE_SLACK_S):
                return predicted
            if kind == "erratic":
                return predicted  # undated; callers check is_dated()
            if captured_unix is not None:
                # Only borrow a boot's clock when this ds continues that boot's counter;
                # a rebooted ring restarts near zero and must not be projected through an
                # older boot that only ran at higher counts.
                plausible = [unix + (ds - anchor_ds) / 10.0
                             for epoch in epochs for anchor_ds, unix in epoch[5]
                             if unix + (ds - anchor_ds) / 10.0 <= captured_unix + FUTURE_SLACK_S
                             and epoch[0] - EPOCH_RESET_SLACK_DS <= ds <= epoch[1] + EPOCH_RESET_SLACK_DS]
                if plausible:
                    return max(plausible)
        fallback = e[2] - (e[1] - ds) / 10.0
        return min(fallback, captured_unix + FUTURE_SLACK_S) if captured_unix is not None else fallback
    return unix_s


def undated_reason(epochs, ds, captured_unix):
    """Return `None` when `(ds, captured_unix)` is dated, or one of
    `'missing_anchor'`, `'accelerated_counter'`, `'ambiguous_reboot_stall'`."""
    candidates = [e for e in epochs
                  if e[0] - EPOCH_RESET_SLACK_DS <= ds <= e[1] + EPOCH_RESET_SLACK_DS]
    if not candidates:
        return "missing_anchor"
    def capture_distance(e):
        if captured_unix < e[3]:
            return e[3] - captured_unix
        if captured_unix > e[4]:
            return captured_unix - e[4]
        return 0
    e = min(candidates, key=capture_distance)
    if e[5]:
        kind, pair = bracket(e[5], ds)
        if kind == "erratic":
            return "accelerated_counter"
        if kind == "stalled":
            before, after = pair
            boots = [b for b in e[6] if before[0] < b <= after[0]]
            if boots and min(boots) <= ds < max(boots):
                return "ambiguous_reboot_stall"
            return None
        anchor_ds, anchor_unix = min(e[5], key=lambda a: abs(a[0] - ds))
        if anchor_unix + (ds - anchor_ds) / 10.0 <= captured_unix + FUTURE_SLACK_S:
            return None
        if any(unix + (ds - anchor_ds) / 10.0 <= captured_unix + FUTURE_SLACK_S
               and epoch[0] - EPOCH_RESET_SLACK_DS <= ds <= epoch[1] + EPOCH_RESET_SLACK_DS
               for epoch in epochs for anchor_ds, unix in epoch[5]):
            return None
        return "accelerated_counter"
    return "missing_anchor"


def is_dated(epochs, ds, captured_unix):
    """False when the boot holding `ds` has no anchor and was downloaded in one go
    (so the only available time is the download time). Mirrors `ClockSource::is_dated`."""
    return undated_reason(epochs, ds, captured_unix) is None


def latest_unix(epochs):
    anchors = [unix for e in epochs for _, unix in e[5]]
    return max(anchors) if anchors else max(e[2] for e in epochs)
