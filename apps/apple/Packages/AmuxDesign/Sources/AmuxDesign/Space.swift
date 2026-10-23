import CoreGraphics

/// The steps every gap in the app is taken from.
///
/// A gap picked by eye for each screen is how two screens end up a point or
/// two apart where they should agree, so spacing comes from this scale and
/// nothing between its steps. It counts in fours, as the system's own
/// layout does, and the screen's margin is the system's, so what the app
/// draws lines up with the navigation bar's title above it.
public enum Space {
    public static let xxs: CGFloat = 2
    public static let xs: CGFloat = 4
    public static let s: CGFloat = 8
    public static let m: CGFloat = 12
    public static let l: CGFloat = 16
    public static let xl: CGFloat = 24
    public static let xxl: CGFloat = 32

    /// The screen's leading and trailing margin: the system's, so a list
    /// starts where the large title does.
    public static let margin: CGFloat = l
}
