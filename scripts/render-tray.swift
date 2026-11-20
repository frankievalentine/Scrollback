// Renders crates/app/icon/tray.svg into raw RGBA pixels (36 by 36, the menu
// bar size at 2x), which the app embeds directly so it needs no image decoder.
//
//   swift scripts/render-tray.swift crates/app/icon/tray.svg crates/app/icon/tray.rgba
import AppKit

let arguments = CommandLine.arguments
guard arguments.count == 3, let image = NSImage(contentsOfFile: arguments[1]) else {
    FileHandle.standardError.write("usage: render-tray.swift <tray.svg> <tray.rgba>\n".data(using: .utf8)!)
    exit(1)
}
let side = 36
// Drawn premultiplied, which is all a graphics context accepts. The icon is
// pure black, so its colour channels are zero either way and only the alpha
// channel carries the shape.
let bitmap = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
bitmap.size = NSSize(width: side, height: side)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
NSGraphicsContext.current?.imageInterpolation = .high
image.draw(in: NSRect(x: 0, y: 0, width: side, height: side))
NSGraphicsContext.restoreGraphicsState()
var pixels = Data(capacity: side * side * 4)
let source = bitmap.bitmapData!
for row in 0..<side {
    pixels.append(source + row * bitmap.bytesPerRow, count: side * 4)
}
try pixels.write(to: URL(fileURLWithPath: arguments[2]))
