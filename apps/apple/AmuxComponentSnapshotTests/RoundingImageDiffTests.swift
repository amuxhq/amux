import SnapshotTesting
import UIKit
import XCTest

final class RoundingImageDiffTests: XCTestCase {
    private let diffing = RoundingImageDiff.allowingRasterNoise(.image(scale: 1), named: "test")

    func testIdenticalImagesMatch() throws {
        let reference = try image([100, 110, 120, 255])
        XCTAssertNil(diffing.diffV2(reference, reference))
    }

    func testOneLevelRoundingInEveryColorChannelMatches() throws {
        let reference = try image([100, 110, 120, 255, 100, 110, 120, 255])
        let actual = try image([101, 109, 121, 255, 99, 111, 119, 255])
        XCTAssertNotNil(Diffing<UIImage>.image(scale: 1).diffV2(reference, actual))
        XCTAssertNil(diffing.diffV2(reference, actual))
    }

    func testRoundingEverywhereSpendsNoneOfTheAllowance() throws {
        let reference = try image(flat(pixels: 200))
        let actual = try image(flat(pixels: 200, moving: 200, by: UInt8(RoundingImageDiff.rounding)))
        XCTAssertEqual(RoundingImageDiff.measure(reference, actual), .init(pixels: 0, largest: 0))
        XCTAssertNil(diffing.diffV2(reference, actual))
    }

    func testAFewPixelsALittlePastRoundingMatchAndAreCounted() throws {
        let reference = try image(flat(pixels: 200))
        let actual = try image(flat(
            pixels: 200, moving: RoundingImageDiff.strayPixels,
            by: UInt8(RoundingImageDiff.strayCeiling)))
        XCTAssertEqual(
            RoundingImageDiff.measure(reference, actual),
            .init(pixels: RoundingImageDiff.strayPixels, largest: RoundingImageDiff.strayCeiling))
        XCTAssertNil(diffing.diffV2(reference, actual))
    }

    func testOnePixelMoreThanTheAllowanceFails() throws {
        let reference = try image(flat(pixels: 200))
        let actual = try image(flat(
            pixels: 200, moving: RoundingImageDiff.strayPixels + 1, by: UInt8(RoundingImageDiff.rounding) + 1))
        XCTAssertNotNil(diffing.diffV2(reference, actual))
    }

    func testOneChannelPastTheCeilingFailsAndRetainsAttachments() throws {
        let reference = try image([100, 110, 120, 255, 100, 110, 120, 255])
        for channel in 0..<3 {
            var pixels: [UInt8] = [100, 110, 120, 255, 100, 110, 120, 255]
            pixels[channel] += UInt8(RoundingImageDiff.strayCeiling) + 1
            let failure = try XCTUnwrap(diffing.diffV2(reference, try image(pixels)))
            XCTAssertFalse(failure.1.isEmpty)
        }
    }

    func testAlphaPastTheCeilingFails() throws {
        XCTAssertNotNil(diffing.diffV2(
            try image([0, 0, 0, 255]), try image([0, 0, 0, 240])))
    }

    func testDifferentDimensionsFail() throws {
        XCTAssertNotNil(diffing.diffV2(
            try image([100, 110, 120, 255]),
            try image([100, 110, 120, 255, 100, 110, 120, 255])))
    }

    /// One row of one colour, its first `moving` pixels moved by `by` in the
    /// red channel.
    private func flat(pixels: Int, moving: Int = 0, by: UInt8 = 0) -> [UInt8] {
        (0..<pixels).flatMap { [100 + ($0 < moving ? by : 0), 110, 120, 255] }
    }

    private func image(_ rgba: [UInt8]) throws -> UIImage {
        let provider = try XCTUnwrap(CGDataProvider(data: Data(rgba) as CFData))
        let colorSpace = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        let cgImage = try XCTUnwrap(CGImage(
            width: rgba.count / 4, height: 1, bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: rgba.count, space: colorSpace,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent))
        return UIImage(cgImage: cgImage, scale: 1, orientation: .up)
    }
}
