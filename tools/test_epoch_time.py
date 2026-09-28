"""Regression checks for the epoch-aware ring_timestamp -> wall-clock mapping.

Run: python3 tools/test_epoch_time.py
"""
import json

from epoch_time import TAG_RING_START, TAG_TIME_SYNC, build_epochs, make_unix_s

T1 = 1_788_000_000  # first sync: ds 1000 is read at T1
OFF_S = 2 * 86400   # ring dead on an empty battery, ds counter frozen


def brownout_rows():
    """Sync at ds 1000; ring runs to ds 2000, dies for OFF_S, boots at ds 2001
    without resetting ds, runs to ds 3000; second sync reads ds 3000 at T2."""
    t2 = T1 + (2000 - 1000) / 10 + OFF_S + (3000 - 2001) / 10
    rows = [(ds, 0x46, "{}", T1) for ds in range(0, 1001, 100)]
    rows += [(ds, 0x46, "{}", t2) for ds in range(1100, 2001, 100)]
    rows.append((2001, TAG_RING_START, "{}", t2))
    rows += [(ds, 0x46, "{}", t2) for ds in range(2100, 3001, 100)]
    return rows, t2


def test_brownout_keeps_pre_boot_dates():
    rows, t2 = brownout_rows()
    pairs = [(r[0], r[3]) for r in rows]
    unix_s = make_unix_s(build_epochs(pairs, markers=rows))
    assert unix_s(1000) == T1
    assert unix_s(1500) == T1 + 50
    assert unix_s(2500) == t2 - 50
    # Without markers the dead time is folded into the old dates (previous behaviour).
    assert abs(make_unix_s(build_epochs(pairs))(1500) - (T1 + 50 + OFF_S)) < 1


def test_time_sync_anchors_piece_without_sync_end():
    rows, t2 = brownout_rows()
    # Drop the first sync: the pre-boot piece is only anchored by a time_sync.
    rows = [(ds, tag, js, t2) for ds, tag, js, _ in rows]
    rows.append((500, TAG_TIME_SYNC, json.dumps({"unix_time": T1 - 50}), t2))
    pairs = [(r[0], r[3]) for r in rows]
    unix_s = make_unix_s(build_epochs(pairs, markers=rows))
    assert unix_s(1500) == T1 + 50
    assert unix_s(2500) == t2 - 50


if __name__ == "__main__":
    test_brownout_keeps_pre_boot_dates()
    test_time_sync_anchors_piece_without_sync_end()
    print("ok")
