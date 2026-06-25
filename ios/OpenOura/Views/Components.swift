import SwiftUI

/// Rounded dark card container.
struct Card<Content: View>: View {
    var padding: CGFloat = 16
    @ViewBuilder var content: Content
    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Brand.card)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

/// App header: logo + title + last-synced subtitle.
struct AppHeader: View {
    @EnvironmentObject var ring: OuraRing
    let title: String
    var body: some View {
        HStack(spacing: 11) {
            RingLogo().frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.title2.bold())
                Text(subtitle).font(.caption2).foregroundStyle(Brand.dim)
            }
            Spacer()
            if ring.syncing { ProgressView().controlSize(.small) }
        }
        .padding(.horizontal, 18).padding(.top, 8)
    }
    private var subtitle: String {
        if ring.syncing { return "Syncing…" }
        if let d = ring.lastSync { return "Synced \(relativeTime(d))" }
        return ring.state == .ready ? "Connected" : "Not synced yet"
    }
}

/// Circular gauge (Oura-style). Maps `value` within `range` to an arc.
struct ScoreRing: View {
    let value: Double?
    var range: ClosedRange<Double> = 40...120
    var color: Color
    var caption: String
    var unit: String
    var pulse = false
    @State private var animatePulse = false

    var body: some View {
        let frac = value.map { min(max(($0 - range.lowerBound) / (range.upperBound - range.lowerBound), 0), 1) } ?? 0
        ZStack {
            Circle().stroke(Brand.line, lineWidth: 16)
            Circle()
                .trim(from: 0, to: CGFloat(frac))
                .stroke(color.gradient, style: StrokeStyle(lineWidth: 16, lineCap: .round))
                .rotationEffect(.degrees(-90))
                .shadow(color: color.opacity(0.5), radius: 8)
                .animation(.easeOut(duration: 0.6), value: frac)
            VStack(spacing: 0) {
                Text(value.map { String(Int($0.rounded())) } ?? "—")
                    .font(.system(size: 52, weight: .bold, design: .rounded))
                    .contentTransition(.numericText())
                Text(unit).font(.footnote).foregroundStyle(Brand.dim)
                Text(caption).font(.caption2.weight(.semibold)).foregroundStyle(color).padding(.top, 2)
            }
            .scaleEffect(pulse && animatePulse ? 1.04 : 1.0)
            .animation(pulse ? .easeInOut(duration: 0.6).repeatForever(autoreverses: true) : nil, value: animatePulse)
        }
        .onAppear { animatePulse = pulse }
    }
}

/// Small metric tile with optional sparkline.
struct MetricTile: View {
    let title: String
    let value: String
    var unit: String = ""
    var accent: Color
    var spark: [Double]? = nil

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 6) {
                Text(title.uppercased()).font(.caption2.weight(.semibold)).foregroundStyle(accent)
                HStack(alignment: .firstTextBaseline, spacing: 3) {
                    Text(value).font(.system(size: 26, weight: .bold, design: .rounded))
                    if !unit.isEmpty { Text(unit).font(.caption2).foregroundStyle(Brand.dim) }
                }
                if let spark, spark.count > 1 {
                    Sparkline(values: spark, color: accent).frame(height: 26)
                } else {
                    Spacer().frame(height: 0)
                }
            }
        }
    }
}

/// Lightweight Path-based sparkline (no external charting dependency).
struct Sparkline: View {
    var values: [Double]
    var color: Color
    var minY: Double? = nil
    var maxY: Double? = nil

    var body: some View {
        GeometryReader { geo in
            let vals = values
            if vals.count >= 2 {
                let lo = minY ?? (vals.min() ?? 0)
                let hi = maxY ?? (vals.max() ?? 1)
                let range = Swift.max(hi - lo, 0.0001)
                let w = geo.size.width, h = geo.size.height
                let pts = vals.enumerated().map { i, v in
                    CGPoint(x: w * Double(i) / Double(vals.count - 1), y: h - (v - lo) / range * h)
                }
                ZStack {
                    Path { p in
                        p.move(to: CGPoint(x: 0, y: h))
                        pts.forEach { p.addLine(to: $0) }
                        p.addLine(to: CGPoint(x: w, y: h))
                    }.fill(LinearGradient(colors: [color.opacity(0.25), color.opacity(0)], startPoint: .top, endPoint: .bottom))
                    Path { p in p.move(to: pts[0]); pts.dropFirst().forEach { p.addLine(to: $0) } }
                        .stroke(color, style: StrokeStyle(lineWidth: 2, lineJoin: .round))
                }
            }
        }
    }
}

func relativeTime(_ d: Date) -> String {
    let f = RelativeDateTimeFormatter()
    f.unitsStyle = .abbreviated
    return f.localizedString(for: d, relativeTo: Date())
}
