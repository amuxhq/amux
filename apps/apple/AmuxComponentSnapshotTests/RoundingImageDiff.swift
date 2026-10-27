import SnapshotTesting
import UIKit

enum RoundingImageDiff {
    /// How far a channel of any pixel may move: native rasterizers can round
    /// a border channel differently across hosts.
    static let rounding = 1
    /// How many pixels may move further than rounding, and how far a channel
    /// of such a pixel may move. Now and then the rasterizer draws the
    /// anti-aliased edge of a rounded border a few levels differently: once
    /// on CI, eight pixels on a card's four corners moved by up to four
    /// levels in a picture that had stopped changing. Noise is small in both
    /// ways at once and a change somebody made is not, so both numbers are
    /// held: a mark gone missing moves its pixels far, and a colour that
    /// drifted moves thousands of them. Never a percentage of arbitrary
    /// mismatching pixels, which grows with the picture. The whole-screen
    /// comparison holds the same numbers in `ios-tools`'s `golden::ALLOWANCE`.
    static let strayPixels = 64
    static let strayCeiling = 8

    /// What separates two pictures of one size: how many pixels moved
    /// further than rounding, and the furthest any channel of one moved.
    struct Strays: Equatable {
        var pixels = 0
        var largest = 0

        var allowed: Bool {
            pixels <= RoundingImageDiff.strayPixels && largest <= RoundingImageDiff.strayCeiling
        }
    }

    /// `exact`, forgiving rounding everywhere and the stray pixels the
    /// allowance covers. A match that spent some of the allowance prints how
    /// much under `name`, so the allowance can be judged from the runs.
    static func allowingRasterNoise(
        _ exact: Diffing<UIImage>, named name: String
    ) -> Diffing<UIImage> {
        var result = exact
        result.diffV2 = { reference, actual in
            guard let failure = exact.diffV2(reference, actual) else { return nil }
            guard let strays = measure(reference, actual), strays.allowed else { return failure }
            if strays.pixels > 0 {
                print(
                    "AMUX_SNAPSHOT_STRAYS component=\(name) pixels=\(strays.pixels) "
                        + "largest=\(strays.largest)")
            }
            return nil
        }
        return result
    }

    /// Nothing when the two cannot be compared pixel by pixel.
    static func measure(_ reference: UIImage, _ actual: UIImage) -> Strays? {
        guard let expected = reference.cgImage, let taken = actual.cgImage,
            expected.width > 0, expected.height > 0,
            expected.width == taken.width, expected.height == taken.height,
            let expectedBytes = normalizedBytes(expected),
            let takenBytes = normalizedBytes(taken)
        else { return nil }

        var found = Strays()
        var index = 0
        while index < expectedBytes.count {
            var moved = 0
            for channel in index..<index + 4 {
                moved = max(moved, abs(Int(expectedBytes[channel]) - Int(takenBytes[channel])))
            }
            if moved > rounding {
                found.pixels += 1
                found.largest = max(found.largest, moved)
            }
            index += 4
        }
        return found
    }

    private static func normalizedBytes(_ image: CGImage) -> [UInt8]? {
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
                let context = CGContext(
                    data: buffer.baseAddress,
                    width: image.width, height: image.height,
                    bitsPerComponent: 8, bytesPerRow: image.width * 4,
                    space: colorSpace,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        return drawn ? bytes : nil
    }
}
