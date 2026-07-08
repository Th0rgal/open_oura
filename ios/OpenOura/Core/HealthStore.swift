import Foundation

/// On-disk cache of the last synced history, so the app shows real data instantly
/// on launch and only refreshes it when a new sync *completes* (never mid-sync).
enum HealthStore {
    private struct StoredEvent: Codable { let tag: UInt8; let ts: UInt32; let json: String; let identity: String? }

    private static let fileURL: URL = {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("oura_history.json")
    }()

    static func save(_ events: [DecodedEvent]) -> Bool {
        let stored = events.map {
            StoredEvent(tag: $0.tag, ts: $0.timestamp, json: jsonString($0.json), identity: $0.identity)
        }
        do {
            let data = try JSONEncoder().encode(stored)
            try data.write(to: fileURL, options: [.atomic])
        } catch {
            return false
        }
        UserDefaults.standard.set(Date(), forKey: "lastSyncDate")
        return true
    }

    static func load() -> [DecodedEvent] {
        guard let data = try? Data(contentsOf: fileURL),
              let stored = try? JSONDecoder().decode([StoredEvent].self, from: data) else { return [] }
        return stored.map { se in
            let dict = (try? JSONSerialization.jsonObject(with: Data(se.json.utf8))) as? [String: Any] ?? [:]
            return DecodedEvent(
                tag: se.tag,
                timestamp: se.ts,
                name: OuraCore.eventName(tag: se.tag),
                json: dict,
                identity: se.identity ?? "\(se.tag)-\(se.ts)-\(se.json)"
            )
        }
    }

    static func clear() {
        try? FileManager.default.removeItem(at: fileURL)
        UserDefaults.standard.removeObject(forKey: "lastSyncDate")
    }

    static var lastSync: Date? { UserDefaults.standard.object(forKey: "lastSyncDate") as? Date }

    private static func jsonString(_ d: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: d),
              let s = String(data: data, encoding: .utf8) else { return "{}" }
        return s
    }
}
