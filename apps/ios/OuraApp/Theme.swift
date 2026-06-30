import SwiftUI

// "Observatory" design tokens (see apps/ios/GOAL.md). Black canvas, sparse ink,
// one accent per screen, monospace numerics. No panels — data floats on the void.
enum Obs {
    static let canvas = Color(red: 0.039, green: 0.039, blue: 0.043) // #0A0A0B
    static let black = Color.black

    static let ink = Color(white: 0.93)        // primary
    static let ink2 = Color(white: 0.54)       // secondary
    static let trace = Color(white: 0.27)       // structural traces / dashes

    static let teal = Color(red: 0.31, green: 0.82, blue: 0.77)   // in-range / good
    static let yellow = Color(red: 0.90, green: 0.75, blue: 0.30) // the one threshold

    // Type — SF Mono for every number/axis/tag; SF Pro for prose.
    static func mono(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
    static func prose(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight)
    }
}

// A small all-caps mono section tag, the Flywheel "GRAPH LIST" voice.
struct ObsTag: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text.uppercased())
            .font(Obs.mono(11, .medium))
            .tracking(2)
            .foregroundStyle(Obs.ink2)
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
