import SwiftUI

// Activity type → a clean SF Symbol. Keyword-matched so the ~40 AAD behaviour labels all
// resolve to a sensible figure.* glyph; unknowns fall back to a neutral cardio symbol.
func activitySymbol(_ label: String) -> String {
    let l = label.lowercased()
    switch true {
    case l.contains("run"): return "figure.run"
    case l.contains("walk"): return "figure.walk"
    case l.contains("hik"): return "figure.hiking"
    case l.contains("cycl"), l.contains("bik"): return "figure.outdoor.cycle"
    case l.contains("swim"): return "figure.pool.swim"
    case l.contains("row"): return "figure.rower"
    case l.contains("core"): return "figure.core.training"
    case l.contains("strength"): return "figure.strengthtraining.traditional"
    case l.contains("cross train"): return "figure.cross.training"
    case l.contains("yoga"): return "figure.yoga"
    case l.contains("pilates"): return "figure.pilates"
    case l.contains("hiit"), l.contains("interval"): return "figure.highintensity.intervaltraining"
    case l.contains("elliptical"): return "figure.elliptical"
    case l.contains("box"): return "figure.boxing"
    case l.contains("martial"): return "figure.martial.arts"
    case l.contains("danc"): return "figure.dance"
    case l.contains("basketball"): return "figure.basketball"
    case l.contains("soccer"): return "figure.soccer"
    case l.contains("football"): return "figure.american.football"
    case l.contains("baseball"): return "figure.baseball"
    case l.contains("volleyball"): return "figure.volleyball"
    case l.contains("tennis"), l.contains("padel"), l.contains("badminton"): return "figure.tennis"
    case l.contains("hockey"): return "figure.hockey"
    case l.contains("surf"): return "figure.surfing"
    case l.contains("snowboard"): return "figure.snowboarding"
    case l.contains("ski"): return "figure.skiing.crosscountry"
    case l.contains("horse"): return "figure.equestrian.sports"
    case l.contains("stretch"): return "figure.flexibility"
    case l.contains("climb"): return "figure.climbing"
    case l.contains("golf"): return "figure.golf"
    case l.contains("meditat"): return "figure.mind.and.body"
    case l.contains("fitness"): return "figure.strengthtraining.functional"
    default: return "figure.mixed.cardio"
    }
}

// Capitalise an activity label's first letter for display.
func actLabel(_ s: String) -> String { s.isEmpty ? s : s.prefix(1).uppercased() + s.dropFirst() }

// A labelled activity/workout row: clean SF Symbol + name, duration + start time.
struct SessionRow: View {
    let label: String
    let durationMin: Int
    let startHM: String
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: activitySymbol(label)).font(.system(size: 14))
                .foregroundStyle(Obs.teal).frame(width: 20)
            Text(actLabel(label)).font(Obs.mono(13, .medium)).foregroundStyle(Obs.ink)
            Spacer()
            Text("\(durationMin) min").font(Obs.mono(12)).foregroundStyle(Obs.teal)
            Text(startHM).font(Obs.mono(11)).foregroundStyle(Obs.ink2)
        }
    }
}

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

