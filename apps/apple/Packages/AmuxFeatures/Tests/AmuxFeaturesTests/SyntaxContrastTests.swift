import AmuxCore
import AmuxDesign
import XCTest
@testable import AmuxFeatures

/// Code's colours read where code is drawn: on the well behind a code block
/// and an edit card, and under an edit card's green and red, in both
/// appearances. A colour nudged toward its neighbours, or a tint made
/// stronger, fails here rather than on somebody's phone. The Mac's palette
/// is held to the same rule (`CodeContrast`) in its own tests.
final class SyntaxContrastTests: XCTestCase {
    func testEveryTokenColourReadsOnTheWellAndUnderAnEditsTints() {
        let design = Design.app
        let kinds: [TokenKind] = [.keyword, .string, .number, .type, .function, .comment, .plain]
        for appearance in [Appearance.light, .dark] {
            func colour(_ ramp: Ramp) -> CodeContrast.Colour { CodeContrast.Colour(hex: ramp.hex(appearance)) }
            let well = colour(design.sunken)
            CodeContrast.assertEveryInkReads(
                kinds.map { ("\($0)", colour(design.ink($0))) },
                on: [
                    ("the well", well),
                    ("an added line", colour(design.added).opacity(DiffPreview.wash).over(well)),
                    ("a removed line", colour(design.removed).opacity(DiffPreview.wash).over(well)),
                ],
                in: appearance.rawValue)
        }
    }

    func testNamesTakeTheInkAndADiffFenceAChangesColours() {
        let design = Design.app
        XCTAssertEqual(design.ink(.plain), design.ink)
        XCTAssertEqual(design.ink(.inserted), design.added)
        XCTAssertEqual(design.ink(.deleted), design.removed)
        XCTAssertNotEqual(design.ink(.keyword), design.accent, "code is never waiting for anyone")
    }
}
