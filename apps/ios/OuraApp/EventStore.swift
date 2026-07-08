#if TORCH
import Foundation
// sqlite3 comes from the bridging header (TorchBridge.h includes <sqlite3.h>)

// Shared DB reader for the on-device models. SleepStaging and ActivityModel both need
// the same decoded-JSON event stream and time anchor; this is that read in one place
// (CvaModel reads raw PPG blobs instead, so it opens the DB itself).
enum EventStore {
    // A decoded event row: ring timestamp (ds), tag, decoded JSON, capture unix time.
    struct Ev { let ds: Int64; let tag: Int; let json: [String: Any]; let cu: Int64 }

    /// All events with decoded JSON, ordered by insertion/sync order. Empty on any failure.
    static func decodedEvents(dbPath: String) -> [Ev] {
        var db: OpaquePointer?
        guard sqlite3_open(dbPath, &db) == SQLITE_OK else { return [] }
        defer { sqlite3_close(db) }

        var events: [Ev] = []
        var stmt: OpaquePointer?
        let sql = "SELECT ring_timestamp, tag, decoded_json, captured_unix FROM events WHERE decoded_json IS NOT NULL ORDER BY id"
        if sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK {
            while sqlite3_step(stmt) == SQLITE_ROW {
                guard let cText = sqlite3_column_text(stmt, 2),
                      let data = String(cString: cText).data(using: .utf8),
                      let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { continue }
                events.append(Ev(ds: sqlite3_column_int64(stmt, 0),
                                 tag: Int(sqlite3_column_int(stmt, 1)),
                                 json: obj,
                                 cu: sqlite3_column_int64(stmt, 3)))
            }
        }
        sqlite3_finalize(stmt)
        return events
    }

    // `ds` (ring_timestamp) is a per-boot relative deciseconds counter — it resets to ~0
    // every time the ring reboots. A single global anchor therefore mis-dates older
    // boots (data scattered months off). Recover each boot "epoch" by walking events in
    // real sync order (captured_unix, then insertion order) and splitting on any large backward jump
    // in ds, then anchor each epoch independently. Mirrors crates/oura-summary/src/lib.rs
    // and tools/epoch_time.py so the on-device models and the shared brain agree.
    struct Epoch { var minDs: Int64; var maxDs: Int64; var anchorUnix: Int64 }

    // A real reboot drops ds by millions; 6 h of slack absorbs minor out-of-order framing
    // within an epoch without ever splitting one.
    static let epochResetSlackDs: Int64 = 6 * 3600 * 10

    /// Segment events into boot epochs. Precondition: `events` is non-empty.
    static func epochs(_ events: [Ev]) -> [Epoch] {
        epochsWithAssignments(events).epochs
    }

    /// Segment events and return the boot epoch assigned to each event row.
    static func epochsWithAssignments(_ events: [Ev]) -> (epochs: [Epoch], eventEpochs: [Int]) {
        let order = events.enumerated().map { (idx: $0.offset, cu: $0.element.cu, ds: $0.element.ds) }
            .sorted { $0.cu != $1.cu ? $0.cu < $1.cu : $0.idx < $1.idx }
        var eps: [Epoch] = []
        var eventEpochs = Array(repeating: 0, count: events.count)
        for o in order {
            if var e = eps.last, o.ds >= e.maxDs - epochResetSlackDs {
                eventEpochs[o.idx] = eps.count - 1
                if o.ds >= e.maxDs { e.maxDs = o.ds; e.anchorUnix = o.cu }
                e.minDs = min(e.minDs, o.ds)
                eps[eps.count - 1] = e
            } else {
                eventEpochs[o.idx] = eps.count
                eps.append(Epoch(minDs: o.ds, maxDs: o.ds, anchorUnix: o.cu))
            }
        }
        return (eps, eventEpochs)
    }

    /// Map a raw ds to wall-clock seconds via the narrowest epoch containing it.
    static func unixSeconds(_ ds: Int64, _ eps: [Epoch]) -> Double {
        var best: Epoch?
        for e in eps where ds >= e.minDs - epochResetSlackDs && ds <= e.maxDs + epochResetSlackDs {
            if best == nil || (e.maxDs - e.minDs) < (best!.maxDs - best!.minDs) { best = e }
        }
        let e = best ?? eps[eps.count - 1]
        return Double(e.anchorUnix) - Double(e.maxDs - ds) / 10.0
    }

    /// Map a raw ds to wall-clock seconds via the event's assigned boot epoch.
    static func unixSeconds(_ ds: Int64, epochIdx: Int, _ eps: [Epoch]) -> Double {
        let e = eps[epochIdx]
        return Double(e.anchorUnix) - Double(e.maxDs - ds) / 10.0
    }
}
#endif
