#if TORCH
import Foundation
// sqlite3 comes from the bridging header (TorchBridge.h includes <sqlite3.h>)

// On-device sleep staging: read the synced DB, assemble the raw SleepNet inputs
// (a faithful port of tools/run_sleep_model.py — the model bakes in its own
// preprocessing), and run sleepnet_moonstone via the LibTorch lite bridge. Returns
// date-label → per-30s stage codes, matching the web dashboard's `nights[].stages`.
enum SleepStaging {
    // Returns date-key → stage codes, plus a non-nil `error` only for genuine failures
    // (bundled model missing). An empty map with `error == nil` just means no sleep data.
    static func run() -> (staged: [String: [Int]], error: String?) {
        guard let modelPath = Bundle.main.path(forResource: "sleepnet_moonstone_1_2_0", ofType: "ptl")
        else { return ([:], "sleep model file missing from the app bundle") }

        let events = EventStore.decodedEvents(dbPath: DB.readPath())
        guard !events.isEmpty else { return ([:], nil) }

        // anchor = (ds, captured_unix) at the largest ds; ms(ds) → absolute epoch ms
        let a = EventStore.anchor(events)
        func ms(_ ds: Int64) -> Int64 { Int64(Double(a.unix) * 1000 - Double(a.maxDs - ds) * 100) }

        // distinct bedtime periods (dedup by start, keep longest), newest first
        var beds: [Int64: Int64] = [:]
        for e in events where e.tag == 0x76 {
            if let s = (e.json["bedtime_start_ds"] as? NSNumber)?.int64Value,
               let en = (e.json["bedtime_end_ds"] as? NSNumber)?.int64Value {
                beds[s] = max(beds[s] ?? 0, en)
            }
        }

        var result: [String: [Int]] = [:]
        for (startDs, endDs) in beds.sorted(by: { $0.key > $1.key }) {
            let lo = startDs - 6000, hi = endDs + 6000
            var beats: [(Int64, Float, Float, Float)] = []
            var acm: [(Int64, Float)] = [], temp: [(Int64, Float)] = []
            for e in events where e.ds >= lo && e.ds <= hi {
                switch e.tag {
                case 0x60, 0x80:
                    guard let ibi = e.json["ibi_ms"] as? [NSNumber] else { continue }
                    let amp = (e.json["amplitude"] as? [NSNumber]) ?? []
                    let t = ms(e.ds); var acc: Int64 = 0
                    for (i, xn) in ibi.enumerated() {
                        let x = xn.int64Value
                        if x <= 0 { continue }
                        acc += x
                        let valid: Float = (x >= 300 && x <= 2000) ? 1 : 0
                        beats.append((t + acc, Float(x), i < amp.count ? amp[i].floatValue : 0, valid))
                    }
                case 0x47:
                    if let mo = (e.json["motion_seconds"] as? NSNumber)?.floatValue { acm.append((ms(e.ds), mo)) }
                case 0x46:
                    if let temps = e.json["temps_c"] as? [NSNumber], let c = temps.first?.floatValue { temp.append((ms(e.ds), c)) }
                default: break
                }
            }
            beats.sort { $0.0 < $1.0 }; acm.sort { $0.0 < $1.0 }; temp.sort { $0.0 < $1.0 }
            guard !beats.isEmpty, beats.contains(where: { $0.3 == 1 }) else { continue }

            var ibiTs = beats.map { $0.0 }
            var ibiVal = beats.flatMap { [$0.1, $0.2, $0.3] }
            var acmTs = acm.map { $0.0 }, acmVal = acm.map { $0.1 }
            var tempTs = temp.map { $0.0 }, tempVal = temp.map { $0.1 }
            var out = [Int32](repeating: 0, count: 4096)
            let n = oura_sleepnet(modelPath,
                                  &ibiTs, &ibiVal, Int32(beats.count),
                                  &acmTs, &acmVal, Int32(acm.count),
                                  &tempTs, &tempVal, Int32(temp.count),
                                  ms(startDs), ms(endDs), &out, 4096)
            if n > 0 {
                // key by the exact bedtime start_ds (matches the summary's night.start_ds)
                // so two sleeps on one calendar day stay distinct.
                result[String(startDs)] = out.prefix(Int(n)).map(Int.init)
            }
        }
        return (result, nil)
    }

}
#endif
