import Foundation

// ── the shared build_summary() JSON, decoded (same contract as the web client) ──
// SIBLING CLIENT: the web dashboard (dashboard/web/app.js) renders the SAME summary
// JSON. A user-facing change here usually belongs there too — see the feature map in
// docs/clients-web-and-ios.md. New computed fields go in crates/oura-summary; new
// models get an on-device path (TorchBridge.mm + *Model.swift) AND a Python runner.

struct Trend: Decodable {
    var series: [Double] = []
    var latest: Double? = nil
    var baseline: Double? = nil
    var delta_pct: Double? = nil
}
struct Vitals: Decodable { var hrv = Trend(); var rhr = Trend() }
struct NightRow: Decodable, Identifiable {
    var date: String?; var ymd: String?; var start_ds: Int64?; var start: String?; var end: String?
    var in_bed_h: Double?; var hrv_ms: Double?; var rhr: Double?
    var skin_temp: Double?; var spo2_mean: Double?
    // model-derived (present once the hypnogram runner is wired): per-30s stage codes
    // 1=deep 2=light 3=rem 4=wake, the stage percentages, and efficiency.
    var deep_pct: Double?; var light_pct: Double?; var rem_pct: Double?
    var wake_pct: Double?; var efficiency: Double?
    var stages: [Int]? = nil
    var id: String { (date ?? "") + (start ?? "") }
    var hasHypnogram: Bool { (stages?.count ?? 0) > 1 }
}
struct DailyStat: Decodable { var active_kcal: Double?; var total_kcal: Double?; var steps: Double?; var distance_m: Double? }
struct Profile: Decodable { var sex: String?; var age: Double?; var height_m: Double?; var weight_kg: Double?; var ring_size: Double? }
// a detected activity session (on-device automatic_activity_detection)
struct WorkoutSession: Identifiable {
    let start: String; let end: String; let durationMin: Int; let label: String; let isWorkout: Double
    var id: String { start + label }
    var dayLabel: String { String(start.prefix(10)) }      // YYYY-MM-DD
    var startHM: String { String(start.suffix(5)) }        // HH:MM
}
struct Cardio: Decodable { var vascular_age: Double?; var chronological_age: Double?; var pwv_ms: Double?; var segments: Int? }
struct Fitness: Decodable { var vo2max: Double? }
struct Device: Decodable {
    var serial: String?; var firmware: String?
    var battery_pct: Int?
    var days_of_data: Double?; var nights: Int?
    var synced: String?; var synced_hm: String?
}
struct Summary: Decodable {
    var digest: String?
    var device: Device?
    var nights: [NightRow] = []
    var vitals = Vitals()
    var activity_profile: [String: [Double]] = [:]   // date → 96 × 15-min mean MET-above-rest
    var activity_daily: [String: DailyStat] = [:]     // date → steps / active-kcal / total-kcal
    var profile: Profile?
    var cardio: Cardio?
    var fitness: Fitness?
    var workouts: [WorkoutSession] = []   // on-device only (not in the JSON)
    var modelErrors: [String] = []        // on-device model failures (not in the JSON)
    var error: String?
    // `workouts`/`modelErrors` are filled on-device (not in the FFI JSON), so keep them
    // out of decoding.
    enum CodingKeys: String, CodingKey {
        case digest, device, nights, vitals, activity_profile, activity_daily, profile, cardio, fitness, error
    }
    /// recent days (newest first) that have a movement profile.
    var activeDays: [String] { activity_profile.keys.sorted(by: >) }
}

extension Summary {
    // The calendar date you WOKE from a night. Nights are labelled by onset date (the
    // evening you went to bed), so an overnight sleep crossing midnight belongs to the
    // next day's morning. Pairing a day with the sleep you woke from — not the sleep you
    // started that evening — is what makes "night + activity of the day" one coherent
    // day. Kept identical to the web dashboard's wakeYmd().
    func wakeYmd(_ n: NightRow) -> String? {
        guard let ymd = n.ymd else { return nil }
        guard let s = n.start, let e = n.end, e < s else { return ymd }
        let p = ymd.split(separator: "-").compactMap { Int($0) }
        guard p.count == 3 else { return ymd }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        var comps = DateComponents(); comps.year = p[0]; comps.month = p[1]; comps.day = p[2]
        guard let base = cal.date(from: comps),
              let next = cal.date(byAdding: .day, value: 1, to: base) else { return ymd }
        let o = cal.dateComponents([.year, .month, .day], from: next)
        return String(format: "%04d-%02d-%02d", o.year ?? 0, o.month ?? 0, o.day ?? 0)
    }

    // every date with a night (by wake date) or activity — newest first; the unit both
    // the home hero and AllDaysView iterate.
    var days: [String] {
        var set = Set(activity_profile.keys)
        for n in nights { if let w = wakeYmd(n) { set.insert(w) } }
        return set.sorted(by: >)
    }

    // the primary sleep you woke from on the morning of `day` — the longest in-bed night
    // wins over same-morning naps. Falls back to a MM-DD match for older data lacking ymd.
    func night(forDay day: String) -> NightRow? {
        let cands = nights.filter { wakeYmd($0) == day }
        if let best = cands.max(by: { ($0.in_bed_h ?? 0) < ($1.in_bed_h ?? 0) }) { return best }
        return nights.first { $0.ymd == nil && ($0.date ?? "").hasSuffix(String(day.suffix(5))) }
    }
    func workoutsOn(_ day: String) -> [WorkoutSession] {
        workouts.filter { $0.isWorkout >= 0.5 && $0.dayLabel == day }
    }
}

// identifies a day for the activity-detail sheet (String isn't Identifiable on its own).
struct DaySel: Identifiable { let id: String }
