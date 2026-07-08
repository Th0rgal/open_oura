"""Epoch-aware ring_timestamp -> wall-clock mapping.

`ring_timestamp` (ds) is a per-boot relative deciseconds counter: it resets to ~0
every time the ring reboots (battery drain, firmware reset). A single global anchor
therefore scatters older boots to nonsense dates. Recover each boot "epoch" by
walking events in real sync order (captured_unix, then ds) and splitting on any large
backward jump in ds, then anchor each epoch independently: its newest ds is pinned to
that event's capture time and the rest offset by the decisecond delta.

This mirrors the Rust logic in `crates/oura-summary/src/lib.rs` so the web model
runners and the shared summary brain agree on dates.
"""

# A real reboot drops ds by millions; 6 h of slack absorbs minor out-of-order framing
# within an epoch without ever splitting one.
EPOCH_RESET_SLACK_DS = 6 * 3600 * 10


def build_epochs(pairs):
    """pairs: iterable of (ds, captured_unix). Returns list of [min_ds, max_ds, anchor_unix]."""
    order = sorted((cu, ds) for ds, cu in pairs)
    epochs = []
    for cu, ds in order:
        if epochs and ds >= epochs[-1][1] - EPOCH_RESET_SLACK_DS:
            e = epochs[-1]
            if ds >= e[1]:
                e[1] = ds
                e[2] = cu
            e[0] = min(e[0], ds)
        else:
            epochs.append([ds, ds, cu])
    return epochs


def make_unix_s(epochs):
    """Return f(ds) -> wall-clock seconds, choosing the narrowest epoch containing ds."""
    def unix_s(ds):
        best = None
        for e in epochs:
            if e[0] - EPOCH_RESET_SLACK_DS <= ds <= e[1] + EPOCH_RESET_SLACK_DS:
                span = e[1] - e[0]
                if best is None or span < best[0]:
                    best = (span, e)
        e = best[1] if best else epochs[-1]
        return e[2] - (e[1] - ds) / 10.0
    return unix_s
