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
        .tint(.pink)
        .sheet(isPresented: $ring.showConnectGuide) {
            ConnectGuideView()
                .presentationDetents([.large])
        }
    }
}

/// Small connection state banner reused across tabs. Tap to (re)open the guide.
struct StatusBanner: View {
    @EnvironmentObject var ring: OuraRing
    var body: some View {
        Button { ring.showConnectGuide = true } label: {
            HStack(spacing: 8) {
                Circle().fill(color).frame(width: 9, height: 9)
                Text(ring.status).font(.footnote).foregroundColor(.secondary)
                Spacer()
                if let b = ring.batteryPercent {
                    Image(systemName: ring.charging ? "battery.100.bolt" : "battery.50")
                    Text("\(b)%").font(.footnote)
                } else if ring.state != .ready {
                    Text("Connect").font(.footnote.weight(.semibold)).foregroundColor(.pink)
                }
            }
            .padding(.horizontal)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
    var color: Color {
        switch ring.state {
        case .ready: return .green
        case .failed: return .red
        case .idle: return .gray
        default: return .yellow
        }
    }
}
