import SwiftUI

// SIBLING CLIENT: the web dashboard (dashboard/web/app.js) renders the SAME summary
// JSON. A user-facing change here usually belongs there too — see the feature map in
// docs/clients-web-and-ios.md. New computed fields go in crates/oura-summary; new
// models get an on-device path (TorchBridge.mm + *Model.swift) AND a Python runner.

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
struct Profile: Decodable { var sex: String?; var age: Double?; var height_m: Double?; var weight_kg: Double?; var ring_size: Double? }
// a detected activity session (on-device automatic_activity_detection)
struct WorkoutSession: Identifiable {
    let start: String; let end: String; let durationMin: Int; let label: String; let isWorkout: Double
    var id: String { start + label }
    var dayLabel: String { String(start.prefix(10)) }      // YYYY-MM-DD
    var startHM: String { String(start.suffix(5)) }        // HH:MM
}
struct Cardio: Decodable { var vascular_age: Double?; var chronological_age: Double?; var pwv_ms: Double?; var segments: Int? }
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
    var profile: Profile?
    var cardio: Cardio?
    var workouts: [WorkoutSession] = []   // on-device only (not in the JSON)
    var error: String?
    // `workouts` is filled on-device (not in the FFI JSON), so keep it out of decoding.
    enum CodingKeys: String, CodingKey {
        case digest, device, nights, vitals, activity_profile, activity_daily, profile, cardio, error
    }
    /// recent days (newest first) that have a movement profile.
    var activeDays: [String] { activity_profile.keys.sorted(by: >) }
}

enum Core {
    /// Fast, model-free summary (vitals, activity ridges, device) straight from the
    /// shared-core JSON — safe to compute on a background queue and show immediately.
    static func base() -> Summary {
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

    #if TORCH
    /// The slow part: run the three on-device torch models and fold their results into
    /// the summary. Call off the main thread (see RootView.load); never on launch.
    static func withModels(_ base: Summary) -> Summary {
        var s = base
        // run SleepNet on-device and fold the hypnogram + stage breakdown into each
        // night, so the app renders the same sleep diagrams as the web dashboard.
        let staged = SleepStaging.run()
        for i in s.nights.indices {
            guard let date = s.nights[i].date, let stages = staged[date], !stages.isEmpty else { continue }
            s.nights[i].stages = stages
            let total = Double(stages.count)
            let pct = { (code: Int) in (Double(stages.filter { $0 == code }.count) / total * 100).rounded() }
            s.nights[i].deep_pct = pct(1); s.nights[i].light_pct = pct(2)
            s.nights[i].rem_pct = pct(3); s.nights[i].wake_pct = pct(4)
            let asleep = total - Double(stages.filter { $0 == 4 }.count)
            s.nights[i].efficiency = (asleep / total * 100).rounded()
        }
        // cardiovascular age from the ring's raw PPG (cva_2_1_0), on-device
        if let cva = CvaModel.run(sex: s.profile?.sex ?? "M", age: s.profile?.age ?? 30,
                                  heightM: s.profile?.height_m ?? 1.78, weightKg: s.profile?.weight_kg ?? 75,
                                  ringSize: s.profile?.ring_size ?? 10) {
            s.cardio = Cardio(vascular_age: cva.vascularAge, chronological_age: s.profile?.age ?? 30,
                              pwv_ms: cva.pwv, segments: cva.segments)
        }
        // detected activity sessions (automatic_activity_detection), on-device
        s.workouts = ActivityModel.run()
        return s
    }
    #endif
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

extension Summary {
    // a "day" is an activity date (YYYY-MM-DD); the matching night is found by MM-DD.
    func night(forDay day: String) -> NightRow? {
        let mmdd = String(day.suffix(5))
        return nights.first { ($0.date ?? "").hasSuffix(mmdd) }
    }
    func workoutsOn(_ day: String) -> [WorkoutSession] {
        workouts.filter { $0.isWorkout >= 0.5 && $0.dayLabel == day }
    }
}

// One day's activity: movement ridge + steps/kcal + that day's workouts.
struct DaySummaryView: View {
    let s: Summary
    let day: String
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(day).font(Obs.mono(12, .medium)).foregroundStyle(Obs.ink2)
                Spacer()
                if let st = s.activity_daily[day] {
                    Text("\(Int(st.steps ?? 0)) steps").font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                    Text("· \(Int(st.active_kcal ?? 0)) kcal").font(Obs.mono(11)).foregroundStyle(Obs.teal)
                }
            }
            MovementRidge(profile: s.activity_profile[day] ?? [])
            ForEach(s.workoutsOn(day)) { w in
                HStack {
                    Text(w.label.prefix(1).uppercased() + w.label.dropFirst())
                        .font(Obs.mono(13, .medium)).foregroundStyle(Obs.ink)
                    Spacer()
                    Text("\(w.durationMin) min").font(Obs.mono(12)).foregroundStyle(Obs.teal)
                    Text(w.startHM).font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                }
            }
        }
    }
}

// "show all days" → a page listing every day; tap one for its full detail.
struct AllDaysView: View {
    let s: Summary
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ZStack {
                Obs.black.ignoresSafeArea()
                ScrollView {
                    VStack(spacing: 14) {
                        ForEach(s.activeDays, id: \.self) { day in
                            NavigationLink {
                                DayDetailView(s: s, day: day)
                            } label: {
                                HStack(spacing: 12) {
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text(day).font(Obs.mono(13, .medium)).foregroundStyle(Obs.ink)
                                        if let st = s.activity_daily[day] {
                                            Text("\(Int(st.steps ?? 0)) steps · \(Int(st.active_kcal ?? 0)) kcal")
                                                .font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                                        }
                                    }
                                    Spacer(minLength: 8)
                                    if let n = s.night(forDay: day), n.hasHypnogram {
                                        Hypnogram(stages: n.stages!, height: 20).frame(width: 96)
                                    }
                                    Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(24)
                }
            }
            .navigationTitle("all days")
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .toolbarColorScheme(.dark, for: .navigationBar)
        }
        .preferredColorScheme(.dark)
    }
}

// One day in full: that night's sleep (hypnogram + breakdown + vitals) and the day's
// activity (ridge + workouts).
struct DayDetailView: View {
    let s: Summary
    let day: String
    var body: some View {
        ZStack {
            Obs.black.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if let n = s.night(forDay: day) {
                        ObsTag("sleep")
                        Text("\(n.start ?? "—") → \(n.end ?? "—") · \(n.in_bed_h.map { String(format: "%.1f h", $0) } ?? "—")")
                            .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                        if n.hasHypnogram {
                            Hypnogram(stages: n.stages!)
                            StageBreakdown(deep: n.deep_pct ?? 0, light: n.light_pct ?? 0, rem: n.rem_pct ?? 0, wake: n.wake_pct ?? 0)
                        }
                        let cells: [(String, String)] = [
                            ("hrv", n.hrv_ms.map { "\(Int($0)) ms" } ?? "—"),
                            ("resting hr", n.rhr.map { "\(Int($0)) bpm" } ?? "—"),
                            ("skin temp", n.skin_temp.map { String(format: "%.1f °c", $0) } ?? "—"),
                            ("blood o₂", n.spo2_mean.map { "\(Int($0))%" } ?? "—"),
                            ("efficiency", n.efficiency.map { "\(Int($0))%" } ?? "—"),
                        ]
                        VStack(spacing: 12) { ForEach(cells, id: \.0) { ObsStat(label: $0.0, value: $0.1) } }
                    }
                    ObsTag("activity")
                    DaySummaryView(s: s, day: day)
                }
                .padding(24)
            }
        }
        .navigationTitle(day)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarColorScheme(.dark, for: .navigationBar)
        .preferredColorScheme(.dark)
    }
}

// ── root ─────────────────────────────────────────────────────────────────────
struct RootView: View {
    @State private var s: Summary?
    @State private var sheetNight: NightRow?
    @State private var showAllDays = false
    private func f(_ v: Double?, _ fallback: String = "—") -> String {
        v.map { "\(Int($0))" } ?? fallback
    }
    private func relAge(_ diff: Double) -> String {
        let a = abs((diff * 10).rounded() / 10)
        if diff < -0.05 { return "\(a) yr younger" }
        if diff > 0.05 { return "\(a) yr older" }
        return "in line"
    }
    var body: some View {
        ZStack {
            Obs.black.ignoresSafeArea()
            if let s {
                content(s)
            } else {
                VStack(spacing: 14) {
                    ProgressView().tint(Obs.teal)
                    Text("reading your ring…").font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                }
            }
        }
        .preferredColorScheme(.dark)
        .sheet(item: $sheetNight) { SleepDetail(n: $0) }
        .sheet(isPresented: $showAllDays) { if let s { AllDaysView(s: s) } }
        .onAppear(perform: load)
    }

    // The heavy on-device models run off the main thread (load): show the fast
    // model-free summary first, then fold in the hypnogram / CVA / activity results.
    private func load() {
        guard s == nil else { return }
        DispatchQueue.global(qos: .userInitiated).async {
            let base = Core.base()
            DispatchQueue.main.async { if s == nil { s = base } }
            #if TORCH
            if base.error == nil {
                let full = Core.withModels(base)
                DispatchQueue.main.async { s = full }
            }
            #endif
        }
    }

    @ViewBuilder private func content(_ s: Summary) -> some View {
        let latest = s.nights.first
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

                        // hero: the most recent day's real movement profile (model-free)
                        if let day = s.activeDays.first, let prof = s.activity_profile[day], prof.count > 1 {
                            VStack(alignment: .leading, spacing: 8) {
                                MovementRidge(profile: prof, height: 132)
                                HStack(spacing: 8) {
                                    Text("\(day) · movement").font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                                    Spacer()
                                    if let st = s.activity_daily[day] {
                                        Text("\(Int(st.steps ?? 0)) steps").font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                                        Text("· \(Int(st.active_kcal ?? 0)) kcal").font(Obs.mono(11)).foregroundStyle(Obs.teal)
                                    }
                                }
                            }
                        } else {
                            NightOrbit(seed: Int(s.vitals.hrv.latest ?? 4))
                        }

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

                        // cardiovascular age (on-device CVA model, from raw PPG)
                        if let cv = s.cardio, let va = cv.vascular_age {
                            ObsTag("cardiovascular")
                            VStack(spacing: 12) {
                                ObsStat(label: "vascular age", value: String(format: "%.1f yr", va), accent: Obs.teal)
                                if let ca = cv.chronological_age { ObsStat(label: "vs your age", value: relAge(va - ca)) }
                                if let pwv = cv.pwv_ms { ObsStat(label: "pulse-wave velocity", value: String(format: "%.2f m/s", pwv)) }
                                if let seg = cv.segments { ObsStat(label: "segments analysed", value: "\(seg)") }
                            }
                        }

                        // last night — tap for the hypnogram + breakdown + vitals
                        if let n = s.nights.first {
                            ObsTag("last night")
                            Button { sheetNight = n } label: { SleepRow(n: n) }.buttonStyle(.plain)
                        }

                        // activity (movement ridge + steps/kcal + workouts) of the most
                        // recent day, merged into one section
                        if let day = s.activeDays.first {
                            ObsTag("activity")
                            DaySummaryView(s: s, day: day)
                        }

                        // browse every day → per-day detail (sleep + activity)
                        if !s.activeDays.isEmpty {
                            Button { showAllDays = true } label: {
                                HStack {
                                    Text("show all \(s.activeDays.count) days").font(Obs.mono(12, .medium)).foregroundStyle(Obs.teal)
                                    Spacer()
                                    Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
                                }.contentShape(Rectangle())
                            }.buttonStyle(.plain)
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
}

@main
struct OuraApp: App {
    var body: some Scene { WindowGroup { RootView() } }
}
