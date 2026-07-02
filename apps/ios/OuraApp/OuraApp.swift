import SwiftUI

// The SwiftUI screens for OuraApp. Data types live in Models.swift, the model/FFI
// orchestration in Core.swift, and the reusable charts/cells in Components.swift.
// SIBLING CLIENT: the web dashboard (dashboard/web/app.js) renders the SAME summary
// JSON — a user-facing change here usually belongs there too (docs/clients-web-and-ios.md).

// The detail sheet: hypnogram (or a note when on-device staging isn't available yet)
// + stage breakdown + that night's vitals.
struct SleepDetail: View {
    let n: NightRow
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        ZStack {
            Obs.canvas.ignoresSafeArea()
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

                    ObsTag("hypnogram", icon: "moon.stars.fill")
                    if n.hasHypnogram {
                        SleepStages(n: n)
                    } else {
                        Text("On-device sleep staging is computed by the SleepNet model, which runs once the on-device torch runner is wired (it powers the web dashboard today). Signal-derived vitals for this night are below.")
                            .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    ObsTag("that night", icon: "waveform.path.ecg")
                    NightVitals(n: n)
                }
                .padding(24)
            }
        }
        .preferredColorScheme(.dark)
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

// The home's unified "today": last night's sleep and that day's activity as ONE unit,
// each region tappable to open its own detail (sleep → SleepDetail, activity →
// ActivityDetail). Mirrors the web dashboard's day card. Previous days live behind
// "show all days" (AllDaysView → DayDetailView, which shows the same pairing).
struct TodayCard: View {
    let s: Summary
    let day: String
    let onSleep: () -> Void
    let onActivity: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(day).font(Obs.mono(12, .medium)).foregroundStyle(Obs.ink)
                .padding(.bottom, 14)

            // night — tap for the hypnogram + breakdown + that night's vitals
            if let n = s.night(forDay: day) {
                Button(action: onSleep) {
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            ObsTag("sleep", icon: "moon.fill")
                            Spacer()
                            Text(n.in_bed_h.map { String(format: "%.1fh", $0) } ?? "—")
                                .font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                            Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
                        }
                        Text("\(n.start ?? "—") → \(n.end ?? "—")")
                            .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                        if n.hasHypnogram { Hypnogram(stages: n.stages!, height: 26) }
                        else if let e = n.efficiency {
                            Text("efficiency \(Int(e))%").font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                        }
                    }
                    .contentShape(Rectangle())
                }.buttonStyle(.plain)

                Rectangle().fill(Obs.trace.opacity(0.4)).frame(height: 0.5)
                    .padding(.vertical, 16)
            }

            // activity — tap for the movement ridge + steps/kcal + this day's workouts
            Button(action: onActivity) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        ObsTag("activity", icon: "figure.walk")
                        Spacer()
                        if let st = s.activity_daily[day] {
                            Text("\(Int(st.steps ?? 0)) steps").font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                            Text("· \(Int(st.active_kcal ?? 0)) kcal").font(Obs.mono(11)).foregroundStyle(Obs.teal)
                        }
                        Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
                    }
                    MovementRidge(profile: s.activity_profile[day] ?? [])
                    ForEach(Array(s.workoutsOn(day).prefix(2))) { w in
                        HStack {
                            Text(w.label.prefix(1).uppercased() + w.label.dropFirst())
                                .font(Obs.mono(12, .medium)).foregroundStyle(Obs.ink)
                            Spacer()
                            Text("\(w.durationMin) min").font(Obs.mono(11)).foregroundStyle(Obs.teal)
                            Text(w.startHM).font(Obs.mono(11)).foregroundStyle(Obs.ink2)
                        }
                    }
                }
                .contentShape(Rectangle())
            }.buttonStyle(.plain)
        }
        .obsCard()
    }
}

// The activity-only detail sheet for a day: movement ridge + workouts + totals — the
// activity counterpart to SleepDetail.
struct ActivityDetail: View {
    let s: Summary
    let day: String
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        ZStack {
            Obs.canvas.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(day).font(Obs.prose(19, .semibold)).foregroundStyle(Obs.ink)
                            if let st = s.activity_daily[day] {
                                Text("\(Int(st.steps ?? 0)) steps · \(Int(st.active_kcal ?? 0)) kcal active")
                                    .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                            }
                        }
                        Spacer()
                        Button { dismiss() } label: {
                            Image(systemName: "xmark").font(.system(size: 13, weight: .medium)).foregroundStyle(Obs.ink2)
                        }
                    }

                    ObsTag("movement", icon: "waveform.path")
                    MovementRidge(profile: s.activity_profile[day] ?? [], height: 96)

                    let ws = s.workoutsOn(day)
                    if !ws.isEmpty {
                        ObsTag("sessions", icon: "figure.run")
                        VStack(spacing: 12) {
                            ForEach(ws) { w in
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

                    if let st = s.activity_daily[day] {
                        ObsTag("totals", icon: "sum")
                        VStack(spacing: 12) {
                            ObsStat(label: "steps", value: "\(Int(st.steps ?? 0))")
                            if let dm = st.distance_m { ObsStat(label: "distance", value: String(format: "%.1f km", dm / 1000)) }
                            ObsStat(label: "active energy", value: "\(Int(st.active_kcal ?? 0)) kcal", accent: Obs.teal)
                            if let tk = st.total_kcal { ObsStat(label: "total energy", value: "\(Int(tk)) kcal") }
                        }
                    }
                }
                .padding(24)
            }
        }
        .preferredColorScheme(.dark)
    }
}

// "show all days" → a page listing every day; tap one for its full detail.
struct AllDaysView: View {
    let s: Summary
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ZStack {
                Obs.canvas.ignoresSafeArea()
                ScrollView {
                    VStack(spacing: 14) {
                        ForEach(s.days, id: \.self) { day in
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
            Obs.canvas.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if let n = s.night(forDay: day) {
                        ObsTag("sleep", icon: "moon.fill")
                        Text("\(n.start ?? "—") → \(n.end ?? "—") · \(n.in_bed_h.map { String(format: "%.1f h", $0) } ?? "—")")
                            .font(Obs.mono(12)).foregroundStyle(Obs.ink2)
                        SleepStages(n: n)
                        NightVitals(n: n)
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

// Pair + sync from a real ring: paste the auth key (exported on the desktop), connect
// over BLE, drain history into the writable DB. BLE only works on a physical device.
struct SyncView: View {
    @ObservedObject var ring: RingSync
    let onSynced: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var key = Keychain.loadKey() ?? ""
    var body: some View {
        NavigationStack {
            ZStack {
                Obs.canvas.ignoresSafeArea()
                VStack(alignment: .leading, spacing: 18) {
                    Text("Pair your ring").font(Obs.prose(20, .semibold)).foregroundStyle(Obs.ink)
                    Text("Wear the ring (off the charger), then paste the auth key you exported on your computer.")
                        .font(Obs.mono(12)).foregroundStyle(Obs.ink2).fixedSize(horizontal: false, vertical: true)
                    TextField("32-hex auth key", text: $key)
                        .font(Obs.mono(13)).foregroundStyle(Obs.ink)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .padding(12)
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Obs.trace, lineWidth: 0.8))
                    Button {
                        Task {
                            await ring.run(keyHex: key)
                            if ring.lastReport != nil { onSynced() }
                        }
                    } label: {
                        HStack(spacing: 8) {
                            if ring.busy { ProgressView().tint(Obs.black) }
                            Text(ring.busy ? "syncing…" : "Connect & Sync").font(Obs.mono(13, .medium))
                        }
                        .frame(maxWidth: .infinity).padding(.vertical, 12)
                        .background(Obs.teal).foregroundStyle(Obs.black)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                    .disabled(ring.busy)
                    if !ring.status.isEmpty {
                        Text(ring.status).font(Obs.mono(12))
                            .foregroundStyle(ring.lastReport != nil ? Obs.teal : Obs.ink2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer()
                }
                .padding(24)
            }
            .navigationTitle("sync").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .toolbarColorScheme(.dark, for: .navigationBar)
        }
        .preferredColorScheme(.dark)
    }
}

// ── root ─────────────────────────────────────────────────────────────────────
struct RootView: View {
    @State private var s: Summary?
    @State private var sheetNight: NightRow?
    @State private var sheetActivityDay: DaySel?
    @State private var showAllDays = false
    @State private var showSync = false
    @StateObject private var ring = RingSync()
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
            Obs.canvas.ignoresSafeArea()
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
        .sheet(item: $sheetActivityDay) { sel in if let s { ActivityDetail(s: s, day: sel.id) } }
        .sheet(isPresented: $showAllDays) { if let s { AllDaysView(s: s) } }
        .sheet(isPresented: $showSync) { SyncView(ring: ring, onSynced: reload) }
        .onAppear(perform: load)
    }

    // re-read the DB after a sync brought in new events
    private func reload() {
        s = nil
        load()
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
                        Button { showSync = true } label: {
                            Image(systemName: "arrow.triangle.2.circlepath")
                                .font(.system(size: 16)).foregroundStyle(Obs.teal)
                        }
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
                        ObsTag("vitals · last night", icon: "waveform.path.ecg")
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
                            ObsTag("cardiovascular", icon: "heart.fill")
                            VStack(spacing: 12) {
                                ObsStat(label: "vascular age", value: String(format: "%.1f yr", va), accent: Obs.teal)
                                if let ca = cv.chronological_age { ObsStat(label: "vs your age", value: relAge(va - ca)) }
                                if let pwv = cv.pwv_ms { ObsStat(label: "pulse-wave velocity", value: String(format: "%.2f m/s", pwv)) }
                                if let seg = cv.segments { ObsStat(label: "segments analysed", value: "\(seg)") }
                            }
                            .obsCard()
                        }

                        // fitness — anthropometric VO₂max estimate (model-free, from demographics)
                        if let vo = s.fitness?.vo2max {
                            ObsTag("fitness", icon: "bolt.heart.fill")
                            ObsStat(label: "vo₂max estimate", value: String(format: "%.1f ml/kg/min", vo), accent: Obs.teal)
                                .obsCard()
                        }

                        // today — last night's sleep + that day's activity as one unit;
                        // tap the sleep or the activity region for its own detail.
                        if let day = s.days.first {
                            ObsTag("today", icon: "sun.max.fill")
                            TodayCard(s: s, day: day,
                                      onSleep: { if let n = s.night(forDay: day) { sheetNight = n } },
                                      onActivity: { sheetActivityDay = DaySel(id: day) })
                        }

                        // browse every day → per-day detail (sleep + activity)
                        if !s.days.isEmpty {
                            Button { showAllDays = true } label: {
                                HStack {
                                    Text("show all \(s.days.count) days").font(Obs.mono(12, .medium)).foregroundStyle(Obs.teal)
                                    Spacer()
                                    Image(systemName: "chevron.right").font(.system(size: 11)).foregroundStyle(Obs.trace)
                                }.contentShape(Rectangle())
                            }.buttonStyle(.plain)
                        }

                        // on-device model failures (empty unless a torch model genuinely
                        // failed — a missing bundle or an inference error, not just no data)
                        if !s.modelErrors.isEmpty {
                            ObsTag("on-device models", icon: "exclamationmark.triangle")
                            VStack(alignment: .leading, spacing: 6) {
                                ForEach(s.modelErrors, id: \.self) { e in
                                    Text("• \(e)").font(Obs.mono(11)).foregroundStyle(Obs.yellow)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                        }

                        // device & data health
                        ObsTag("device & data health", icon: "cpu")
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
                        .obsCard()
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
