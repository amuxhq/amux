import AmuxMacScreens
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
        case .smallest: MainWindowSize.least
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
    private static let sheetSize = CGSize(width: 540, height: 640)

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
        styleMask =
            switch story.window {
            case .main: Self.mainStyle
            case .settings: [.titled, .closable, .fullSizeContentView]
            // A sheet has no title bar of its own; it hangs from its window's.
            case .sheet: [.borderless]
            }
        title = story.window == .settings ? "Settings" : "amux"
        let host = NSHostingController(rootView: story.view())
        host.sceneBridgingOptions = [.toolbars, .title]
        // The main window's least size comes from its content, as the app's
        // does (it sizes its window by `.contentMinSize`), so the smallest
        // story is drawn at the size the app can really reach.
        host.sizingOptions = story.window == .main ? [.minSize] : []
        contentViewController = host
        host.view.layoutSubtreeIfNeeded()
        let content: CGSize =
            switch story.window {
            case .main: size.points
            case .settings: Self.settingsSize
            case .sheet: Self.sheetSize
            }
        setContentSize(
            CGSize(width: max(content.width, contentMinSize.width), height: max(content.height, contentMinSize.height)))
        setFrameTopLeftPoint(NSPoint(x: left, y: top))
    }
}
