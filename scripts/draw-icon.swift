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

let shackle = CGMutablePath()
shackle.move(to: CGPoint(x: 360, y: 444))
shackle.addLine(to: CGPoint(x: 360, y: 347))
shackle.addCurve(to: CGPoint(x: 658, y: 318), control1: CGPoint(x: 360, y: 166), control2: CGPoint(x: 658, y: 166))
let shackleOutline = shackle.copy(strokingWithWidth: 58, lineCap: .round, lineJoin: .round, miterLimit: 10)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -8), blur: 14, color: color(0x030a16, alpha: 0.35))
context.addPath(shackleOutline)
context.setFillColor(color(0xe7a44b))
context.fillPath()
context.restoreGState()
gradient(shackleOutline, colors: [color(0xffe2a0), color(0xf6bd66), color(0xed8e45)],
         from: CGPoint(x: 360, y: 220), to: CGPoint(x: 658, y: 449))

let book = CGMutablePath()
book.move(to: CGPoint(x: 272, y: 451))
book.addCurve(to: CGPoint(x: 512, y: 487), control1: CGPoint(x: 356, y: 411), control2: CGPoint(x: 442, y: 443))
book.addCurve(to: CGPoint(x: 752, y: 451), control1: CGPoint(x: 582, y: 443), control2: CGPoint(x: 668, y: 411))
book.addLine(to: CGPoint(x: 752, y: 702))
book.addCurve(to: CGPoint(x: 512, y: 751), control1: CGPoint(x: 668, y: 665), control2: CGPoint(x: 577, y: 703))
book.addCurve(to: CGPoint(x: 272, y: 702), control1: CGPoint(x: 447, y: 703), control2: CGPoint(x: 356, y: 665))
book.closeSubpath()
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -16), blur: 25, color: color(0x030915, alpha: 0.4))
context.addPath(book)
context.setFillColor(color(0xe8edf2))
context.fillPath()
context.restoreGState()
gradient(book, colors: [color(0xffffff), color(0xf5f1e8), color(0xcbd7df)],
         from: CGPoint(x: 440, y: 444), to: CGPoint(x: 540, y: 751))

let binding = CGMutablePath()
binding.move(to: CGPoint(x: 512, y: 493))
binding.addLine(to: CGPoint(x: 512, y: 730))
context.addPath(binding)
context.setLineWidth(10)
context.setLineCap(.round)
context.setStrokeColor(color(0x597486, alpha: 0.25))
context.strokePath()

for (x, height) in [(336.0, 42.0), (370.0, 82.0), (404.0, 118.0), (438.0, 66.0)] {
    let bar = CGPath(roundedRect: CGRect(x: x, y: 582 - height / 2, width: 18, height: height),
                     cornerWidth: 9, cornerHeight: 9, transform: nil)
    gradient(bar, colors: [color(0x365673), color(0x1d344c)],
             from: CGPoint(x: x, y: 523), to: CGPoint(x: x, y: 641))
}

for (y, width, alpha) in [(548.0, 134.0, 0.9), (588.0, 116.0, 0.7), (628.0, 87.0, 0.5)] {
    let line = CGPath(roundedRect: CGRect(x: 567, y: y, width: width, height: 14),
                      cornerWidth: 7, cornerHeight: 7, transform: nil)
    context.addPath(line)
    context.setFillColor(color(0x3c5870, alpha: alpha))
    context.fillPath()
}

let image = context.makeImage()!
let output = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = CGImageDestinationCreateWithURL(output as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(destination, image, nil)
precondition(CGImageDestinationFinalize(destination))
print("Rendered \(resolution)×\(resolution) icon master")
