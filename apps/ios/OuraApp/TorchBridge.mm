// Objective-C++ bridge: run SleepNet (moonstone) through the LibTorch lite
// interpreter on iOS. Mirrors the input order of tools/run_sleep_model.py:
//   forward(bedtime, ibi_val, ibi_ts, acm_val, acm_ts, temp_val, temp_ts,
//           spo2_val, spo2_ts, scalars, tst)
// and reads stages from output tuple element 1, column 0 (staging[:, 0]).
#import "TorchBridge.h"
#include <torch/csrc/jit/mobile/import.h>
#include <torch/csrc/jit/mobile/module.h>
#include <ATen/ATen.h>
#include <algorithm>
#include <string>
#include <vector>

static at::Tensor blobLong(const int64_t *p, int64_t n) {
    return at::from_blob((void *)p, {n}, at::kLong).clone();
}
static at::Tensor blobFloat2d(const float *p, int64_t rows, int64_t cols) {
    return at::from_blob((void *)p, {rows, cols}, at::kFloat).clone();
}

int oura_sleepnet(const char *model_path,
                  const int64_t *ibi_ts, const float *ibi_val, int n_ibi,
                  const int64_t *acm_ts, const float *acm_val, int n_acm,
                  const int64_t *temp_ts, const float *temp_val, int n_temp,
                  int64_t bedtime_start_ms, int64_t bedtime_end_ms,
                  int *out_stages, int max_out) {
    try {
        auto m = torch::jit::_load_for_mobile(std::string(model_path), c10::nullopt);

        auto ibi_ts_t = blobLong(ibi_ts, n_ibi);
        auto ibi_val_t = blobFloat2d(ibi_val, n_ibi, 3);
        auto acm_ts_t = blobLong(acm_ts, n_acm);
        auto acm_val_t = blobFloat2d(acm_val, n_acm, 1);
        auto temp_ts_t = blobLong(temp_ts, n_temp);
        auto temp_val_t = blobFloat2d(temp_val, n_temp, 1);

        int64_t bt[2] = {bedtime_start_ms, bedtime_end_ms};
        auto bedtime = blobLong(bt, 2);
        auto spo2_val = at::empty({0, 1}, at::kFloat);
        auto spo2_ts = at::empty({0}, at::kLong);
        float sc[5] = {35.f, 25.f, 0.f, 0.f, 0.f};
        auto scalars = at::from_blob(sc, {5}, at::kFloat).clone();
        float tstv[1] = {300.f};
        auto tst = at::from_blob(tstv, {1}, at::kFloat).clone();

        std::vector<c10::IValue> inputs{bedtime, ibi_val_t, ibi_ts_t, acm_val_t, acm_ts_t,
                                        temp_val_t, temp_ts_t, spo2_val, spo2_ts, scalars, tst};
        auto out = m.forward(inputs).toTuple();
        auto staging = out->elements()[1].toTensor();           // [epochs, channels]
        auto col0 = staging.select(1, 0).to(at::kInt).contiguous();
        int n = std::min<int>((int)col0.numel(), max_out);
        const int *acc = col0.data_ptr<int>();
        for (int i = 0; i < n; i++) out_stages[i] = acc[i];
        return n;
    } catch (const std::exception &e) {
        return -1;
    }
}

int oura_cva(const char *model_path, const float *ppg, int n_segs, const float *demo,
             double *out_vascular_age, double *out_pwv) {
    try {
        auto m = torch::jit::_load_for_mobile(std::string(model_path), c10::nullopt);
        auto ppg_t = blobFloat2d(ppg, n_segs, 1500);
        auto demo_t = blobFloat2d(demo, 1, 5);
        auto out = m.forward({ppg_t, demo_t}).toTuple();
        // (daily_cva, quality, raw_quality, daily_pwv, ppg_segment_metrics)
        *out_vascular_age = out->elements()[0].toTensor().item<double>();
        *out_pwv = out->elements()[3].toTensor().item<double>();
        return 0;
    } catch (const std::exception &e) {
        return -1;
    }
}
