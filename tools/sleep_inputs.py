"""Pure SleepNet input/output checks shared with the iOS implementation."""


def aligned_stages(timestamps_ms, stages, start_ms, end_ms):
    """Place 30-second model epochs on the bedtime grid; uncovered cells are 0."""
    if len(timestamps_ms) != len(stages) or end_ms <= start_ms:
        raise ValueError("invalid sleep output/window")
    if any(b <= a for a, b in zip(timestamps_ms, timestamps_ms[1:])):
        raise ValueError("sleep output timestamps must increase")
    if any(code not in (1, 2, 3, 4) for code in stages):
        raise ValueError("invalid sleep stage code")
    # SleepNet stamps each epoch with its END time: the first output is start+30 s and
    # the last lands on (or just past) the bedtime end. Epoch k covers (t-30 s, t], so
    # a window that is not a whole number of epochs keeps its final partial epoch.
    result = [0] * max(1, -(-(end_ms - start_ms) // 30000))
    for timestamp, code in zip(timestamps_ms, stages):
        if timestamp <= start_ms:
            continue
        index = int((timestamp - start_ms - 1) // 30000)
        if index < len(result):
            result[index] = code
    return result


def collect_inputs(rows, start_ds, end_ds, captured_unix, unix_ms):
    """Reject events from overlapping relative-counter ranges in other boots."""
    start_ms, end_ms = unix_ms(start_ds, captured_unix), unix_ms(end_ds, captured_unix)
    beats_60, beats_80, hrv_rows = [], [], []
    motion, temp_46, temp_75 = [], [], []
    for ds, tag, value, captured in rows:
        if not start_ds - 6000 <= ds <= end_ds + 6000:
            continue
        timestamp = unix_ms(ds, captured)
        if not start_ms - 600000 <= timestamp <= end_ms + 600000:
            continue
        if abs(timestamp - (start_ms + (ds - start_ds) * 100)) > 300000:
            continue
        if tag == 0x60:
            amplitude = value.get("amplitude", [])
            ibis = value.get("ibi_ms", [])
            elapsed = 0
            for i, ibi in enumerate(ibis):
                if ibi <= 0:
                    continue
                elapsed += ibi
                amp = float(amplitude[i]) if i < len(amplitude) else 0.0
                tail_noise = (len(ibis) >= 4 and i >= len(ibis) - 2 and ibi < 600 and amp <= 0)
                valid = (300 <= ibi <= 2000) and not tail_noise
                beats_60.append((timestamp + elapsed, float(ibi), amp, float(valid)))
        elif tag == 0x80:
            amplitude = value.get("amplitude", [])
            quality = value.get("quality", [])
            elapsed = 0
            for i, ibi in enumerate(value.get("ibi_ms", [])):
                if ibi <= 0:
                    continue
                elapsed += ibi
                valid = 300 <= ibi <= 2000 and (i < len(quality) and quality[i] == 1)
                beats_80.append((timestamp + elapsed, float(ibi),
                                 float(amplitude[i]) if i < len(amplitude) else 0.0, float(valid)))
        elif tag == 0x5D:
            hrv_rows.append((timestamp, value))
        elif tag == 0x47 and value.get("motion_seconds") is not None:
            motion.append((timestamp, float(value["motion_seconds"])))
        elif tag == 0x75 and value.get("temps_c"):
            vals = [float(x) for x in value["temps_c"] if x and float(x) > 0]
            if vals:
                temp_75.append((timestamp, sum(vals) / len(vals)))
        elif tag == 0x46 and value.get("temps_c"):
            temp_46.append((timestamp, float(value["temps_c"][0])))

    if beats_60 and beats_80:
        beats_60.sort()
        ts_60 = [b[0] for b in beats_60]
        amps_60 = sorted(b[2] for b in beats_60 if b[2] > 0)
        med_amp = amps_60[len(amps_60) // 2] if amps_60 else 1200.0
        import bisect
        merged = list(beats_60)
        for ts, ibi, amp, valid in beats_80:
            idx = bisect.bisect_left(ts_60, ts)
            near = False
            if idx < len(ts_60) and abs(ts_60[idx] - ts) <= 15000:
                near = True
            if idx > 0 and abs(ts_60[idx - 1] - ts) <= 15000:
                near = True
            if not near:
                merged.append((ts, ibi, amp if amp > 0 else med_amp, valid))
        beats = merged
    elif beats_60:
        beats = beats_60
    else:
        beats = beats_80

    if not any(b[3] == 1.0 for b in beats) and hrv_rows:
        synth = []
        for ts0, value in sorted(hrv_rows):
            step_ms = max(1, int(value.get("interval_min", 5) or 5)) * 60000
            hrs = value.get("hr_bpm", []) or []
            rmssds = value.get("rmssd_ms", []) or []
            for i, hr in enumerate(hrs):
                if not hr or float(hr) <= 0:
                    continue
                mean_ibi = max(350.0, min(1800.0, 60000.0 / float(hr)))
                rmssd = float(rmssds[i]) if i < len(rmssds) and rmssds[i] else 30.0
                jitter = max(4.0, min(60.0, rmssd * 0.5))
                slot_start = ts0 + i * step_ms
                t_cur = float(slot_start)
                k = 0
                while t_cur < slot_start + step_ms:
                    sign = 1.0 if (k % 2 == 0) else -1.0
                    ibi = max(330.0, min(1950.0, round(mean_ibi + sign * jitter * 0.5)))
                    t_cur += ibi
                    synth.append((int(t_cur), ibi, 1200.0, 1.0))
                    k += 1
        beats = synth

    temp = temp_75 if temp_75 else temp_46
    return sorted(beats), sorted(motion), sorted(temp)


def _interp_at_ds(pts, ds):
    if not pts:
        return 0.0
    if ds <= pts[0][0]:
        return pts[0][1]
    if ds >= pts[-1][0]:
        return pts[-1][1]
    lo, hi = 0, len(pts) - 1
    while lo + 1 < hi:
        mid = (lo + hi) // 2
        if pts[mid][0] <= ds:
            lo = mid
        else:
            hi = mid
    t0, v0 = pts[lo]
    t1, v1 = pts[hi]
    if t1 <= t0:
        return v0
    f = (ds - t0) / (t1 - t0)
    return v0 * (1.0 - f) + v1 * f


def refine_deep_stages(stages, hr_t, hrv_t, motion_t, start_ds, end_ds):
    """Recover N3 deep sleep (code 1) when Oura Gen 4 0x80 missing PPG amplitude
    causes SleepNet to collapse all NREM sleep into Light sleep (code 2)."""
    import math
    n = len(stages)
    if n < 40 or end_ds <= start_ds or 1 in stages:
        return list(stages)
    if stages.count(2) < 20:
        return list(stages)
    hr_pts = sorted((int(ds), float(v)) for ds, v in hr_t if 30.0 < v < 140.0)
    if len(hr_pts) < 6:
        return list(stages)
    hrv_pts = sorted((int(ds), float(v)) for ds, v in (hrv_t or []) if v > 0.0)

    span_ds = max(1.0, float(end_ds - start_ds))
    epoch_ds = span_ds / n
    hr_ep = [_interp_at_ds(hr_pts, start_ds + int(round((i + 0.5) * epoch_ds))) for i in range(n)]
    hrv_ep = [_interp_at_ds(hrv_pts, start_ds + int(round((i + 0.5) * epoch_ds))) for i in range(n)] if hrv_pts else [0.0] * n

    epoch_mo = [0.0] * n
    for ds, val in (motion_t or []):
        f = (ds - start_ds) / span_ds
        if -1e-6 <= f <= 1.0 + 1e-6:
            idx = min(max(int(f * n), 0), n - 1)
            if val > epoch_mo[idx]:
                epoch_mo[idx] = float(val)

    if max(hr_ep) - min(hr_ep) < 2.0:
        return list(stages)

    local_hr = [0.0] * n
    for i in range(n):
        lo = max(0, i - 90)
        hi = min(n, i + 91)
        local_hr[i] = sum(hr_ep[lo:hi]) / (hi - lo)

    sorted_hrv = sorted(v for _, v in hrv_pts)
    hrv_med = sorted_hrv[len(sorted_hrv) // 2] if sorted_hrv else 0.0

    sleep_indices = [i for i, c in enumerate(stages) if c in (1, 2, 3)]
    if len(sleep_indices) < 20:
        return list(stages)
    onset = sleep_indices[0]
    final_sleep = sleep_indices[-1]
    sleep_span = max(1, final_sleep - onset)

    runs = []
    i = 0
    while i < n:
        if stages[i] == 2 and epoch_mo[i] <= 1.0:
            j = i
            while j < n and stages[j] == 2 and epoch_mo[j] <= 1.0:
                j += 1
            if j - i >= 12:
                runs.append((i, j))
            i = j
        else:
            i += 1
    if not runs:
        return list(stages)

    segmented_runs = []
    for r_start, r_end in runs:
        if r_end - r_start <= 180:
            segmented_runs.append((r_start, r_end))
        else:
            cur = r_start
            while cur + 12 <= r_end:
                seg_end = min(r_end, cur + 90)
                if r_end - seg_end < 24:
                    seg_end = r_end
                segmented_runs.append((cur, seg_end))
                cur = seg_end

    first_cycle_max_len = max(
        (r_end - r_start for r_start, r_end in segmented_runs if ((r_start + r_end) / 2.0 - onset) / sleep_span < 0.16),
        default=0,
    )

    scored_runs = []
    for r_start, r_end in segmented_runs:
        run_len = r_end - r_start
        mid = (r_start + r_end) / 2.0
        sleep_frac = max(0.0, (mid - onset) / sleep_span)
        if sleep_frac > 0.56:
            continue
        if sleep_frac < 0.12 and first_cycle_max_len >= 40 and run_len < first_cycle_max_len * 0.65:
            continue
        mean_hr = sum(hr_ep[r_start:r_end]) / run_len
        mean_loc_hr = sum(local_hr[r_start:r_end]) / run_len
        prev_end = max(1, r_start)
        prev_hr = sum(hr_ep[:prev_end]) / prev_end
        mean_hrv = sum(hrv_ep[r_start:r_end]) / run_len if hrv_pts else 0.0
        hr_dip = mean_loc_hr - mean_hr
        prev_dip = prev_hr - mean_hr
        if prev_dip < -0.25 or (prev_dip < 0.10 and hr_dip < 0.35):
            continue
        if sorted_hrv and sleep_frac > 0.12 and mean_hrv > max(38.0, hrv_med * 1.15) and hr_dip < 2.0:
            continue
        homeo = math.exp(-2.2 * sleep_frac)
        dur_bonus = min(1.0, max(0.0, (run_len - 12) / 48.0))
        dip_bonus = max(-0.5, min(0.6, hr_dip / 3.0))
        score = homeo * 0.55 + dur_bonus * 0.35 + dip_bonus * 0.25
        scored_runs.append((score, r_start, r_end, sleep_frac, hr_dip))

    scored_runs.sort(key=lambda x: x[0], reverse=True)
    asleep_epochs = len(sleep_indices)
    target_deep = max(12, int(round(asleep_epochs * 0.105)))
    out = list(stages)
    selected_centers = []
    deep_count = 0

    for _score, r_start, r_end, sleep_frac, hr_dip in scored_runs:
        if deep_count >= target_deep:
            break
        run_len = r_end - r_start
        mid = (r_start + r_end) // 2
        if any(abs(mid - c) < 110 for c in selected_centers):
            continue
        remaining = target_deep - deep_count
        if remaining < 8:
            break
        if sleep_frac < 0.16:
            lead = 2 if r_start <= onset + 12 else min(10, max(2, (run_len - 12) // 5))
            tail = min(4, max(1, (run_len - 12 - lead) // 6))
            max_bout = min(56, run_len - lead - tail, remaining)
            if max_bout < 8:
                continue
            best_s = r_start + lead
            best_hr = 1e9
            for cand_s in range(r_start + lead, max(r_start + lead + 1, r_end - tail - max_bout + 1)):
                m_hr = sum(hr_ep[cand_s:cand_s + max_bout]) / max_bout
                if m_hr < best_hr:
                    best_hr = m_hr
                    best_s = cand_s
            s_idx = best_s
            e_idx = s_idx + max_bout
        elif sleep_frac < 0.36:
            cap = min(42, max(10, (run_len * 9) // 20)) if hr_dip >= 2.0 else min(26, max(10, (run_len * 2) // 5))
            max_bout = min(cap, remaining, run_len)
            tail = min(4, max(1, (run_len - max_bout) // 4))
            e_idx = r_end - tail
            s_idx = max(r_start + min(4, run_len), e_idx - max_bout)
        else:
            max_bout = min(22, max(8, (run_len * 2) // 5), remaining, run_len)
            s_idx = r_start + (run_len - max_bout) // 2
            e_idx = s_idx + max_bout
        if e_idx <= s_idx or e_idx - s_idx < 8:
            continue
        for k in range(s_idx, e_idx):
            out[k] = 1
        deep_count += e_idx - s_idx
        selected_centers.append((s_idx + e_idx) // 2)

    return out

