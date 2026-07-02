import SwiftUI

// "Observatory" design tokens (see apps/ios/GOAL.md), shared design language with the
// web dashboard: a calm near-black canvas with a faint teal aurora, translucent glass
// surfaces, one accent, monospace numerics. Science-refined minimalism.
enum Obs {
    static let black = Color.black
    // deep, slightly-cool base of the gradient canvas (matches web --bg #0b0c0f)
    static let base = Color(red: 0.043, green: 0.047, blue: 0.059)
    static let baseLow = Color(red: 0.024, green: 0.027, blue: 0.035)

    static let ink = Color(white: 0.93)        // primary
    static let ink2 = Color(white: 0.54)       // secondary
    static let trace = Color(white: 0.27)       // structural traces / dashes

    static let teal = Color(red: 0.176, green: 0.831, blue: 0.749) // accent, == web #2dd4bf
    static let yellow = Color(red: 0.90, green: 0.75, blue: 0.30) // the one threshold

    /// The shared calm canvas: a soft vertical gradient with a faint teal aurora bloom
    /// top-center — the iOS echo of the web dashboard's ambient glow. Use behind screens
    /// via `Obs.canvas.ignoresSafeArea()` instead of a flat fill.
    static var canvas: some View {
        LinearGradient(colors: [base, baseLow], startPoint: .top, endPoint: .bottom)
            .overlay(alignment: .top) {
                RadialGradient(
                    colors: [teal.opacity(0.10), .clear],
                    center: .top, startRadius: 0, endRadius: 460
                )
                .blendMode(.screen)
            }
    }

    // sleep-stage hues, matching the web dashboard's dark theme (deep darkest → awake
    // lightest). Used by the hypnogram + stage breakdown.
    static let deep = Color(red: 0.365, green: 0.412, blue: 0.537)
    static let light = Color(red: 0.549, green: 0.592, blue: 0.702)
    static let rem = Color(red: 0.435, green: 0.631, blue: 0.659)
    static let wake = Color(red: 0.757, green: 0.690, blue: 0.561)
    /// stage code 1=deep 2=light 3=rem 4=wake (matches run_sleep_model.py)
    static func stage(_ s: Int) -> Color {
        switch s { case 1: return deep; case 2: return light; case 3: return rem; default: return wake }
    }

    // Type — SF Mono for every number/axis/tag; SF Pro for prose.
    static func mono(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
    static func prose(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight)
    }
}

// A small all-caps mono section tag with an optional leading SF Symbol — the section
// header voice, mirroring the web dashboard's icon + label panel heads.
struct ObsTag: View {
    let text: String
    var icon: String? = nil
    init(_ text: String, icon: String? = nil) { self.text = text; self.icon = icon }
    var body: some View {
        HStack(spacing: 7) {
            if let icon {
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Obs.ink2.opacity(0.85))
            }
            Text(text.uppercased())
                .font(Obs.mono(11, .medium))
                .tracking(2)
                .foregroundStyle(Obs.ink2)
        }
    }
}

// ── glass surface ─────────────────────────────────────────────────────────────
// A translucent "liquid glass" card: an ultra-thin material with a 1px light rim and a
// soft top sheen (a web-glassmorphism-style approximation of the web dashboard panels;
// not Apple's system Liquid Glass API). Degrades to the material fill under reduced
// transparency automatically.
struct ObsCard: ViewModifier {
    var padding: CGFloat = 18
    var radius: CGFloat = 18
    func body(content: Content) -> some View {
        content
            .padding(padding)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        LinearGradient(
                            colors: [.white.opacity(0.16), .white.opacity(0.03)],
                            startPoint: .top, endPoint: .bottom
                        ),
                        lineWidth: 0.8
                    )
            )
            .shadow(color: .black.opacity(0.28), radius: 18, x: 0, y: 10)
    }
}

extension View {
    func obsCard(padding: CGFloat = 18, radius: CGFloat = 18) -> some View {
        modifier(ObsCard(padding: padding, radius: radius))
    }
}

// One labelled datum, value in mono. The atom of the readout panels.
struct ObsStat: View {
    let label: String
    let value: String
    var accent: Color = Obs.ink
    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label).font(Obs.mono(13)).foregroundStyle(Obs.ink2)
            Spacer(minLength: 16)
            Text(value).font(Obs.mono(15, .medium)).foregroundStyle(accent)
                .monospacedDigit()
        }
    }
}
