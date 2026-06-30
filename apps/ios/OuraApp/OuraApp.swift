import SwiftUI

// ── the shared build_summary() JSON, decoded (same contract as the web client) ──
struct Trend: Decodable {
    var series: [Double] = []
    var latest: Double? = nil
    var baseline: Double? = nil
    var delta_pct: Double? = nil
}
struct Vitals: Decodable { var hrv = Trend(); var rhr = Trend() }
struct NightRow: Decodable {
    var date: String?; var start: String?; var end: String?
    var in_bed_h: Double?; var hrv_ms: Double?; var rhr: Double?
    var skin_temp: Double?; var spo2_mean: Double?
}
struct Stream: Decodable { let name: String; let count: Int }
struct Device: Decodable {
    var serial: String?; var firmware: String?
    var battery_pct: Int?; var fresh_hours: Double?
    var days_of_data: Double?; var nights: Int?; var total_events: Int?
    var synced: String?; var synced_hm: String?
    var streams: [Stream] = []
}
struct Summary: Decodable {
    var digest: String?
    var device: Device?
    var nights: [NightRow] = []
    var vitals = Vitals()
    var error: String?
}

enum Core {
    static func summary() -> Summary {
        guard let path = Bundle.main.path(forResource: "oura", ofType: "db") else {
            return Summary(error: "oura.db not in bundle")
        }
        let json = summaryJson(dbPath: path, tzOffset: 1)   // same tz default as the web dashboard
        guard let data = json.data(using: .utf8),
              let s = try? JSONDecoder().decode(Summary.self, from: data)
        else { return Summary(error: "decode failed") }
        return s
    }
}

// ── components ────────────────────────────────────────────────────────────────
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

// ── root ─────────────────────────────────────────────────────────────────────
struct RootView: View {
    private let s = Core.summary()
    private func f(_ v: Double?, _ fallback: String = "—") -> String {
        v.map { "\(Int($0))" } ?? fallback
    }
    var body: some View {
        let latest = s.nights.first
        ZStack {
            Obs.black.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    HStack {
                        Text("open_oura").font(Obs.prose(20, .semibold)).foregroundStyle(Obs.ink)
                        Text("BETA").font(Obs.mono(9, .bold)).tracking(1).foregroundStyle(Obs.ink2)
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .overlay(RoundedRectangle(cornerRadius: 5).stroke(Obs.trace, lineWidth: 0.8))
                        Spacer()
                    }

                    if let err = s.error {
                        ObsTag("no data"); Text(err).font(Obs.mono(13)).foregroundStyle(Obs.yellow)
                    } else {
                        // digest headline
                        if let d = s.digest {
                            Text(d).font(Obs.prose(16, .regular)).foregroundStyle(Obs.ink)
                                .fixedSize(horizontal: false, vertical: true)
                        }

                        NightOrbit(seed: Int(s.vitals.hrv.latest ?? 4))

                        // vitals
                        ObsTag("vitals · last night")
                        HStack(alignment: .top, spacing: 24) {
                            VitalCell(tag: "hrv", value: f(s.vitals.hrv.latest), unit: "ms",
                                      delta: s.vitals.hrv.delta_pct, series: s.vitals.hrv.series)
                            VitalCell(tag: "resting hr", value: f(s.vitals.rhr.latest), unit: "bpm",
                                      delta: s.vitals.rhr.delta_pct, series: s.vitals.rhr.series,
                                      deltaGoodWhenPositive: false)
                        }
                        HStack(alignment: .top, spacing: 24) {
                            VitalCell(tag: "skin temp",
                                      value: latest?.skin_temp.map { String(format: "%.1f", $0) } ?? "—",
                                      unit: "°c")
                            VitalCell(tag: "blood o₂", value: f(latest?.spo2_mean), unit: "%")
                        }

                        // sleep timing (signal-derived; hypnogram arrives with the torch runner)
                        if let n = latest {
                            ObsTag("sleep · \(n.date ?? "")")
                            VStack(spacing: 12) {
                                ObsStat(label: "time in bed",
                                        value: n.in_bed_h.map { String(format: "%.1f h", $0) } ?? "—",
                                        accent: Obs.teal)
                                ObsStat(label: "window", value: "\(n.start ?? "—") → \(n.end ?? "—")")
                            }
                        }

                        // device & data health
                        ObsTag("device & data health")
                        VStack(spacing: 12) {
                            ObsStat(label: "serial", value: s.device?.serial ?? "—")
                            ObsStat(label: "firmware", value: s.device?.firmware ?? "—")
                            ObsStat(label: "battery",
                                    value: s.device?.battery_pct.map { "\($0)%" } ?? "—",
                                    accent: Obs.teal)
                            ObsStat(label: "synced",
                                    value: s.device.flatMap { d in d.synced.map { "\($0) \(d.synced_hm ?? "")" } } ?? "—")
                            ObsStat(label: "days of data",
                                    value: s.device?.days_of_data.map { String(format: "%.0f", $0) } ?? "—")
                            ObsStat(label: "nights", value: "\(s.device?.nights ?? s.nights.count)")
                        }
                    }
                }
                .padding(24).padding(.top, 8)
            }
        }
        .preferredColorScheme(.dark)
    }
}

@main
struct OuraApp: App {
    var body: some Scene { WindowGroup { RootView() } }
}
