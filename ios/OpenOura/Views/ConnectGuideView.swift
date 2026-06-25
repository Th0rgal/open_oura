import SwiftUI

/// Onboarding / connection guide. Walks the user through placing the ring on its
/// charging pad next to the phone (it sleeps when off-charger and not worn, so the
/// pad is what makes it reliably connectable), then drives connect/auth with live
/// progress and a clear success/failure state.
struct ConnectGuideView: View {
    @EnvironmentObject var ring: OuraRing
    @Environment(\.dismiss) private var dismiss

    private var paired: Bool { KeyStore.keyBytes() != nil }
    private var inProgress: Bool {
        [ConnState.scanning, .connecting, .authenticating].contains(ring.state)
    }
    private var failed: Bool { if case .failed = ring.state { return true } else { return false } }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 28) {
                    illustration
                        .frame(height: 130)
                        .padding(.top, 28)

                    Text(ring.state == .ready ? "Ring connected" : "Connect your ring")
                        .font(.title2.bold())

                    if ring.state == .ready {
                        connectedBody
                    } else {
                        steps
                    }
                }
                .padding(.horizontal, 24)
            }

            footer
                .padding(20)
                .background(.ultraThinMaterial)
        }
        .presentationDragIndicator(.visible)
        .onChange(of: ring.state) { newValue in
            // Auto-dismiss shortly after a successful connection.
            if newValue == .ready {
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.1) {
                    if ring.state == .ready { dismiss() }
                }
            }
        }
    }

    // MARK: pieces

    private var illustration: some View {
        HStack(spacing: 18) {
            Image(systemName: "iphone")
                .font(.system(size: 60, weight: .light))
                .foregroundStyle(.white)
            Image(systemName: "dot.radiowaves.left.and.right")
                .font(.title)
                .foregroundStyle(inProgress ? .pink : Color(white: 0.4))
            ZStack {
                Circle().fill(Color(white: 0.16)).frame(width: 96, height: 96)        // pad
                Circle().fill(Color(white: 0.10)).frame(width: 70, height: 70)        // pad inset
                Circle().stroke(ring.state == .ready ? .green : .pink, lineWidth: 9)  // the ring
                    .frame(width: 44, height: 44)
            }
        }
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: 18) {
            step(1, "Place the ring on its charging pad", "Off the charger and not worn, the ring sleeps to save battery. The pad wakes it so it's reliably connectable.")
            step(2, "Put the pad next to your iPhone", "Within arm's reach (about 30 cm) for a strong, stable Bluetooth link.")
            step(3, paired ? "Tap Connect" : "Tap Pair ring", paired ? "We'll authenticate with your saved key automatically." : "We'll generate a key and install it on the ring (it must be factory-reset).")
        }
    }

    private func step(_ n: Int, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Text("\(n)")
                .font(.subheadline.bold()).foregroundColor(.black)
                .frame(width: 26, height: 26)
                .background(Circle().fill(.pink))
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.subheadline.weight(.semibold))
                Text(detail).font(.footnote).foregroundColor(.secondary)
            }
            Spacer(minLength: 0)
        }
    }

    private var connectedBody: some View {
        VStack(spacing: 10) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 44)).foregroundStyle(.green)
            if let s = ring.serial { Text(s).font(.footnote.monospaced()).foregroundColor(.secondary) }
            if let b = ring.batteryPercent { Text("Battery \(b)%").font(.footnote).foregroundColor(.secondary) }
        }
    }

    private var footer: some View {
        VStack(spacing: 10) {
            // Live status line.
            HStack(spacing: 8) {
                if inProgress { ProgressView() }
                Text(ring.status)
                    .font(.footnote)
                    .foregroundColor(failed ? .red : .secondary)
            }
            .frame(maxWidth: .infinity)

            if ring.state == .ready {
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent).tint(.green)
                    .frame(maxWidth: .infinity)
            } else {
                Button {
                    Task { paired ? await ring.authenticateAndLoad() : await ring.pairNewRing() }
                } label: {
                    Text(buttonLabel).frame(maxWidth: .infinity).padding(.vertical, 4)
                }
                .buttonStyle(.borderedProminent).tint(.pink)
                .disabled(inProgress)

                if !paired {
                    Text("Already paired this ring with the CLI? Import its key in Settings instead.")
                        .font(.caption2).foregroundColor(.secondary).multilineTextAlignment(.center)
                }
            }
        }
    }

    private var buttonLabel: String {
        if inProgress { return paired ? "Connecting…" : "Pairing…" }
        if failed { return "Try again" }
        return paired ? "Connect" : "Pair ring"
    }
}
