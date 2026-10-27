// Exits 0 when the given image is blank (a single color), 1 when it has content, 2 when unreadable.
import CoreGraphics
import Foundation
import ImageIO

let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
    let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
else { exit(2) }

let width = image.width
let height = image.height
var pixels = [UInt8](repeating: 0, count: width * height * 4)
guard
    let context = CGContext(
        data: &pixels, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { exit(2) }
context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))

let first = Array(pixels[0..<4])
var offset = 0
while offset < pixels.count {
    if pixels[offset] != first[0] || pixels[offset + 1] != first[1] || pixels[offset + 2] != first[2] {
        exit(1)
    }
    offset += 4 * 7
}
exit(0)
