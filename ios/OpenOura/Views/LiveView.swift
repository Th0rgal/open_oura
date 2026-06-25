import SwiftUI

struct LiveView: View {
    @EnvironmentObject var ring: OuraRing

    var body: some View {
        TabScaffold(title: "Live") {
            Card {
                VStack(spacing: 14) {
                    ScoreRing(value: ring.liveHR.map(Double.init), range: 40...140, color: Brand.hr,
                              caption: ring.liveActive ? "LIVE" : "HEART RATE", unit: "bpm",
                              pulse: ring.liveActive)
                        .frame(width: 190, height: 190)
                        .padding(.top, 6)
                    if ring.liveActive && ring.liveHR == nil {
                        Text("Measuring… keep the ring snug and still")
                            .font(.caption).foregroundStyle(Brand.dim)
                    }
                }
            }
            .padding(.horizontal, 16)

            HStack(spacing: 12) {
                MetricTile(title: "HRV (RMSSD)", value: ring.liveHRV.map { "\($0)" } ?? "—",
                           unit: "ms", accent: Brand.hrv, spark: ring.hrvSeries.suffix(60))
                MetricTile(title: "Motion", value: ring.motionG.map { String(format: "%.2f", $0) } ?? "—",
                           unit: "g", accent: Brand.motion, spark: ring.motionSeries.suffix(80))
            }
            .padding(.horizontal, 16)

            Card {
                VStack(alignment: .leading, spacing: 8) {
                    Text("HEART RATE").font(.caption2.weight(.semibold)).foregroundStyle(Brand.hr)
                    Sparkline(values: ring.hrSeries, color: Brand.hr).frame(height: 90)
                    HStack {
                        Label("\(ring.restlessness ?? 0)% restless", systemImage: "figure.walk.motion")
                        Spacer()
                        if let g = ring.motionG { Text("|a| \(String(format: "%.2f", g)) g") }
                    }.font(.caption2).foregroundStyle(Brand.dim)
                }
            }
            .padding(.horizontal, 16)

            Button(action: { ring.liveActive ? ring.stopLive() : ring.startLive() }) {
                Label(ring.liveActive ? "Stop live" : "Start live",
                      systemImage: ring.liveActive ? "stop.fill" : "play.fill")
                    .frame(maxWidth: .infinity).padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent).tint(ring.liveActive ? Brand.dim : Brand.hr)
            .disabled(ring.state != .ready)
            .padding(.horizontal, 16)

            Text("Live mode forces the green-LED measurement and streams motion. Heart rate arrives in bursts every few seconds; wear the ring snugly.")
                .font(.caption2).foregroundStyle(Brand.dim)
                .multilineTextAlignment(.center).padding(.horizontal, 28)
        }
    }
}
