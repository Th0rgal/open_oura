#if TORCH
import Foundation
// sqlite3 comes from the bridging header (TorchBridge.h includes <sqlite3.h>)

// Shared DB reader for the on-device models. SleepStaging and ActivityModel both need
// the same decoded-JSON event stream and time anchor; this is that read in one place
// (CvaModel reads raw PPG blobs instead, so it opens the DB itself).
enum EventStore {
    // A decoded event row: ring timestamp (ds), tag, decoded JSON, capture unix time.
    struct Ev { let ds: Int64; let tag: Int; let json: [String: Any]; let cu: Int64 }

    /// All events with decoded JSON, ordered by ring timestamp. Empty on any failure.
    static func decodedEvents(dbPath: String) -> [Ev] {
        var db: OpaquePointer?
        guard sqlite3_open(dbPath, &db) == SQLITE_OK else { return [] }
        defer { sqlite3_close(db) }

        var events: [Ev] = []
        var stmt: OpaquePointer?
        let sql = "SELECT ring_timestamp, tag, decoded_json, captured_unix FROM events WHERE decoded_json IS NOT NULL ORDER BY ring_timestamp"
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

    // Time anchor for the event stream: the (ds, captured_unix) pair at the largest ds,
    // plus the smallest ds. Callers turn `ds` into an absolute clock via these.
    struct Anchor { let maxDs: Int64; let unix: Int64; let minDs: Int64 }

    /// Derive the time anchor. Precondition: `events` is non-empty.
    static func anchor(_ events: [Ev]) -> Anchor {
        var maxDs = events[0].ds, unix = events[0].cu, minDs = events[0].ds
        for e in events {
            if e.ds > maxDs { maxDs = e.ds; unix = e.cu }
            if e.ds < minDs { minDs = e.ds }
        }
        return Anchor(maxDs: maxDs, unix: unix, minDs: minDs)
    }
}
#endif
