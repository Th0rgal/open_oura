import SwiftUI

struct SleepView: View {
    @EnvironmentObject var ring: OuraRing

    private func stageColor(_ s: String) -> Color {
        switch s {
        case "deep": return Brand.sleepDeep
        case "light": return Brand.sleepLight
        case "rem": return Brand.sleepRem
        default: return Brand.sleepAwake
        }
    }

    var body: some View {
        TabScaffold(title: "Sleep") {
            if ring.health.hypnogram.isEmpty {
                Card {
                    VStack(alignment: .leading, spacing: 8) {
                        Label("No sleep stages yet", systemImage: "moon.zzz.fill").font(.headline)
                        Text("Wear the ring overnight, then Sync. Sleep stages are computed on the ring and arrive as history events — no Oura cloud needed.")
                            .font(.footnote).foregroundStyle(Brand.dim)
                    }
                }
                .padding(.horizontal, 16)
            } else {
                Card {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("HYPNOGRAM").font(.caption2.weight(.semibold)).foregroundStyle(Brand.sleepRem)
                            Spacer()
                            Text("\(ring.health.hypnogram.count) epochs").font(.caption2).foregroundStyle(Brand.dim)
                        }
                        HypnogramView(stages: ring.health.hypnogram, color: stageColor)
                            .frame(height: 130)
                        HStack(spacing: 14) {
                            ForEach(["deep", "rem", "light", "awake"], id: \.self) { s in
                                HStack(spacing: 4) {
                                    Circle().fill(stageColor(s)).frame(width: 8, height: 8)
                                    Text(s.capitalized).font(.caption2).foregroundStyle(Brand.dim)
                                }
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)

                Card {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("STAGE DISTRIBUTION").font(.caption2.weight(.semibold)).foregroundStyle(Brand.sleepRem)
                        ForEach(ring.health.stageCounts, id: \.stage) { item in
                            let pct = Double(item.count) / Double(max(ring.health.hypnogram.count, 1))
                            VStack(alignment: .leading, spacing: 4) {
                                HStack {
                                    Text(item.stage.capitalized).font(.footnote)
                                    Spacer()
                                    Text("\(Int(pct * 100))%").font(.footnote.weight(.semibold)).foregroundStyle(Brand.dim)
                                }
                                GeometryReader { geo in
                                    ZStack(alignment: .leading) {
                                        Capsule().fill(Brand.line).frame(height: 8)
                                        Capsule().fill(stageColor(item.stage)).frame(width: geo.size.width * pct, height: 8)
                                    }
                                }.frame(height: 8)
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
            }

            Button(action: { Task { await ring.syncHistory() } }) {
                HStack {
                    if ring.syncing { ProgressView().tint(.white) }
                    Text(ring.syncing ? "Syncing…" : "Sync now")
                }.frame(maxWidth: .infinity).padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent).tint(Brand.sleepRem)
            .disabled(ring.state != .ready || ring.syncing)
            .padding(.horizontal, 16)
        }
    }
}

/// Stepped hypnogram (deep low → awake high).
struct HypnogramView: View {
    let stages: [String]
    let color: (String) -> Color
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
                    .fill(color(s))
                    .frame(width: max(w, 0.6), height: barH)
                    .position(x: w * (CGFloat(i) + 0.5), y: h - barH / 2)
            }
        }
    }
}
