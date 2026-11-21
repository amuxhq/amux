import Foundation
import XCTest

/// The rule both apps hold code's colours to: every token's colour reads at
/// 4.5:1 or more (WCAG AA for body text) on every ground a token is drawn
/// on, in both appearances. Each app's test names its own inks and grounds;
/// the rule and its arithmetic are here once, so the iPhone and the Mac
/// cannot drift into checking different things.
///
/// The Mac's tests compile this same file (apps/apple/Mac/project.yml), as
/// the two apps share no test package.
enum CodeContrast {
    /// The least contrast body text is held to (WCAG AA).
    static let least = 4.5

    /// An sRGB colour, each channel 0 to 1, with how opaque it is.
    struct Colour: CustomStringConvertible {
        var red: Double
        var green: Double
        var blue: Double
        var alpha: Double = 1

        init(red: Double, green: Double, blue: Double, alpha: Double = 1) {
            (self.red, self.green, self.blue, self.alpha) = (red, green, blue, alpha)
        }

        init(hex: UInt32) {
            self.init(
                red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
                blue: Double(hex & 0xFF) / 255)
        }

        /// This colour laid over `ground`, as it is drawn there.
        func over(_ ground: Colour) -> Colour {
            func mix(_ top: Double, _ under: Double) -> Double { top * alpha + under * (1 - alpha) }
            return Colour(
                red: mix(red, ground.red), green: mix(green, ground.green), blue: mix(blue, ground.blue))
        }

        /// The same colour at `alpha`, as a tint is laid under a line.
        func opacity(_ alpha: Double) -> Colour {
            Colour(red: red, green: green, blue: blue, alpha: self.alpha * alpha)
        }

        var description: String {
            String(format: "#%02X%02X%02X", Int((red * 255).rounded()), Int((green * 255).rounded()),
                   Int((blue * 255).rounded())) + (alpha < 1 ? String(format: "@%.2f", alpha) : "")
        }

        fileprivate var luminance: Double {
            func linear(_ value: Double) -> Double {
                value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
            }
            return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
        }
    }

    /// The contrast of `ink` drawn on `ground`, both laid over whatever is
    /// under them first.
    static func ratio(_ ink: Colour, on ground: Colour) -> Double {
        let (a, b) = (ink.over(ground).luminance, ground.luminance)
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    /// Fails once for every ink that reads below ``least`` on a ground.
    static func assertEveryInkReads(
        _ inks: [(name: String, colour: Colour)], on grounds: [(name: String, colour: Colour)],
        in appearance: String, file: StaticString = #filePath, line: UInt = #line
    ) {
        for ink in inks {
            for ground in grounds {
                let contrast = ratio(ink.colour, on: ground.colour)
                XCTAssertGreaterThanOrEqual(
                    contrast, least,
                    "\(ink.name) \(ink.colour) in \(appearance) reads at "
                        + String(format: "%.2f", contrast) + ":1 on \(ground.name) \(ground.colour)",
                    file: file, line: line)
            }
        }
    }
}
