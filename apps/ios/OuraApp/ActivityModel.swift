#if TORCH
import Foundation
// sqlite3 comes from the bridging header (TorchBridge.h includes <sqlite3.h>)

// On-device activity detection: assemble the MET / motion / temp / HR input series
// (a faithful port of tools/run_activity_model.py — incl. the day-rebased minute
// axis that keeps float32 time alignment exact) and run automatic_activity_detection
// to get the detected workout sessions.
enum ActivityModel {
    private static let BEHAVIOR: [Int: String] = [
        -1: "nothing", 0: "—", 1: "badminton", 2: "boxing", 3: "cross-country skiing",
        4: "cross training", 5: "cycling", 6: "dance", 7: "elliptical", 8: "strength",
        9: "hockey", 10: "pilates", 11: "rowing", 12: "running", 13: "swimming", 14: "walking",
        15: "yoga", 16: "golf", 17: "tennis", 18: "climbing", 19: "downhill skiing",
        20: "snowboarding", 21: "hiking", 22: "horseback riding", 23: "volleyball", 24: "basketball",
        25: "football", 26: "soccer", 27: "baseball", 28: "core", 29: "cricket", 30: "HIIT",
        32: "fitness class", 39: "martial arts", 41: "mountain biking", 42: "nordic walking",
        49: "stretching", 50: "surfing", 53: "padel", 65535: "other", 65536: "nap",
        65537: "sleep", 65538: "pause", 70937: "meditation", 71201: "eating", 71227: "relax", 71239: "transport",
    ]

    static func run() -> [WorkoutSession] {
        let dbPath = DB.readPath()
        guard let modelPath = Bundle.main.path(forResource: "automatic_activity_detection_3_1_11", ofType: "ptl")
        else { return [] }

        var db: OpaquePointer?
        guard sqlite3_open(dbPath, &db) == SQLITE_OK else { return [] }
        defer { sqlite3_close(db) }

        struct Ev { let ds: Int64; let tag: Int; let json: [String: Any]; let cu: Int64 }
        var events: [Ev] = []
        var stmt: OpaquePointer?
        if sqlite3_prepare_v2(db, "SELECT ring_timestamp, tag, decoded_json, captured_unix FROM events WHERE decoded_json IS NOT NULL ORDER BY ring_timestamp", -1, &stmt, nil) == SQLITE_OK {
            while sqlite3_step(stmt) == SQLITE_ROW {
                guard let c = sqlite3_column_text(stmt, 2),
                      let data = String(cString: c).data(using: .utf8),
                      let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { continue }
                events.append(Ev(ds: sqlite3_column_int64(stmt, 0), tag: Int(sqlite3_column_int(stmt, 1)), json: obj, cu: sqlite3_column_int64(stmt, 3)))
            }
        }
        sqlite3_finalize(stmt)
        guard !events.isEmpty else { return [] }

        var maxDs = events[0].ds, anchor = events[0].cu, minDs = events[0].ds
        for e in events { if e.ds > maxDs { maxDs = e.ds; anchor = e.cu }; if e.ds < minDs { minDs = e.ds } }
        func unixMin(_ ds: Int64) -> Double { (Double(anchor) - Double(maxDs - ds) / 10.0) / 60.0 }
        let offset = Int((unixMin(minDs) / 1440).rounded(.down)) * 1440
        func tmin(_ ds: Int64) -> Int { Int(unixMin(ds).rounded()) - offset }
        let nan = Float.nan
        func num(_ v: Any?) -> Float { (v as? NSNumber)?.floatValue ?? 0 }

        var metD: [Int: (Float, Float)] = [:]   // round(t) → (t, met)
        var motion: [[Float]] = [], temp: [[Float]] = [], hr: [[Float]] = []
        for e in events {
            let t = Float(tmin(e.ds))
            switch e.tag {
            case 0x50:
                if let met = e.json["met"] as? [NSNumber] {
                    for (i, m) in met.enumerated() { metD[Int(t) + i] = (t + Float(i), m.floatValue) }
                }
            case 0x47:
                motion.append([t, num(e.json["orientation"]), num(e.json["motion_seconds"]),
                               num(e.json["avg_x"]), num(e.json["avg_y"]), num(e.json["avg_z"]),
                               nan, num(e.json["low_intensity"]), num(e.json["high_intensity"])])
            case 0x46:
                if let c = (e.json["temps_c"] as? [NSNumber])?.first?.floatValue { temp.append([t, c]) }
            case 0x80:
                if let b = e.json["hr_bpm"] as? [NSNumber], !b.isEmpty {
                    hr.append([t, b.map { $0.floatValue }.reduce(0, +) / Float(b.count)])
                }
            default: break
            }
        }
        guard !metD.isEmpty else { return [] }
        var metFlat: [Float] = []
        for (_, v) in metD.sorted(by: { $0.value.0 < $1.value.0 }) { metFlat.append(v.0); metFlat.append(v.1) }
        let nMet = metD.count
        var motionFlat = motion.sorted { $0[0] < $1[0] }.flatMap { $0 }
        var tempFlat = temp.sorted { $0[0] < $1[0] }.flatMap { $0 }
        var hrFlat = hr.sorted { $0[0] < $1[0] }.flatMap { $0 }

        // context [year, month, day, weekday(Mon=0)] from the anchor's local clock
        let tz = (Double(TimeZone.current.secondsFromGMT()) / 3600).rounded()
        var cal = Calendar(identifier: .gregorian); cal.timeZone = TimeZone(identifier: "UTC")!
        let d = Date(timeIntervalSince1970: Double(anchor) + tz * 3600)
        let c = cal.dateComponents([.year, .month, .day, .weekday], from: d)
        var context: [Float] = [Float(c.year!), Float(c.month!), Float(c.day!), Float(((c.weekday! + 5) % 7))]
        var user: [Float] = [30, 1, 1.78, 78] + Array(repeating: nan, count: 10)

        var out = [Float](repeating: 0, count: 256 * 9)
        let n = oura_activity(modelPath, &context, &user, &metFlat, Int32(nMet),
                              &motionFlat, Int32(motion.count), &tempFlat, Int32(temp.count),
                              &hrFlat, Int32(hr.count), 0.5, 5.0, &out, 256)
        guard n > 0 else { return [] }

        let fmt = DateFormatter(); fmt.timeZone = TimeZone(identifier: "UTC"); fmt.dateFormat = "yyyy-MM-dd HH:mm"
        let hm = DateFormatter(); hm.timeZone = TimeZone(identifier: "UTC"); hm.dateFormat = "HH:mm"
        func local(_ minute: Float) -> Date { Date(timeIntervalSince1970: (Double(minute) + Double(offset)) * 60 + tz * 3600) }
        var sessions: [WorkoutSession] = []
        for r in 0..<Int(n) {
            let w = Array(out[r * 9..<r * 9 + 9])
            let start = w[0], end = w[1]
            let label = BEHAVIOR[Int(w[3])] ?? "activity"
            sessions.append(WorkoutSession(start: fmt.string(from: local(start)), end: hm.string(from: local(end)),
                                           durationMin: Int((end - start).rounded()), label: label, isWorkout: Double(w[2])))
        }
        return sessions
    }
}
#endif
