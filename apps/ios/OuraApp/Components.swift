import SwiftUI

// ── reusable readout + chart components ───────────────────────────────────────
struct Sparkline: View {
    let series: [Double]
    var accent: Color = Obs.teal
    var body: some View {
        Canvas { ctx, size in
            guard series.count > 1 else { return }
            let lo = series.min()!, hi = series.max()!
            let span = max(hi - lo, 1e-6)
            var p = Path()
            for (i, v) in series.enumerated() {
                let x = size.width * CGFloat(i) / CGFloat(series.count - 1)
                let y = size.height * (1 - CGFloat((v - lo) / span))
                i == 0 ? p.move(to: .init(x: x, y: y)) : p.addLine(to: .init(x: x, y: y))
            }
            ctx.stroke(p, with: .color(accent), style: .init(lineWidth: 1.2, lineJoin: .round))
        }
        .frame(height: 26)
    }
}

// A vitals readout: big mono value, unit, delta vs baseline, sparkline.
struct VitalCell: View {
    let tag: String
    let value: String
    let unit: String
    var delta: Double? = nil
    var series: [Double] = []
    var deltaGoodWhenPositive = true
    var body: some View {
        let good = (delta ?? 0) >= 0 ? deltaGoodWhenPositive : !deltaGoodWhenPositive
        VStack(alignment: .leading, spacing: 6) {
            ObsTag(tag)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(value).font(Obs.mono(26, .medium)).foregroundStyle(Obs.ink).monospacedDigit()
                Text(unit).font(Obs.mono(11)).foregroundStyle(Obs.ink2)
            }
            if let d = delta {
                Text("\(d >= 0 ? "+" : "")\(d, specifier: "%.0f")% vs base")
                    .font(Obs.mono(10))
                    .foregroundStyle(good ? Obs.teal : Obs.yellow)
            } else {
                Text("—").font(Obs.mono(10)).foregroundStyle(Obs.ink2)
            }
            if series.count > 1 { Sparkline(series: series, accent: good ? Obs.teal : Obs.yellow) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct NightOrbit: View {
    var seed: Int
    var body: some View {
        Canvas { ctx, size in
            let c = CGPoint(x: size.width / 2, y: size.height / 2)
            let maxR = min(size.width, size.height) / 2 - 10
            for k in 1...4 {
                var ring = Path()
                ring.addArc(center: c, radius: maxR * CGFloat(k) / 4, startAngle: .degrees(0),
                            endAngle: .degrees(360), clockwise: false)
                ctx.stroke(ring, with: .color(Obs.trace), style: .init(lineWidth: 0.6, dash: [2, 5]))
            }
            var trace = Path(); trace.move(to: c)
            var rng = seed == 0 ? 1 : seed
            for i in 1...7 {
                rng = (rng &* 1103515245 &+ 12345) & 0x7fffffff
                let ang = Double(rng % 360) * .pi / 180
                let r = maxR * CGFloat(i) / 7
                let p = CGPoint(x: c.x + cos(ang) * r, y: c.y + sin(ang) * r)
                trace.addLine(to: p)
                ctx.fill(Path(ellipseIn: CGRect(x: p.x - 2.5, y: p.y - 2.5, width: 5, height: 5)),
                         with: .color(Obs.ink))
                ctx.stroke(Path(ellipseIn: CGRect(x: p.x - 4, y: p.y - 4, width: 8, height: 8)),
                           with: .color(Obs.teal.opacity(0.5)), lineWidth: 0.6)
            }
            ctx.stroke(trace, with: .color(Obs.ink2), lineWidth: 0.8)
        }
        .frame(height: 200)
    }
}

// Sleep-stage hypnogram: a strip of colored segments over the night (1=deep …
// 4=wake), matching the web dashboard's `.hyp`. Renders whenever stage data exists.
struct Hypnogram: View {
    let stages: [Int]
    var height: CGFloat = 40
    var body: some View {
        Canvas { ctx, size in
            guard !stages.isEmpty else { return }
            let w = size.width / CGFloat(stages.count)
            for (i, s) in stages.enumerated() {
                let r = CGRect(x: CGFloat(i) * w, y: 0, width: w + 0.6, height: size.height)
                ctx.fill(Path(r), with: .color(Obs.stage(s).opacity(0.85)))
            }
        }
        .frame(height: height)
        .clipShape(RoundedRectangle(cornerRadius: 4))
    }
}

// Deep / Light / REM / Awake proportion bar + legend.
struct StageBreakdown: View {
    let deep: Double, light: Double, rem: Double, wake: Double
    private var parts: [(String, Double, Color)] {
        [("Deep", deep, Obs.deep), ("Light", light, Obs.light), ("REM", rem, Obs.rem), ("Awake", wake, Obs.wake)]
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            GeometryReader { geo in
                HStack(spacing: 1.5) {
                    ForEach(parts, id: \.0) { p in
                        Rectangle().fill(p.2)
                            .frame(width: max(0, geo.size.width * CGFloat(p.1 / 100) - 1.5))
                    }
                    Spacer(minLength: 0)
                }
            }
            .frame(height: 8).clipShape(Capsule())
            HStack(spacing: 14) {
                ForEach(parts, id: \.0) { p in
                    HStack(spacing: 5) {
                        Circle().fill(p.2).frame(width: 7, height: 7)
                        Text("\(p.0) ").font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                            + Text("\(Int(p.1))%").font(Obs.mono(11, .medium)).foregroundStyle(Obs.ink)
                    }
                }
            }
        }
    }
}

// Continuous movement ridge from the 96 × 15-min MET-above-rest buckets — the web
// actogram's ridge, model-free (computed from raw MET). One day's profile.
struct MovementRidge: View {
    let profile: [Double]
    var height: CGFloat = 44
    var body: some View {
        Canvas { ctx, size in
            guard profile.count > 1 else { return }
            let peak = max(profile.max() ?? 1, 0.5)
            let n = profile.count
            func pt(_ i: Int) -> CGPoint {
                CGPoint(x: size.width * CGFloat(i) / CGFloat(n - 1),
                        y: size.height * (1 - CGFloat(min(1, profile[i] / peak))))
            }
            var area = Path(); area.move(to: CGPoint(x: 0, y: size.height))
            for i in 0..<n { area.addLine(to: pt(i)) }
            area.addLine(to: CGPoint(x: size.width, y: size.height)); area.closeSubpath()
            ctx.fill(area, with: .color(Obs.teal.opacity(0.16)))
            var line = Path(); line.move(to: pt(0))
            for i in 1..<n { line.addLine(to: pt(i)) }
            ctx.stroke(line, with: .color(Obs.teal.opacity(0.8)), style: .init(lineWidth: 1.2, lineJoin: .round))
        }
        .frame(height: height)
    }
}

// Hypnogram + stage breakdown for a night — renders nothing when on-device staging
// isn't available. Shared by the sleep-detail and day-detail sheets.
struct SleepStages: View {
    let n: NightRow
    var body: some View {
        if n.hasHypnogram {
            Hypnogram(stages: n.stages!)
            StageBreakdown(deep: n.deep_pct ?? 0, light: n.light_pct ?? 0,
                           rem: n.rem_pct ?? 0, wake: n.wake_pct ?? 0)
        }
    }
}

// The five signal-derived vitals for a night, shared by both detail sheets.
struct NightVitals: View {
    let n: NightRow
    var body: some View {
        let cells: [(String, String)] = [
            ("hrv", n.hrv_ms.map { "\(Int($0)) ms" } ?? "—"),
            ("resting hr", n.rhr.map { "\(Int($0)) bpm" } ?? "—"),
            ("skin temp", n.skin_temp.map { String(format: "%.1f °c", $0) } ?? "—"),
            ("blood o₂", n.spo2_mean.map { "\(Int($0))%" } ?? "—"),
            ("efficiency", n.efficiency.map { "\(Int($0))%" } ?? "—"),
        ]
        VStack(spacing: 12) {
            ForEach(cells, id: \.0) { ObsStat(label: $0.0, value: $0.1) }
        }
    }
}
