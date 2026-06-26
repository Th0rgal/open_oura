import SwiftUI

@main
struct OpenOuraApp: App {
    @StateObject private var ring = OuraRing()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(ring)
                .preferredColorScheme(.dark)
                .task {
                    ring.loadCachedHistory()   // show last-synced data immediately
                    // Silently reconnect to a known ring on launch; otherwise guide.
                    if ring.canAutoReconnect { ring.autoReconnect() }
                    else { ring.showConnectGuide = true }
                }
        }
    }
}
