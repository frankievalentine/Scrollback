// Renders crates/app/icon/AppIcon.svg into the PNG sizes macOS wants and
// leaves them in an .iconset folder for `iconutil`.
//
//   swift scripts/render-icon.swift <AppIcon.svg> <out.iconset>
import AppKit

let arguments = CommandLine.arguments
guard arguments.count == 3, let image = NSImage(contentsOfFile: arguments[1]) else {
    FileHandle.standardError.write("usage: render-icon.swift <AppIcon.svg> <out.iconset>\n".data(using: .utf8)!)
    exit(1)
}
let folder = URL(fileURLWithPath: arguments[2])
try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)

// Each point size at 1x and 2x.
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = points * scale
        let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        bitmap.size = NSSize(width: pixels, height: pixels)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        NSGraphicsContext.current?.imageInterpolation = .high
        image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels))
        NSGraphicsContext.restoreGraphicsState()
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        try bitmap.representation(using: .png, properties: [:])!.write(to: folder.appendingPathComponent(name))
    }
}
