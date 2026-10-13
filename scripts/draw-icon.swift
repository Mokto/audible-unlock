import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let resolution = 2048
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let context = CGContext(data: nil, width: resolution, height: resolution, bitsPerComponent: 8,
                        bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
context.scaleBy(x: 2, y: 2)
context.translateBy(x: 0, y: 1024)
context.scaleBy(x: 1, y: -1)
context.setAllowsAntialiasing(true)

func color(_ hex: UInt32, alpha: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: space, components: [CGFloat((hex >> 16) & 255) / 255,
                                          CGFloat((hex >> 8) & 255) / 255,
                                          CGFloat(hex & 255) / 255, alpha])!
}

func gradient(_ path: CGPath, colors: [CGColor], from: CGPoint, to: CGPoint) {
    context.saveGState()
    context.addPath(path)
    context.clip()
    context.drawLinearGradient(CGGradient(colorsSpace: space, colors: colors as CFArray, locations: nil)!,
                               start: from, end: to, options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
    context.restoreGState()
}

let tile = CGPath(roundedRect: CGRect(x: 96, y: 76, width: 832, height: 832),
                  cornerWidth: 186, cornerHeight: 186, transform: nil)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -18), blur: 30, color: color(0x000917, alpha: 0.3))
context.addPath(tile)
context.setFillColor(color(0x142239))
context.fillPath()
context.restoreGState()
gradient(tile, colors: [color(0x294969), color(0x15263e), color(0x0d1524)],
         from: CGPoint(x: 190, y: 76), to: CGPoint(x: 790, y: 908))

context.saveGState()
context.addPath(tile)
context.clip()
let glow = CGGradient(colorsSpace: space,
                      colors: [color(0x61a7bd, alpha: 0.18), color(0x61a7bd, alpha: 0)] as CFArray,
                      locations: [0, 1])!
context.drawRadialGradient(glow, startCenter: CGPoint(x: 350, y: 225), startRadius: 0,
                           endCenter: CGPoint(x: 350, y: 225), endRadius: 530, options: [])
context.restoreGState()
context.addPath(tile)
context.setStrokeColor(color(0xa6d1e3, alpha: 0.15))
context.setLineWidth(2)
context.strokePath()

let shelf = CGPath(roundedRect: CGRect(x: 232, y: 688, width: 560, height: 34),
                   cornerWidth: 17, cornerHeight: 17, transform: nil)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -12), blur: 22, color: color(0x030915, alpha: 0.45))
context.addPath(shelf)
context.setFillColor(color(0xe7a44b))
context.fillPath()
context.restoreGState()
gradient(shelf, colors: [color(0xffe2a0), color(0xf6bd66), color(0xed8e45)],
         from: CGPoint(x: 232, y: 688), to: CGPoint(x: 792, y: 722))

let spines: [(height: CGFloat, top: UInt32, bottom: UInt32)] = [
    (150, 0xf7f3ea, 0xd9e1e7), (214, 0xdbe7f0, 0xb7c9d6), (276, 0xf7f3ea, 0xd9e1e7),
    (320, 0xf2d9a8, 0xdcb575), (276, 0xf7f3ea, 0xd9e1e7), (214, 0xdbe7f0, 0xb7c9d6), (150, 0xf7f3ea, 0xd9e1e7),
]
for (index, spine) in spines.enumerated() {
    let x = 300 + CGFloat(index) * 62
    let rect = CGRect(x: x, y: 682 - spine.height, width: 52, height: spine.height)
    let path = CGPath(roundedRect: rect, cornerWidth: 10, cornerHeight: 10, transform: nil)
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -8), blur: 14, color: color(0x030915, alpha: 0.35))
    context.addPath(path)
    context.setFillColor(color(spine.top))
    context.fillPath()
    context.restoreGState()
    gradient(path, colors: [color(spine.top), color(spine.bottom)],
             from: CGPoint(x: x, y: rect.minY), to: CGPoint(x: x, y: rect.maxY))
    for y in [rect.minY + 26, rect.maxY - 34] {
        context.addPath(CGPath(roundedRect: CGRect(x: x + 10, y: y, width: 32, height: 8),
                               cornerWidth: 4, cornerHeight: 4, transform: nil))
        context.setFillColor(color(0x365673, alpha: 0.45))
        context.fillPath()
    }
}

let band = CGMutablePath()
band.move(to: CGPoint(x: 238, y: 520))
band.addCurve(to: CGPoint(x: 786, y: 520), control1: CGPoint(x: 238, y: 170), control2: CGPoint(x: 786, y: 170))
let bandOutline = band.copy(strokingWithWidth: 44, lineCap: .round, lineJoin: .round, miterLimit: 10)
var cups = [CGPath]()
for x in [198.0, 746.0] {
    cups.append(CGPath(roundedRect: CGRect(x: x, y: 470, width: 80, height: 150),
                       cornerWidth: 34, cornerHeight: 34, transform: nil))
}
for path in [bandOutline] + cups {
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -8), blur: 14, color: color(0x030a16, alpha: 0.35))
    context.addPath(path)
    context.setFillColor(color(0xe7a44b))
    context.fillPath()
    context.restoreGState()
    gradient(path, colors: [color(0xffe2a0), color(0xf6bd66), color(0xed8e45)],
             from: CGPoint(x: 238, y: 250), to: CGPoint(x: 786, y: 620))
}

let image = context.makeImage()!
let output = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = CGImageDestinationCreateWithURL(output as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(destination, image, nil)
precondition(CGImageDestinationFinalize(destination))
print("Rendered \(resolution)×\(resolution) icon master")
