import SwiftUI

struct TodayView: View {
    @EnvironmentObject var ring: OuraRing

    var body: some View {
        NavigationStack {
            ScrollView {
                StatusBanner().padding(.top, 4)
                VStack(spacing: 14) {
                    HStack(spacing: 14) {
                        MetricCard(title: "Resting HR", accent: .pink) {
                            BigValue(value: ring.health.latestHR.map(String.init) ?? "--", unit: "bpm")
                        }
                        MetricCard(title: "HRV", accent: .purple) {
                            BigValue(value: ring.health.latestHRV.map(String.init) ?? "--", unit: "ms")
                        }
                    }
                    HStack(spacing: 14) {
                        MetricCard(title: "Skin temp", accent: .orange) {
                            BigValue(value: ring.health.latestTemp.map { String(format: "%.1f", $0) } ?? "--", unit: "°C")
                        }
                        MetricCard(title: "Battery", accent: .green) {
                            BigValue(value: ring.batteryPercent.map(String.init) ?? "--", unit: "%")
                        }
                    }

                    if !ring.health.hr.isEmpty {
                        MetricCard(title: "Heart rate (synced)", accent: .pink) {
                            Sparkline(values: ring.health.hr.map(\.value), color: .pink).frame(height: 70)
                        }
                    }
                    if !ring.health.temp.isEmpty {
                        MetricCard(title: "Temperature (synced)", accent: .orange) {
                            Sparkline(values: ring.health.temp.map(\.value), color: .orange).frame(height: 60)
                        }
                    }

                    MetricCard(title: "Device", accent: .gray) {
                        row("Serial", ring.serial ?? "—")
                        row("Hardware", ring.hardware ?? "—")
                        row("Firmware", ring.firmware ?? "—")
                        row("Synced events", "\(ring.health.totalEvents)")
                    }

                    Button(action: { Task { await ring.syncHistory() } }) {
                        HStack {
                            if ring.syncing { ProgressView().tint(.white) }
                            Text(ring.syncing ? "Syncing…" : "Sync history")
                        }.frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.borderedProminent).tint(.pink)
                    .disabled(ring.state != .ready || ring.syncing)
                }
                .padding()
            }
            .navigationTitle("Today")
        }
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack { Text(k).foregroundColor(.secondary); Spacer(); Text(v) }
            .font(.footnote)
    }
}
