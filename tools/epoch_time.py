"""Epoch-aware ring_timestamp -> wall-clock mapping.

`ring_timestamp` (ds) is a per-boot relative deciseconds counter: it resets to ~0
every time the ring reboots (battery drain, firmware reset). A single global anchor
therefore scatters older boots to nonsense dates. Recover each boot "epoch" by
walking events in real sync order (captured_unix, then insertion order for ties within
the same second) and splitting on any large backward jump in ds, then anchor each
epoch independently: its newest ds is pinned to that event's capture time and the rest
offset by the decisecond delta.

This mirrors the Rust logic in `crates/oura-summary/src/lib.rs` so the web model
runners and the shared summary brain agree on dates.
"""

# A real reboot drops ds by millions; 6 h of slack absorbs minor out-of-order framing
# within an epoch without ever splitting one.
EPOCH_RESET_SLACK_DS = 6 * 3600 * 10


def build_epoch_assignments(pairs):
    """pairs: iterable of (ds, captured_unix). Returns (epochs, event_epoch_indices)."""
    pairs = list(pairs)
    order = sorted((cu, idx, ds) for idx, (ds, cu) in enumerate(pairs))
    epochs = []
    event_epochs = [0] * len(pairs)
    for cu, idx, ds in order:
        if epochs and ds >= epochs[-1][1] - EPOCH_RESET_SLACK_DS:
            event_epochs[idx] = len(epochs) - 1
            e = epochs[-1]
            if ds >= e[1]:
                e[1] = ds
                e[2] = cu
            e[0] = min(e[0], ds)
        else:
            event_epochs[idx] = len(epochs)
            epochs.append([ds, ds, cu])
    return epochs, event_epochs


def build_epochs(pairs):
    """pairs: iterable of (ds, captured_unix). Returns list of [min_ds, max_ds, anchor_unix]."""
    return build_epoch_assignments(pairs)[0]
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


def unix_in_epoch(epochs, ds, epoch_idx):
    e = epochs[epoch_idx]
    return e[2] - (e[1] - ds) / 10.0
