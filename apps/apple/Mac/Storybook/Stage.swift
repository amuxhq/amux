import AmuxMacStories
import AppKit
import SwiftUI

/// The window sizes a story is drawn at: the app's first size, its smallest
/// and a large one. A story of the Settings window has its one size.
enum StorySize: String, CaseIterable, Codable {
    case regular
    case smallest
    case large

    var points: CGSize {
        switch self {
        case .regular: CGSize(width: 900, height: 640)
        case .smallest: CGSize(width: 680, height: 560)
        case .large: CGSize(width: 1280, height: 860)
        }
    }
}

/// A story's window, with the style the app's own has, so its title bar,
/// toolbar and glass are the ones the app gets.
@MainActor
final class StageWindow: NSWindow {
    /// Past the edge of every display: drawn, and seen by nobody.
    var offDisplay = false

    private static let settingsSize = CGSize(width: 540, height: 760)

    init() {
        super.init(
            contentRect: CGRect(origin: .zero, size: StorySize.regular.points), styleMask: Self.mainStyle,
            backing: .buffered, defer: false)
        isReleasedWhenClosed = false
    }

    private static let mainStyle: NSWindow.StyleMask = [
        .titled, .closable, .miniaturizable, .resizable, .fullSizeContentView,
    ]

    // AppKit otherwise pulls a titled window back onto a display.
    override func constrainFrameRect(_ frame: NSRect, to screen: NSScreen?) -> NSRect {
        offDisplay ? frame : super.constrainFrameRect(frame, to: screen)
    }

    /// Replaces what the window shows, its top-left corner staying put.
    func show(_ story: Story, size: StorySize) {
        let top = frame.maxY
        let left = frame.minX
        styleMask = story.window == .settings ? [.titled, .closable, .fullSizeContentView] : Self.mainStyle
        title = story.window == .settings ? "Settings" : "Amux"
        let host = NSHostingController(rootView: story.view())
        host.sceneBridgingOptions = [.toolbars, .title]
        host.sizingOptions = []
        contentViewController = host
        setContentSize(story.window == .settings ? Self.settingsSize : size.points)
        setFrameTopLeftPoint(NSPoint(x: left, y: top))
    }
}
