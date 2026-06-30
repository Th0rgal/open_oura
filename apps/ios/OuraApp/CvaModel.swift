#if TORCH
import Foundation
// sqlite3 comes from the bridging header (TorchBridge.h includes <sqlite3.h>)

// On-device cardiovascular age: decode the ring's raw PPG (cva_raw_ppg_data, tag
// 0x81) the same way the app does, segment it into 1500-sample windows, and run
// cva_2_1_0 — a faithful port of tools/run_cva_model.py. Returns (vascular_age,
// pwv, segments), or nil when there's no usable PPG.
enum CvaModel {
    private static let SEG_LEN = 1500
    private static let GAP_DS: Int64 = 20  // >2 s splits two PPG measurements

    struct Result { let vascularAge: Double; let pwv: Double; let segments: Int }

    static func run(sex: String, age: Double, heightM: Double, weightKg: Double, ringSize: Double) -> Result? {
        guard let dbPath = Bundle.main.path(forResource: "oura", ofType: "db"),
              let modelPath = Bundle.main.path(forResource: "cva_2_1_0", ofType: "ptl")
        else { return nil }

        var db: OpaquePointer?
        guard sqlite3_open(dbPath, &db) == SQLITE_OK else { return nil }
        defer { sqlite3_close(db) }

        // raw PPG bodies in time order (tag 129)
        var tss: [Int64] = [], bodies: [[UInt8]] = []
        var stmt: OpaquePointer?
        if sqlite3_prepare_v2(db, "SELECT ring_timestamp, body FROM events WHERE tag=129 AND body IS NOT NULL ORDER BY ring_timestamp", -1, &stmt, nil) == SQLITE_OK {
            while sqlite3_step(stmt) == SQLITE_ROW {
                let ts = sqlite3_column_int64(stmt, 0)
                let n = Int(sqlite3_column_bytes(stmt, 1))
                guard n > 0, let p = sqlite3_column_blob(stmt, 1) else { continue }
                tss.append(ts)
                bodies.append([UInt8](UnsafeBufferPointer(start: p.assumingMemoryBound(to: UInt8.self), count: n)))
            }
        }
        sqlite3_finalize(stmt)
        guard !bodies.isEmpty else { return nil }

        // split into contiguous measurement runs, decode + chunk into 1500-sample segments
        var segments: [Float] = []   // flattened n_segs × 1500
        var nSegs = 0
        var run: [[UInt8]] = [bodies[0]]
        func flush(_ r: [[UInt8]]) {
            let wave = decode(r)
            var s = 0
            while s + SEG_LEN <= wave.count { segments.append(contentsOf: wave[s..<s + SEG_LEN]); nSegs += 1; s += SEG_LEN }
        }
        for i in 1..<bodies.count {
            if tss[i] - tss[i - 1] > GAP_DS { flush(run); run = [] }
            run.append(bodies[i])
        }
        flush(run)
        guard nSegs > 0 else { return nil }

        let sexVal: Float = sex.uppercased() == "F" ? -1 : (sex.uppercased() == "O" ? 0 : 1)
        var demo: [Float] = [sexVal, Float(heightM), Float(age), Float(ringSize), Float(weightKg)]
        var vage = 0.0, pwv = 0.0
        let rc = oura_cva(modelPath, &segments, Int32(nSegs), &demo, &vage, &pwv)
        return rc == 0 ? Result(vascularAge: (vage * 10).rounded() / 10, pwv: (pwv * 100).rounded() / 100, segments: nSegs) : nil
    }

    // PPG delta stream: 0x80 marks the next 3 bytes as an absolute 24-bit sample;
    // otherwise an int8 delta from the previous sample.
    private static func decode(_ bodies: [[UInt8]]) -> [Float] {
        var samples: [Float] = []
        var acc: Int32 = 0
        for data in bodies {
            var i = 0; let n = data.count
            while i < n {
                let b = data[i]
                if b == 0x80 && i + 3 < n {
                    var raw = Int32(data[i + 1]) | (Int32(data[i + 2]) << 8) | (Int32(data[i + 3]) << 16)
                    if raw & 0x800000 != 0 { raw -= 0x1000000 }
                    acc = raw; samples.append(Float(acc)); i += 4
                } else {
                    acc &+= Int32(Int8(bitPattern: b)); samples.append(Float(acc)); i += 1
                }
            }
        }
        return samples
    }
}
#endif
