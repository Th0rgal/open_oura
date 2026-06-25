import SwiftUI

struct RootView: View {
    @EnvironmentObject var ring: OuraRing

    var body: some View {
        TabView {
            TodayView().tabItem { Label("Today", systemImage: "sun.max.fill") }
            LiveView().tabItem { Label("Live", systemImage: "waveform.path.ecg") }
            SleepView().tabItem { Label("Sleep", systemImage: "bed.double.fill") }
            SettingsView().tabItem { Label("Settings", systemImage: "gearshape.fill") }
        }
        .tint(Brand.hr)
        .sheet(isPresented: $ring.showConnectGuide) {
            ConnectGuideView().presentationDetents([.large])
        }
    }
}

/// Connection state banner reused across tabs. Tap to (re)open the guide.
struct StatusBanner: View {
    @EnvironmentObject var ring: OuraRing
    var body: some View {
        Button { ring.showConnectGuide = true } label: {
            HStack(spacing: 9) {
                Circle().fill(color).frame(width: 8, height: 8)
                Text(ring.status).font(.footnote).foregroundStyle(Brand.dim)
                Spacer()
                if let b = ring.batteryPercent {
                    Image(systemName: ring.charging ? "battery.100.bolt" : batteryIcon(b))
                        .foregroundStyle(ring.charging ? Brand.battery : Brand.dim)
                    Text("\(b)%").font(.footnote).foregroundStyle(Brand.dim)
                } else if ring.state != .ready {
                    Text("Connect").font(.footnote.weight(.semibold)).foregroundStyle(Brand.hr)
                    Image(systemName: "chevron.right").font(.caption2).foregroundStyle(Brand.hr)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .background(Brand.card2)
            .clipShape(Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 18)
    }
    private var color: Color {
        switch ring.state {
        case .ready: return Brand.battery
        case .failed: return Brand.hr
        case .idle: return Brand.dim
        default: return Brand.temp
        }
    }
    private func batteryIcon(_ p: Int) -> String {
        p > 75 ? "battery.100" : p > 40 ? "battery.50" : "battery.25"
    }
}

/// Standard scrollable tab scaffold: dark background, header, status banner.
struct TabScaffold<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content
    var body: some View {
        ZStack {
            Brand.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 14) {
                    AppHeader(title: title)
                    StatusBanner()
                    content
                }
                .padding(.bottom, 30)
            }
        }
    }
}
