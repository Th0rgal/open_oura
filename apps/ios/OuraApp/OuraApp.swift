import SwiftUI

// ── the shared build_summary() JSON, decoded (same contract as the web client) ──
struct Trend: Decodable {
    var series: [Double] = []
    var latest: Double? = nil
    var baseline: Double? = nil
    var delta_pct: Double? = nil
}
struct Vitals: Decodable { var hrv = Trend(); var rhr = Trend() }
struct NightRow: Decodable, Identifiable {
    var date: String?; var start: String?; var end: String?
    var in_bed_h: Double?; var hrv_ms: Double?; var rhr: Double?
    var skin_temp: Double?; var spo2_mean: Double?
    // model-derived (present once the hypnogram runner is wired): per-30s stage codes
    // 1=deep 2=light 3=rem 4=wake, the stage percentages, and efficiency.
    var deep_pct: Double?; var light_pct: Double?; var rem_pct: Double?
    var wake_pct: Double?; var efficiency: Double?
    var stages: [Int]? = nil
    var id: String { (date ?? "") + (start ?? "") }
    var hasHypnogram: Bool { (stages?.count ?? 0) > 1 }
}
struct DailyStat: Decodable { var active_kcal: Double?; var total_kcal: Double?; var steps: Double? }
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
    var activity_profile: [String: [Double]] = [:]   // date → 96 × 15-min mean MET-above-rest
    var activity_daily: [String: DailyStat] = [:]     // date → steps / active-kcal / total-kcal
    var error: String?
    /// recent days (newest first) that have a movement profile.
    var activeDays: [String] { activity_profile.keys.sorted(by: >) }
}

enum Core {
    static func summary() -> Summary {
        guard let path = Bundle.main.path(forResource: "oura", ofType: "db") else {
            return Summary(error: "oura.db not in bundle")
        }
        // the phone's actual UTC offset, so night labels / sleep windows / digest
        // timing match the wearer's local clock — not a hardcoded constant. The whole
        // stack (web --tz-offset, the Python model runners, this FFI) takes whole
        // hours, so round to the nearest hour (best representable value for the rare
        // sub-hour zones like IST +5:30).
        let secs = TimeZone.current.secondsFromGMT()
        let tzOffset = Int64((Double(secs) / 3600).rounded())
        let json = summaryJson(dbPath: path, tzOffset: tzOffset)
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

// A tappable night summary — opens the detail sheet.
struct SleepRow: View {
    let n: NightRow
    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(n.date ?? "—").font(Obs.mono(13, .medium)).foregroundStyle(Obs.ink)
                Text("\(n.start ?? "—")→\(n.end ?? "—") · \(n.in_bed_h.map { String(format: "%.1fh", $0) } ?? "—")")
                    .font(Obs.mono(11)).foregroundStyle(Obs.ink2)
            }
            Spacer(minLength: 8)
            if n.hasHypnogram { Hypnogram(stages: n.stages!, height: 22).frame(width: 120) }
            else if let e = n.efficiency { Text("\(Int(e))%").font(Obs.mono(13)).foregroundStyle(Obs.ink2) }
            Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
        }
        .contentShape(Rectangle())
    }
}

// The detail sheet: hypnogram (or a note when on-device staging isn't available yet)
// + stage breakdown + that night's vitals.
struct SleepDetail: View {
    let n: NightRow
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        ZStack {
            Obs.black.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(n.date ?? "Sleep").font(Obs.prose(19, .semibold)).foregroundStyle(Obs.ink)
                            Text("\(n.start ?? "—") → \(n.end ?? "—") · \(n.in_bed_h.map { String(format: "%.1f h in bed", $0) } ?? "—")")
                                .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                        }
                        Spacer()
                        Button { dismiss() } label: {
                            Image(systemName: "xmark").font(.system(size: 13, weight: .medium)).foregroundStyle(Obs.ink2)
                        }
                    }

                    ObsTag("hypnogram")
                    if n.hasHypnogram {
                        Hypnogram(stages: n.stages!)
                        StageBreakdown(deep: n.deep_pct ?? 0, light: n.light_pct ?? 0,
                                       rem: n.rem_pct ?? 0, wake: n.wake_pct ?? 0)
                    } else {
                        Text("On-device sleep staging is computed by the SleepNet model, which runs once the on-device torch runner is wired (it powers the web dashboard today). Signal-derived vitals for this night are below.")
                            .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    ObsTag("that night")
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
                .padding(24)
            }
        }
        .preferredColorScheme(.dark)
    }
}

// ── root ─────────────────────────────────────────────────────────────────────
struct RootView: View {
    private let s = Core.summary()
    @State private var sheetNight: NightRow?
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

                        // sleep — every night, tap for the hypnogram + breakdown + vitals
                        if !s.nights.isEmpty {
                            ObsTag("sleep · tap a night")
                            VStack(spacing: 14) {
                                ForEach(s.nights) { n in
                                    Button { sheetNight = n } label: { SleepRow(n: n) }
                                        .buttonStyle(.plain)
                                }
                            }
                        }

                        // activity — the movement ridge (MET) + steps / calories per day
                        if !s.activeDays.isEmpty {
                            ObsTag("activity")
                            VStack(spacing: 18) {
                                ForEach(s.activeDays.prefix(5), id: \.self) { day in
                                    VStack(alignment: .leading, spacing: 6) {
                                        HStack {
                                            Text(day).font(Obs.mono(12, .medium)).foregroundStyle(Obs.ink2)
                                            Spacer()
                                            if let st = s.activity_daily[day] {
                                                Text("\(Int((st.steps ?? 0))) steps")
                                                    .font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                                                Text("· \(Int((st.active_kcal ?? 0))) kcal")
                                                    .font(Obs.mono(11)).foregroundStyle(Obs.teal)
                                            }
                                        }
                                        MovementRidge(profile: s.activity_profile[day] ?? [])
                                    }
                                }
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
        .sheet(item: $sheetNight) { SleepDetail(n: $0) }
    }
}

@main
struct OuraApp: App {
    var body: some Scene { WindowGroup { RootView() } }
}
