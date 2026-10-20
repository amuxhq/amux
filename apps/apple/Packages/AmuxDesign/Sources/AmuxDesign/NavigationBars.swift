#if canImport(UIKit)
import UIKit

extension Design {
    /// Sets the system navigation bar's titles in the display face.
    ///
    /// The bar is the system's, for its behaviour: the large title that
    /// shrinks as the list scrolls and the glass buttons beside it. Only its
    /// type is the app's, so a screen's name reads in the same voice as
    /// everything under it. The bundled face is variable, so the weight is
    /// asked for on its weight axis; a weight trait does not reach it.
    @MainActor
    public func dressNavigationBars() {
        BundledFonts.register()
        let bar = UINavigationBar.appearance()
        bar.largeTitleTextAttributes = [.font: display(size: 34, weight: 650, style: .largeTitle)]
        bar.titleTextAttributes = [.font: display(size: 17, weight: 600, style: .headline)]
    }

    private func display(size: CGFloat, weight: CGFloat, style: UIFont.TextStyle) -> UIFont {
        let wght = 0x7767_6874  // 'wght'
        let descriptor = UIFontDescriptor(fontAttributes: [
            .family: faces.display,
            UIFontDescriptor.AttributeName(rawValue: kCTFontVariationAttribute as String): [wght: weight],
        ])
        let font = UIFont(descriptor: descriptor, size: size)
        return UIFontMetrics(forTextStyle: style).scaledFont(for: font)
    }
}
#endif
