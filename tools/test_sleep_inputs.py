import unittest
from sleep_inputs import aligned_stages, collect_inputs, refine_deep_stages


class SleepInputTests(unittest.TestCase):
    def test_model_time_is_not_stretched_to_whole_night(self):
        self.assertEqual(aligned_stages([90000.0, 120000.0], [2, 3], 0, 120000), [0, 0, 2, 3])
        self.assertEqual(aligned_stages([0, 30000, 60000], [4, 1, 2], 0, 60000), [1, 2])

    def test_epochs_are_stamped_with_their_end_time(self):
        # SleepNet's real grid: first epoch ends 30 s after bedtime, last one at or
        # just past its end. A full night must cover every cell (no "incomplete").
        self.assertEqual(aligned_stages([30000, 60000, 90000, 120000], [1, 2, 3, 4], 0, 120000), [1, 2, 3, 4])
        self.assertEqual(aligned_stages([30000, 60000, 90000, 120000], [1, 2, 3, 4], 0, 100000), [1, 2, 3, 4])

    def test_bad_outputs_fail_instead_of_inventing_stages(self):
        for ts, codes in [([0], [1, 2]), ([30000, 0], [1, 2]), ([0], [8])]:
            with self.assertRaises(ValueError):
                aligned_stages(ts, codes, 0, 60000)

    def test_boot_overlap_and_firmware_quality(self):
        rows = [(100, 0x80, {"ibi_ms": [800, 900], "quality": [0, 1]}, 0),
                (100, 0x60, {"ibi_ms": [1000], "amplitude": [9]}, 86400000),
                (110, 0x47, {"motion_seconds": 2}, 0)]
        beats, motion, temp = collect_inputs(rows, 0, 600, 0, lambda ds, cu: ds * 100 + cu)
        self.assertEqual([beat[3] for beat in beats], [0, 1])
        self.assertEqual(motion, [(11000, 2)])
        self.assertEqual(temp, [])

    def test_gen4_sleep_temp_and_0x60_over_0x80_deduplication(self):
        rows = [
            (100, 0x60, {"ibi_ms": [1000, 1000], "amplitude": [1500, 1600]}, 0),
            # Overlapping 0x80 at the same timestamp with zero amplitude must not clobber 0x60
            (100, 0x80, {"ibi_ms": [1000, 1000], "quality": [1, 1]}, 0),
            # Distant 0x80 (>15s later) fills gap and inherits median 0x60 amplitude
            (400, 0x80, {"ibi_ms": [1000], "quality": [1]}, 0),
            (150, 0x75, {"temps_c": [35.4, 35.6]}, 0),
        ]
        beats, _, temp = collect_inputs(rows, 0, 600, 0, lambda ds, cu: ds * 100 + cu)
        self.assertEqual(len(beats), 3)
        self.assertEqual(beats[0][2], 1500.0)
        self.assertEqual(beats[1][2], 1600.0)
        self.assertGreater(beats[2][2], 0.0)
        self.assertEqual(len(temp), 1)
        self.assertAlmostEqual(temp[0][1], 35.5)

    def test_hrv_event_fallback_synthesizes_beats_when_raw_ibi_missing(self):
        rows = [
            (100, 0x5D, {"interval_min": 5, "hr_bpm": [54, 52], "rmssd_ms": [44, 48]}, 0),
            (100, 0x75, {"temps_c": [35.8]}, 0),
        ]
        beats, _, temp = collect_inputs(rows, 0, 6000, 0, lambda ds, cu: ds * 100 + cu)
        self.assertGreater(len(beats), 400)
        self.assertTrue(all(b[3] == 1.0 for b in beats))
        self.assertEqual(len(temp), 1)

    def test_refine_deep_stages_promotes_quiet_low_hr_nrem_bouts(self):
        start_ds = 10000
        end_ds = start_ds + 960 * 300
        stages = [4] * 30 + [2] * 670 + [3] * 60 + [2] * 200
        hr_t = []
        hrv_t = []
        for i in range(96):
            ds = start_ds + i * 3000
            in_n3 = (8 <= i < 22) or (30 <= i < 40)
            hr_t.append((ds, 51.0 if in_n3 else 58.0))
            hrv_t.append((ds, 46.0 if in_n3 else 36.0))
        motion_t = [(start_ds + e * 300, 8.0 if e < 30 else 0.0) for e in range(960)]
        refined = refine_deep_stages(stages, hr_t, hrv_t, motion_t, start_ds, end_ds)
        deep_pct = (refined.count(1) / len(refined)) * 100.0
        self.assertGreaterEqual(deep_pct, 10.0)
        self.assertLessEqual(deep_pct, 28.0)

    def test_audited_window_79784184_80059324_epoch_end_produces_918_complete_cells(self):
        # Audited reproduction: start_ds=79784184, end_ds=80059324 (duration = 275140 ds = 27514 s = 917.133 epochs).
        # SleepNet stamps each 30 s epoch with its END timestamp in ms, producing 918 epochs
        # starting at start_ms + 30000 and ending at start_ms + 918 * 30000.
        start_ms = 79784184 * 100
        end_ms = 80059324 * 100
        ts = [start_ms + (i + 1) * 30000 for i in range(918)]
        raw_stages = [2] * 918
        aligned = aligned_stages(ts, raw_stages, start_ms, end_ms)
        self.assertEqual(len(aligned), 918)
        self.assertNotIn(0, aligned)

    def test_epoch_time_multi_reboot_stall_and_undated_reasons(self):
        from epoch_time import build_epochs, is_dated, make_unix_s, undated_reason
        cap = 1789195500
        epochs = build_epochs([
            (47893458, 0x42, '{"unix_time":1787733221}', 1787733300),
            (49912254, 0x41, '{}', cap),
            (53000000, 0x76, '{}', cap),
            (57660709, 0x41, '{}', cap),
            (61076535, 0x42, '{"unix_time":1789195418}', cap),
        ])
        unix_s = make_unix_s(epochs)
        self.assertTrue(is_dated(epochs, 48500000, cap))
        self.assertFalse(is_dated(epochs, 53000000, cap))
        self.assertIsNone(unix_s(53000000, cap))
        self.assertEqual(undated_reason(epochs, 53000000, cap), "ambiguous_reboot_stall")
        self.assertTrue(is_dated(epochs, 59000000, cap))

    def test_refine_deep_stages_recovers_early_cycle_n3_and_rejects_late_rem_hrv_surges(self):
        start_ds = 100000
        epochs = 980
        end_ds = start_ds + epochs * 300
        stages = [4] * 30 + [2] * (epochs - 38) + [4] * 8
        hr_t = []
        hrv_t = []
        for i in range(98):
            ds = start_ds + i * 3000
            circadian_base = 63.0 - 10.0 * (i / 97.0)
            if i < 3:
                hr, hrv = 67.0, 25.0
            elif 4 <= i <= 10:
                hr, hrv = 58.0, 33.0
            elif 23 <= i <= 26:
                hr, hrv = 51.0, 29.0
            elif 57 <= i <= 62:
                hr, hrv = 51.5, 60.0
            else:
                hr, hrv = circadian_base, 31.0
            hr_t.append((ds, hr))
            hrv_t.append((ds, hrv))
        motion_t = [
            (start_ds + e * 300, 12.0 if (e < 28 or e >= 972) else 0.0)
            for e in range(epochs)
        ]
        refined = refine_deep_stages(stages, hr_t, hrv_t, motion_t, start_ds, end_ds)
        first_deep = refined.index(1)
        self.assertGreaterEqual(first_deep, 38)
        self.assertLessEqual(first_deep, 70)
        self.assertGreaterEqual(refined[44:105].count(1), 30)
        self.assertEqual(refined[570:620].count(1), 0)


if __name__ == '__main__':
    unittest.main()
