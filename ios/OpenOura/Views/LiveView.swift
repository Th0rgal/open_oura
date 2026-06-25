import SwiftUI

struct LiveView: View {
    @EnvironmentObject var ring: OuraRing

    var body: some View {
        NavigationStack {
            ScrollView {
                StatusBanner().padding(.top, 4)
                VStack(spacing: 14) {
                    MetricCard(title: "Heart rate", accent: .pink) {
                        HStack(alignment: .firstTextBaseline) {
                            BigValue(value: ring.liveHR.map(String.init) ?? "--", unit: "bpm")
                            Spacer()
                            VStack(alignment: .trailing) {
                                Text("HRV \(ring.liveHRV.map(String.init) ?? "--") ms")
                                    .font(.caption).foregroundColor(.secondary)
                            }
                        }
                        Sparkline(values: ring.hrSeries, color: .pink).frame(height: 70)
                    }

                    MetricCard(title: "Motion", accent: .blue) {
                        HStack(alignment: .firstTextBaseline) {
                            BigValue(value: ring.motionG.map { String(format: "%.2f", $0) } ?? "--", unit: "g")
                            Spacer()
                            Text("restless \(ring.restlessness.map(String.init) ?? "0")%")
                                .font(.caption).foregroundColor(.secondary)
                        }
                        Sparkline(values: ring.motionSeries, color: .blue, minY: 0, maxY: 3).frame(height: 70)
                    }

                    MetricCard(title: "HRV trend (RMSSD)", accent: .purple) {
                        Sparkline(values: ring.hrvSeries, color: .purple).frame(height: 60)
                    }

                    Button(action: toggle) {
                        Text(ring.liveActive ? "Stop live" : "Start live")
                            .frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(ring.liveActive ? .gray : .pink)
                    .disabled(ring.state != .ready)

                    Text("Forces the green-LED measurement and streams motion. Give it ~10–20 s on your finger to lock the first beat.")
                        .font(.caption2).foregroundColor(.secondary)
                        .multilineTextAlignment(.center).padding(.horizontal)
                }
                .padding()
            }
            .navigationTitle("Live")
        }
    }

    private func toggle() {
        if ring.liveActive { ring.stopLive() } else { ring.startLive() }
    }
}
