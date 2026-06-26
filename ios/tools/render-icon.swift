// Renders the Open Oura app icon (gapped gradient ring + heartbeat tick) to a PNG.
// Run: swift ios/tools/render-icon.swift <out.png>   (macOS / CoreGraphics)
import AppKit
import CoreGraphics

let size = 1024.0
let cs = CGColorSpaceCreateDeviceRGB()
let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8,
                    bytesPerRow: 0, space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

// Background: radial dark gradient.
let bg = CGGradient(colorsSpace: cs,
    colors: [CGColor(red: 0.106, green: 0.122, blue: 0.169, alpha: 1),
             CGColor(red: 0.043, green: 0.051, blue: 0.071, alpha: 1)] as CFArray,
    locations: [0, 1])!
ctx.drawRadialGradient(bg, startCenter: CGPoint(x: size/2, y: size*0.58), startRadius: 0,
                       endCenter: CGPoint(x: size/2, y: size*0.58), endRadius: size*0.85,
                       options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])

// Gapped ring (gap at top). CG is y-up: top = +90°. Sweep the long way leaving a gap.
let center = CGPoint(x: size/2, y: size/2)
let radius = 300.0, lineWidth = 118.0
let gap = 46.0 * .pi / 180.0
let arc = CGMutablePath()
arc.addArc(center: center, radius: radius,
           startAngle: .pi/2 + gap/2, endAngle: .pi/2 - gap/2 + 2 * .pi, clockwise: false)
let outline = arc.copy(strokingWithWidth: lineWidth, lineCap: .round, lineJoin: .round, miterLimit: 10)
ctx.saveGState()
ctx.addPath(outline); ctx.clip()
let ring = CGGradient(colorsSpace: cs,
    colors: [CGColor(red: 0.953, green: 0.545, blue: 0.659, alpha: 1),
             CGColor(red: 0.796, green: 0.651, blue: 0.969, alpha: 1),
             CGColor(red: 0.580, green: 0.886, blue: 0.835, alpha: 1)] as CFArray,
    locations: [0, 0.5, 1])!
ctx.drawLinearGradient(ring, start: CGPoint(x: size*0.18, y: size*0.85),
                       end: CGPoint(x: size*0.82, y: size*0.15), options: [])
ctx.restoreGState()

// Heartbeat tick across the lower ring (drawn in bg colour to "cut" through).
ctx.setStrokeColor(CGColor(red: 0.043, green: 0.051, blue: 0.071, alpha: 1))
ctx.setLineWidth(26); ctx.setLineCap(.round); ctx.setLineJoin(.round)
let pts = [(392.0, 424.0), (458, 424), (484, 476), (518, 372), (544, 476), (610, 476), (632, 424)]
ctx.beginPath()
ctx.move(to: CGPoint(x: pts[0].0, y: pts[0].1))
for p in pts.dropFirst() { ctx.addLine(to: CGPoint(x: p.0, y: p.1)) }
ctx.strokePath()

let img = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: img)
let png = rep.representation(using: .png, properties: [:])!
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon-1024.png"
try! png.write(to: URL(fileURLWithPath: out))
print("wrote \(out)")
