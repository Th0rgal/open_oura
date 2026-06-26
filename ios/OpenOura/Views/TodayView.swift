import SwiftUI

struct TodayView: View {
    @EnvironmentObject var ring: OuraRing

    private var hr: Int? { ring.liveHR ?? ring.health.latestHR }

    var body: some View {
        TabScaffold(title: "Today") {
            // Hero gauge — latest heart rate (live if measuring, else last synced).
            Card {
                HStack(spacing: 18) {
                    ScoreRing(value: hr.map(Double.init), range: 40...110, color: Brand.hr,
                              caption: ring.liveActive ? "LIVE HR" : "HEART RATE", unit: "bpm",
                              pulse: ring.liveActive)
                        .frame(width: 150, height: 150)
                    VStack(alignment: .leading, spacing: 12) {
                        miniStat("HRV", ring.health.latestHRV.map { "\($0)" } ?? "—", "ms", Brand.hrv)
                        miniStat("Temp", ring.health.latestTemp.map { String(format: "%.1f", $0) } ?? "—", "°C", Brand.temp)
                        miniStat("Battery", ring.batteryPercent.map { "\($0)" } ?? "—", "%", Brand.battery)
                    }
                    Spacer(minLength: 0)
                }
            }
            .padding(.horizontal, 16)

            // Trend tiles.
            HStack(spacing: 12) {
                MetricTile(title: "Heart rate", value: ring.health.latestHR.map { "\($0)" } ?? "—",
                           unit: "bpm", accent: Brand.hr, spark: ring.health.hr.suffix(60).map(\.value))
                MetricTile(title: "HRV (RMSSD)", value: ring.health.latestHRV.map { "\($0)" } ?? "—",
                           unit: "ms", accent: Brand.hrv, spark: ring.health.hrv.suffix(60).map(\.value))
            }
            .padding(.horizontal, 16)

            HStack(spacing: 12) {
                MetricTile(title: "Skin temp", value: ring.health.latestTemp.map { String(format: "%.2f", $0) } ?? "—",
                           unit: "°C", accent: Brand.temp, spark: ring.health.temp.suffix(60).map(\.value))
                MetricTile(title: "Blood oxygen", value: ring.health.spo2.last.map { "\(Int($0.value))" } ?? "—",
                           unit: "%", accent: Brand.spo2, spark: ring.health.spo2.suffix(60).map(\.value))
            }
            .padding(.horizontal, 16)

            // Device + sync.
            Card {
                VStack(spacing: 10) {
                    infoRow("Device", ring.serial ?? "—")
                    Divider().overlay(Brand.line)
                    infoRow("Hardware", ring.hardware ?? "—")
                    Divider().overlay(Brand.line)
                    infoRow("Firmware", ring.firmware ?? "—")
                    Divider().overlay(Brand.line)
                    infoRow("Synced events", "\(ring.health.totalEvents)")
                }
            }
            .padding(.horizontal, 16)

            Button(action: { Task { await ring.syncHistory() } }) {
                HStack {
                    if ring.syncing { ProgressView().tint(.white) }
                    Text(ring.syncing ? "Syncing…" : "Sync now")
                }.frame(maxWidth: .infinity).padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent).tint(Brand.hr)
            .disabled(ring.state != .ready || ring.syncing)
            .padding(.horizontal, 16)
        }
    }

    private func miniStat(_ k: String, _ v: String, _ u: String, _ c: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(k.uppercased()).font(.caption2.weight(.semibold)).foregroundStyle(c)
            HStack(alignment: .firstTextBaseline, spacing: 2) {
                Text(v).font(.system(size: 20, weight: .bold, design: .rounded))
                Text(u).font(.caption2).foregroundStyle(Brand.dim)
            }
        }
    }
    private func infoRow(_ k: String, _ v: String) -> some View {
        HStack { Text(k).foregroundStyle(Brand.dim); Spacer(); Text(v) }.font(.footnote)
    }
}
