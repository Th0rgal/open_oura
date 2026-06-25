import SwiftUI

struct SleepView: View {
    @EnvironmentObject var ring: OuraRing

    private let stageColor: [String: Color] = [
        "deep": .indigo, "rem": .purple, "light": .blue, "awake": .pink,
    ]

    var body: some View {
        NavigationStack {
            ScrollView {
                StatusBanner().padding(.top, 4)
                VStack(spacing: 14) {
                    if ring.health.hypnogram.isEmpty {
                        MetricCard(title: "Hypnogram", accent: .indigo) {
                            Text("No sleep stages synced yet. Wear the ring overnight, then Sync history. (Stages are computed on the ring and arrive as events.)")
                                .font(.footnote).foregroundColor(.secondary)
                        }
                    } else {
                        MetricCard(title: "Hypnogram (\(ring.health.hypnogram.count) epochs)", accent: .indigo) {
                            HypnogramView(stages: ring.health.hypnogram, color: stageColor)
                                .frame(height: 120)
                        }
                        MetricCard(title: "Stage distribution", accent: .indigo) {
                            ForEach(ring.health.stageCounts, id: \.stage) { item in
                                let pct = Int(Double(item.count) / Double(max(ring.health.hypnogram.count, 1)) * 100)
                                HStack {
                                    Circle().fill(stageColor[item.stage] ?? .gray).frame(width: 9, height: 9)
                                    Text(item.stage.capitalized).font(.footnote)
                                    Spacer()
                                    Text("\(pct)%").font(.footnote).foregroundColor(.secondary)
                                }
                            }
                        }
                    }

                    Button(action: { Task { await ring.syncHistory() } }) {
                        Text(ring.syncing ? "Syncing…" : "Sync history")
                            .frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.borderedProminent).tint(.indigo)
                    .disabled(ring.state != .ready || ring.syncing)
                }
                .padding()
            }
            .navigationTitle("Sleep")
        }
    }
}

/// Renders the stage array as a stepped hypnogram (deep low, awake high).
struct HypnogramView: View {
    let stages: [String]
    let color: [String: Color]
    private let level: [String: Int] = ["deep": 0, "light": 1, "rem": 2, "awake": 3]

    var body: some View {
        GeometryReader { geo in
            let n = max(stages.count, 1)
            let w = geo.size.width / CGFloat(n)
            let h = geo.size.height
            ForEach(Array(stages.enumerated()), id: \.offset) { i, s in
                let lvl = level[s] ?? 1
                let barH = h * CGFloat(lvl + 1) / 4.0
                Rectangle()
                    .fill(color[s] ?? .gray)
                    .frame(width: max(w, 1), height: barH)
                    .position(x: w * (CGFloat(i) + 0.5), y: h - barH / 2)
            }
        }
    }
}
