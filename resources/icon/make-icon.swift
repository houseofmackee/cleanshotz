// Draws the CleanShotZ app icon and builds resources/AppIcon.icns:
//   swift resources/icon/make-icon.swift
// A rounded square in macOS proportions (824 of 1024 points, with a soft
// shadow), an indigo to teal gradient, white viewfinder corners and a "Z".
import AppKit

func render(_ size: Int) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(size) / 1024
    let ctx = NSGraphicsContext.current!.cgContext
    ctx.scaleBy(x: s, y: s)

    let body = NSRect(x: 100, y: 100, width: 824, height: 824)
    let shape = NSBezierPath(roundedRect: body, xRadius: 185, yRadius: 185)
    // Shadow under the body.
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.3)
    shadow.shadowBlurRadius = 28
    shadow.shadowOffset = NSSize(width: 0, height: -12)
    shadow.set()
    NSColor.black.setFill()
    shape.fill()
    NSGraphicsContext.restoreGraphicsState()

    // Gradient body.
    let gradient = NSGradient(colors: [
        NSColor(srgbRed: 0.33, green: 0.30, blue: 0.95, alpha: 1),
        NSColor(srgbRed: 0.13, green: 0.72, blue: 0.80, alpha: 1),
    ])!
    gradient.draw(in: shape, angle: -45)
    // A soft highlight on the top half.
    NSGraphicsContext.saveGraphicsState()
    shape.addClip()
    NSGradient(colors: [NSColor.white.withAlphaComponent(0.18), NSColor.white.withAlphaComponent(0)])!
        .draw(in: NSRect(x: 100, y: 512, width: 824, height: 412), angle: -90)
    NSGraphicsContext.restoreGraphicsState()

    // Viewfinder corners.
    NSColor.white.setStroke()
    let inset: CGFloat = 230, arm: CGFloat = 120
    let (lo, hi) = (inset, 1024 - inset)
    for (x, y, dx, dy) in [(lo, hi, 1.0, -1.0), (hi, hi, -1.0, -1.0), (lo, lo, 1.0, 1.0), (hi, lo, -1.0, 1.0)] {
        let path = NSBezierPath()
        path.move(to: NSPoint(x: x, y: y + dy * arm))
        path.line(to: NSPoint(x: x, y: y))
        path.line(to: NSPoint(x: x + dx * arm, y: y))
        path.lineWidth = 46
        path.lineCapStyle = .round
        path.lineJoinStyle = .round
        path.stroke()
    }

    // The Z.
    let z = NSBezierPath()
    z.move(to: NSPoint(x: 392, y: 642))
    z.line(to: NSPoint(x: 632, y: 642))
    z.line(to: NSPoint(x: 392, y: 382))
    z.line(to: NSPoint(x: 632, y: 382))
    z.lineWidth = 58
    z.lineCapStyle = .round
    z.lineJoinStyle = .round
    NSGraphicsContext.saveGraphicsState()
    let zShadow = NSShadow()
    zShadow.shadowColor = NSColor.black.withAlphaComponent(0.18)
    zShadow.shadowBlurRadius = 10
    zShadow.shadowOffset = NSSize(width: 0, height: -6)
    zShadow.set()
    z.stroke()
    NSGraphicsContext.restoreGraphicsState()

    NSGraphicsContext.restoreGraphicsState()
    return rep
}

let root = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        let data = render(points * scale).representation(using: .png, properties: [:])!
        try! data.write(to: iconset.appendingPathComponent(name))
    }
}
try! render(1024).representation(using: .png, properties: [:])!
    .write(to: root.appendingPathComponent("resources/icon/AppIcon-1024.png"))
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", root.appendingPathComponent("resources/AppIcon.icns").path]
try! task.run()
task.waitUntilExit()
print(task.terminationStatus == 0 ? "wrote resources/AppIcon.icns" : "iconutil failed")
