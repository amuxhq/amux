import AmuxCore
import AmuxDesign
import XCTest
@testable import AmuxFeatures

/// Code's colours read where code is drawn: on the well behind a code block
/// and an edit card, and under an edit card's green and red, in both
/// appearances. A colour nudged toward its neighbours, or a tint made
/// stronger, fails here rather than on somebody's phone.
final class SyntaxContrastTests: XCTestCase {
    /// The least contrast body text is held to (WCAG AA).
    private static let least = 4.5

    func testEveryTokenColourReadsOnTheWellAndUnderAnEditsTints() {
        let design = Design.app
        let kinds: [TokenKind] = [.keyword, .string, .number, .type, .function, .comment, .plain]
        for appearance in [Appearance.light, .dark] {
            let well = design.sunken.hex(appearance)
            let grounds = [
                ("the well", well),
                ("an added line", Self.blend(design.added.hex(appearance), over: well, DiffPreview.wash)),
                ("a removed line", Self.blend(design.removed.hex(appearance), over: well, DiffPreview.wash)),
            ]
            for kind in kinds {
                let ink = design.ink(kind).hex(appearance)
                for (name, ground) in grounds {
                    let contrast = Self.contrast(ink, ground)
                    XCTAssertGreaterThanOrEqual(
                        contrast, Self.least,
                        "\(kind) in \(appearance) reads at \(contrast):1 on \(name)")
                }
            }
        }
    }

    func testNamesTakeTheInkAndADiffFenceAChangesColours() {
        let design = Design.app
        XCTAssertEqual(design.ink(.plain), design.ink)
        XCTAssertEqual(design.ink(.inserted), design.added)
        XCTAssertEqual(design.ink(.deleted), design.removed)
        XCTAssertNotEqual(design.ink(.keyword), design.accent, "code is never waiting for anyone")
    }

    private static func blend(_ top: UInt32, over ground: UInt32, _ alpha: Double) -> UInt32 {
        [16, 8, 0].reduce(UInt32(0)) { blended, shift in
            let a = Double((top >> shift) & 0xFF)
            let b = Double((ground >> shift) & 0xFF)
            return blended | UInt32((a * alpha + b * (1 - alpha)).rounded()) << shift
        }
    }

    private static func luminance(_ rgb: UInt32) -> Double {
        let channels = [16, 8, 0].map { shift -> Double in
            let value = Double((rgb >> UInt32(shift)) & 0xFF) / 255
            return value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]
    }

    private static func contrast(_ a: UInt32, _ b: UInt32) -> Double {
        let (x, y) = (luminance(a), luminance(b))
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }
}
