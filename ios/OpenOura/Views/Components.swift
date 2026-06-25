import SwiftUI

/// A lightweight Path-based sparkline (no external charting dependency).
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
                Path { p in
                    for (i, v) in vals.enumerated() {
                        let x = w * Double(i) / Double(vals.count - 1)
                        let y = h - (v - lo) / range * h
                        if i == 0 { p.move(to: CGPoint(x: x, y: y)) }
                        else { p.addLine(to: CGPoint(x: x, y: y)) }
                    }
                }
                .stroke(color, style: StrokeStyle(lineWidth: 2, lineJoin: .round))
            } else {
                Rectangle().fill(Color.white.opacity(0.04))
            }
        }
    }
}

struct MetricCard<Content: View>: View {
    let title: String
    var accent: Color = .accentColor
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title.uppercased())
                .font(.caption2.weight(.semibold))
                .foregroundColor(accent)
            content
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(white: 0.10))
        .clipShape(RoundedRectangle(cornerRadius: 14))
    }
}

/// Big value + unit, e.g. "72 bpm".
struct BigValue: View {
    let value: String
    var unit: String = ""
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Text(value).font(.system(size: 38, weight: .bold, design: .rounded))
            if !unit.isEmpty { Text(unit).font(.subheadline).foregroundColor(.secondary) }
        }
    }
}
