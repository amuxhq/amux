import AmuxValues
import AppKit
import SwiftUI
import XCTest
@testable import AmuxMacScreens

/// The Mac's code colours read where code is drawn: on the well behind a
/// code block and an edit card's patch, and under the patch's green and red,
/// in light and dark. Held to the iPhone's rule (`CodeContrast`). The
/// colours are resolved from the palette itself, as the window draws them,
/// so a hex nudged in `Chrome.swift` fails here.
final class SyntaxContrast: XCTestCase {
    @MainActor
    func testEveryTokenColourReadsOnTheWellAndUnderAnEditsTints() throws {
        let kinds: [TokenKind] = [.keyword, .string, .number, .type, .function, .comment]
        for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
            let appearance = try XCTUnwrap(NSAppearance(named: appearance))
            func colour(_ color: NSColor) -> CodeContrast.Colour {
                var resolved = CodeContrast.Colour(red: 0, green: 0, blue: 0)
                appearance.performAsCurrentDrawingAppearance {
                    let srgb = color.usingColorSpace(.sRGB) ?? color
                    resolved = CodeContrast.Colour(
                        red: srgb.redComponent, green: srgb.greenComponent, blue: srgb.blueComponent,
                        alpha: srgb.alphaComponent)
                }
                return resolved
            }
            let well = colour(NSColor(Palette.well))
            // A kind given no colour takes the text's own, the label colour.
            let inks = kinds.map { kind in ("\(kind)", colour(NSColor(Palette.syntax(kind) ?? .primary))) }
                + [("plain", colour(.labelColor))]
            CodeContrast.assertEveryInkReads(
                inks,
                on: [
                    ("the well", well),
                    ("an added line", colour(NSColor(Palette.added)).opacity(PatchText.wash).over(well)),
                    ("a removed line", colour(NSColor(Palette.removed)).opacity(PatchText.wash).over(well)),
                ],
                in: name)
        }
    }

    /// Names take the text's own ink, and a diff fence a change's colours.
    func testNamesTakeTheInkAndADiffFenceAChangesColours() {
        XCTAssertNil(Palette.syntax(.plain))
        XCTAssertEqual(Palette.syntax(.inserted), Palette.added)
        XCTAssertEqual(Palette.syntax(.deleted), Palette.removed)
    }
}
