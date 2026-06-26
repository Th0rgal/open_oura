import Foundation

/// Derived health summaries computed from a synced batch of decoded events.
/// Mirrors what the Oura app surfaces, from the data the ring actually emits.
struct HealthData {
    struct Sample: Identifiable { let id = UUID(); let ts: UInt32; let value: Double }

    var eventCounts: [(name: String, count: Int)] = []
    var hr: [Sample] = []
    var hrv: [Sample] = []
    var temp: [Sample] = []
    var spo2: [Sample] = []
    var hypnogram: [String] = []
    var totalEvents = 0

    var latestHR: Int? { hr.last.map { Int($0.value.rounded()) } }
    var latestHRV: Int? { hrv.last.map { Int($0.value.rounded()) } }
    var latestTemp: Double? { temp.last?.value }

    /// Sleep-stage distribution (epoch counts) from the most recent hypnogram.
    var stageCounts: [(stage: String, count: Int)] {
        let order = ["deep", "rem", "light", "awake"]
        var c: [String: Int] = [:]
        for s in hypnogram { c[s, default: 0] += 1 }
        return order.compactMap { s in c[s].map { (s, $0) } }
    }

    init() {}

    init(events: [DecodedEvent]) {
        totalEvents = events.count
        var counts: [String: Int] = [:]
        let sorted = events.sorted { $0.timestamp < $1.timestamp }

        func nums(_ any: Any?) -> [Double] {
            (any as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue } ?? []
        }

        for e in sorted {
            counts[e.name, default: 0] += 1
            switch e.tag {
            case 0x80, 0x60: // green/ibi HR events
                let v = nums(e.json["hr_bpm"]).filter { $0 > 30 && $0 < 240 }
                if let m = v.last { hr.append(.init(ts: e.timestamp, value: m)) }
            case 0x5d: // hrv_event: arrays of hr_bpm + rmssd_ms (per 5 min)
                if let r = nums(e.json["rmssd_ms"]).last, r > 0 { hrv.append(.init(ts: e.timestamp, value: r)) }
                if let h = nums(e.json["hr_bpm"]).last, h > 30 { hr.append(.init(ts: e.timestamp, value: h)) }
            case 0x46, 0x69, 0x75: // temperature
                let v = nums(e.json["temps_c"]).filter { $0 > 20 && $0 < 45 }
                if let m = v.last { temp.append(.init(ts: e.timestamp, value: m)) }
            case 0x6f, 0x70, 0x77: // spo2
                if let s = (e.json["spo2_percent"] as? NSNumber)?.doubleValue, s > 50 {
                    spo2.append(.init(ts: e.timestamp, value: s))
                }
            case 0x4b, 0x4e, 0x5a: // sleep phases (hypnogram)
                if let ph = e.json["phases"] as? [String] { hypnogram.append(contentsOf: ph) }
            default: break
            }
        }
        eventCounts = counts.sorted { $0.value > $1.value }.map { ($0.key, $0.value) }
    }
}
