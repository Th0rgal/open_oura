import Foundation

enum Core {
    /// Fast, model-free summary (vitals, activity ridges, device) straight from the
    /// shared-core JSON — safe to compute on a background queue and show immediately.
    static func base() -> Summary {
        let path = DB.readPath()   // synced DB if present, else the bundled seed
        // the phone's actual UTC offset, so night labels / sleep windows / digest
        // timing match the wearer's local clock — not a hardcoded constant. The whole
        // stack (web --tz-offset, the Python model runners, this FFI) takes whole
        // hours, so round to the nearest hour (best representable value for the rare
        // sub-hour zones like IST +5:30).
        let secs = TimeZone.current.secondsFromGMT()
        let tzOffset = Int64((Double(secs) / 3600).rounded())
        let json = summaryJson(dbPath: path, tzOffset: tzOffset)
        guard let data = json.data(using: .utf8),
              let s = try? JSONDecoder().decode(Summary.self, from: data)
        else { return Summary(error: "decode failed") }
        return s
    }

    #if TORCH
    /// The slow part: run the three on-device torch models and fold their results into
    /// the summary. Call off the main thread (see RootView.load); never on launch.
    ///
    /// The models are independent (each opens its own DB read), so they run concurrently
    /// — wall-clock is the slowest single model, not their sum. Each reports a per-model
    /// error for genuine failures (missing model / inference failure); those surface in
    /// `modelErrors` so the UI can say a section didn't compute instead of silently
    /// showing the model-free state.
    static func withModels(_ base: Summary) -> Summary {
        var s = base
        let profile = base.profile

        var staged: [String: [Int]] = [:]
        var cva: CvaModel.Result?
        var workouts: [WorkoutSession] = []
        var sleepErr: String?, cvaErr: String?, actErr: String?

        let group = DispatchGroup()
        let q = DispatchQueue.global(qos: .userInitiated)
        q.async(group: group) { let r = SleepStaging.run(); staged = r.staged; sleepErr = r.error }
        q.async(group: group) {
            let r = CvaModel.run(sex: profile?.sex ?? "M", age: profile?.age ?? 30,
                                 heightM: profile?.height_m ?? 1.78, weightKg: profile?.weight_kg ?? 75,
                                 ringSize: profile?.ring_size ?? 10)
            cva = r.result; cvaErr = r.error
        }
        q.async(group: group) { let r = ActivityModel.run(); workouts = r.sessions; actErr = r.error }
        group.wait()

        // fold SleepNet's hypnogram + stage breakdown into each night, keyed by the exact
        // bedtime start_ds so two sleeps on one calendar day don't collide.
        for i in s.nights.indices {
            guard let sds = s.nights[i].start_ds, let stages = staged[String(sds)], !stages.isEmpty else { continue }
            s.nights[i].stages = stages
            let total = Double(stages.count)
            let pct = { (code: Int) in (Double(stages.filter { $0 == code }.count) / total * 100).rounded() }
            s.nights[i].deep_pct = pct(1); s.nights[i].light_pct = pct(2)
            s.nights[i].rem_pct = pct(3); s.nights[i].wake_pct = pct(4)
            let asleep = total - Double(stages.filter { $0 == 4 }.count)
            s.nights[i].efficiency = (asleep / total * 100).rounded()
        }
        if let cva {
            s.cardio = Cardio(vascular_age: cva.vascularAge, chronological_age: profile?.age ?? 30,
                              pwv_ms: cva.pwv, segments: cva.segments)
        }
        s.workouts = workouts
        s.modelErrors = [sleepErr, cvaErr, actErr].compactMap { $0 }
        return s
    }
    #endif
}
