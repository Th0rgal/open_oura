#ifndef OURA_TORCH_BRIDGE_H
#define OURA_TORCH_BRIDGE_H
#include <stdint.h>
#include <sqlite3.h> // surfaced to Swift via the bridging header (no SQLite3 module needed)

// Run SleepNet (moonstone) via the LibTorch lite interpreter on-device and return
// the per-30s hypnogram stage codes (1=DEEP 2=LIGHT 3=REM 4=WAKE), matching
// tools/run_sleep_model.py. The models bake their own preprocessing into the graph,
// so the caller only supplies the raw input tensors (built in SleepStaging.swift).
//
// `ibi_val` is row-major n_ibi×3 (ibi_ms, amplitude, valid); acm/temp values are
// n×1. Timestamps are absolute epoch-ms (int64). Stage codes are written to
// `out_stages` (up to `max_out`); returns the count written, or -1 on failure.
#ifdef __cplusplus
extern "C" {
#endif
int oura_sleepnet(const char *model_path,
                  const int64_t *ibi_ts, const float *ibi_val, int n_ibi,
                  const int64_t *acm_ts, const float *acm_val, int n_acm,
                  const int64_t *temp_ts, const float *temp_val, int n_temp,
                  int64_t bedtime_start_ms, int64_t bedtime_end_ms,
                  int *out_stages, int max_out);
#ifdef __cplusplus
}
#endif
#endif
