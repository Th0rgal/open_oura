import SwiftUI

/// The Open Oura mark — a gapped gradient ring — drawn as a scalable vector so it
/// matches the app icon. Used in headers and the connect guide.
struct RingLogo: View {
    var lineWidth: CGFloat = 0.16   // as a fraction of the frame
    var animated = false
    @State private var spin = false

    private let grad = AngularGradient(
        colors: [Color(hex: 0xf38ba8), Color(hex: 0xcba6f7), Color(hex: 0x94e2d5), Color(hex: 0xf38ba8)],
        center: .center)

    var body: some View {
        GeometryReader { geo in
            let s = min(geo.size.width, geo.size.height)
            ZStack {
                Circle()
                    .trim(from: 0.06, to: 0.94)        // the "open" gap
                    .stroke(grad, style: StrokeStyle(lineWidth: s * lineWidth, lineCap: .round))
                    .rotationEffect(.degrees(-90 + 22))
                    .rotationEffect(.degrees(animated && spin ? 360 : 0))
                    .animation(animated ? .linear(duration: 6).repeatForever(autoreverses: false) : nil, value: spin)
            }
            .frame(width: s, height: s)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .onAppear { if animated { spin = true } }
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(.sRGB,
                  red: Double((hex >> 16) & 0xff) / 255,
                  green: Double((hex >> 8) & 0xff) / 255,
                  blue: Double(hex & 0xff) / 255,
                  opacity: 1)
    }
}

/// Brand palette (Catppuccin-ish, matching the dashboard + icon).
enum Brand {
    static let bg = Color(hex: 0x0b0d12)
    static let card = Color(hex: 0x12151d)
    static let card2 = Color(hex: 0x171b25)
    static let line = Color(hex: 0x262b38)
    static let text = Color(hex: 0xcdd6f4)
    static let dim = Color(hex: 0x8b91a5)
    static let hr = Color(hex: 0xf38ba8)
    static let hrv = Color(hex: 0xcba6f7)
    static let motion = Color(hex: 0x89b4fa)
    static let spo2 = Color(hex: 0x94e2d5)
    static let temp = Color(hex: 0xfab387)
    static let battery = Color(hex: 0xa6e3a1)
    static let sleepDeep = Color(hex: 0x6c7086)
    static let sleepLight = Color(hex: 0x89b4fa)
    static let sleepRem = Color(hex: 0xcba6f7)
    static let sleepAwake = Color(hex: 0xf38ba8)
}
