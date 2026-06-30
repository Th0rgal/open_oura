import SwiftUI

// Calls into the shared Rust analysis core (oura-analysis via oura-ffi),
// the SAME code the web dashboard computes HRV with — now running on iOS.
func rustRMSSD(_ ibis: [UInt16]) -> Double {
    ibis.withUnsafeBufferPointer { buf in
        oura_rmssd(buf.baseAddress, buf.count)
    }
}

// Independent Swift reference, to show the on-device Rust result is correct.
func swiftRMSSD(_ ibis: [UInt16]) -> Double {
    guard ibis.count >= 2 else { return -1 }
    var sumSq = 0.0
    for i in 1..<ibis.count {
        let d = Double(ibis[i]) - Double(ibis[i - 1])
        sumSq += d * d
    }
    return (sumSq / Double(ibis.count - 1)).squareRoot()
}

struct ContentView: View {
    let ibis: [UInt16] = [800, 820, 810, 830]
    var body: some View {
        let rust = rustRMSSD(ibis)
        let swift = swiftRMSSD(ibis)
        let match = abs(rust - swift) < 1e-9
        return VStack(spacing: 16) {
            Text("open_oura · iOS spike")
                .font(.headline)
            Text("Shared Rust core on-device")
                .font(.subheadline).foregroundStyle(.secondary)
            Divider().padding(.horizontal, 40)
            Text("IBI: \(ibis.map(String.init).joined(separator: ", ")) ms")
                .font(.caption).foregroundStyle(.secondary)
            Text(String(format: "RMSSD (Rust): %.4f ms", rust))
                .font(.title3).monospaced()
            Text(String(format: "RMSSD (Swift ref): %.4f ms", swift))
                .font(.callout).monospaced().foregroundStyle(.secondary)
            Text(match ? "✅ PARITY OK" : "❌ MISMATCH")
                .font(.title2).bold()
                .foregroundStyle(match ? .green : .red)
            #if TORCH
            Divider().padding(.horizontal, 40)
            torchSection
            #endif
        }
        .padding()
    }
}

#if TORCH
extension ContentView {
    var torchSection: some View {
        let path = Bundle.main.path(forResource: "model", ofType: "ptl") ?? ""
        let sum = oura_torch_smoke(path)               // runs the .ptl via LibTorch lite
        let ok = abs(sum - 303.0902) < 1e-2            // matches macOS C++ + Python
        return VStack(spacing: 6) {
            Text("LibTorch lite on-device (.ptl)")
                .font(.subheadline).foregroundStyle(.secondary)
            Text(String(format: "steps_motion_decoder out[1].sum = %.4f", sum))
                .font(.callout).monospaced()
            Text(ok ? "✅ TORCH PARITY OK" : "❌ TORCH MISMATCH")
                .font(.title3).bold()
                .foregroundStyle(ok ? .green : .red)
        }
    }
}
#endif

@main
struct OuraSpikeApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}
