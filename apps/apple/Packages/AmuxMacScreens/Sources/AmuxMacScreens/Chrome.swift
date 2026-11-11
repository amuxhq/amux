import AppKit
import SwiftUI

/// The sizes the main window is laid out with, in one place so the columns
/// and their top bars line up.
enum Metrics {
    /// The rail's width: one column of icons.
    static let rail: CGFloat = 52
    static let sidebar: CGFloat = 272
    static let sidebarRange: ClosedRange<CGFloat> = 220...420
    static let inspector: CGFloat = 380
    static let inspectorRange: ClosedRange<CGFloat> = 300...640
    /// Room left at a top bar's leading edge for the window's buttons.
    static let windowButtons: CGFloat = 78
    /// The chat and composer's widest line, as the references read.
    static let reading: CGFloat = 760
}

/// The window's few colours, light and dark, calibrated against the
/// references: in dark the sidebar sits a shade above the content (as in
/// Claude's), split from it by a hairline; in light it is a touch grey.
/// Everything else is the system's.
enum Palette {
    static let sidebar = Color(nsColor: dynamic(light: 0xF7F7F6, dark: 0x262628))
    static let content = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x1E1E1F))
    /// A selected row.
    static let selection = Color.primary.opacity(0.085)
    /// A row under the pointer.
    static let hover = Color.primary.opacity(0.045)
    /// The floating card, the composer and other surfaces lifted off the
    /// content.
    static let raised = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x29292B))
    /// A code block, a bubble: set apart from prose without a border.
    static let well = Color.primary.opacity(0.05)
    static let line = Color.primary.opacity(0.09)

    private static func dynamic(light: Int, dark: Int) -> NSColor {
        NSColor(name: nil) { appearance in
            let hex = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
            return NSColor(
                srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
        }
    }
}

extension View {
    /// Draws the window's content up into a title bar with no title or
    /// background of its own, the way the references draw theirs: the
    /// window's buttons sit on the sidebar, and each column's top bar is
    /// the title bar's height.
    func windowChrome() -> some View {
        background(WindowChrome())
    }
}

private struct WindowChrome: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView { ChromeView() }
    func updateNSView(_ view: NSView, context: Context) {}

    final class ChromeView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
            window.titlebarSeparatorStyle = .none
            window.styleMask.insert(.fullSizeContentView)
            // An empty compact toolbar makes the title bar the height of the
            // references' top bars and centres the window's buttons in it.
            if window.toolbar == nil {
                let toolbar = NSToolbar(identifier: "sh.amux.main")
                toolbar.showsBaselineSeparator = false
                window.toolbar = toolbar
            }
            window.toolbarStyle = .unifiedCompact
        }
    }
}

/// A column's top bar: the title bar's height, its content centred in it,
/// and dragging the window from anywhere a control is not.
struct TopBar<Content: View>: View {
    let height: CGFloat
    var leading: CGFloat = 0
    @ViewBuilder let content: Content

    var body: some View {
        HStack(spacing: 6) { content }
            .padding(.leading, 12 + leading)
            .padding(.trailing, 10)
            .frame(height: height)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .gesture(WindowDragGesture())
    }
}

/// A borderless icon button in a top bar or the rail.
struct BarButton: View {
    let symbol: String
    let help: String
    var active = false
    var size: CGFloat = 13.5
    var disabled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Icon(symbol, size: size + 2)
                .foregroundStyle(active ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                .opacity(disabled ? 0.4 : 1)
                .frame(width: 28, height: 26)
                .contentShape(Rectangle())
        }
        .disabled(disabled)
        .buttonStyle(HoverButtonStyle(active: active))
        .help(help)
        .accessibilityLabel(help)
    }
}

/// Lights up under the pointer and while pressed, as toolbar buttons do.
struct HoverButtonStyle: ButtonStyle {
    var active = false
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(
                        configuration.isPressed
                            ? Palette.selection : (hovering || active ? Palette.hover : .clear)))
            .onHover { hovering = $0 }
    }
}

/// A draggable edge between two columns, changing `width` by the drag.
struct ColumnEdge: View {
    @Binding var width: CGFloat
    let range: ClosedRange<CGFloat>
    /// The column grows when the edge moves left: it is on the right.
    var trailing = false
    @State private var start: CGFloat?

    var body: some View {
        Rectangle()
            .fill(Palette.line)
            .frame(width: 1)
            .overlay {
                Color.clear
                    .frame(width: 8)
                    .contentShape(Rectangle())
                    .pointerStyle(.columnResize)
                    .gesture(
                        DragGesture(minimumDistance: 1, coordinateSpace: .global)
                            .onChanged { drag in
                                let from = start ?? width
                                if start == nil { start = width }
                                let moved = trailing ? -drag.translation.width : drag.translation.width
                                width = min(max(from + moved, range.lowerBound), range.upperBound)
                            }
                            .onEnded { _ in start = nil })
            }
    }
}

/// A plain row in a sidebar: an icon and words, lit under the pointer,
/// as the references' "New chat" and "Search" are.
struct SidebarAction: View {
    let symbol: String
    let title: String
    var shortcut: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Icon(symbol, size: 15)
                    .foregroundStyle(.secondary)
                    .frame(width: 18)
                Text(title).font(.system(size: 13))
                Spacer(minLength: 4)
                if let shortcut {
                    Text(shortcut).font(.system(size: 11.5)).foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 28)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
    }
}

/// A heading over a group of rows in a sidebar.
struct SectionTitle: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(.leading, 8)
            .padding(.top, 14)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A small spinner for something at work, the size of a glyph in a row:
/// the system's own, which the window server animates.
struct Working: View {
    var body: some View {
        ProgressView()
            .controlSize(.mini)
            .scaleEffect(0.8)
            .frame(width: 14, height: 14)
            .accessibilityLabel("Working")
    }
}

/// One of the app's icons: Lucide's line drawings, which the system draws
/// from their SVG and tints like text. Named as Lucide names them.
struct Icon: View {
    let name: String
    var size: CGFloat = 14

    init(_ name: String, size: CGFloat = 14) {
        self.name = name
        self.size = size
    }

    var body: some View {
        Image(nsImage: Icons.image(name))
            .renderingMode(.template)
            .resizable()
            .interpolation(.high)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// The icons, read once each from the package's resources.
@MainActor
enum Icons {
    private static var loaded: [String: NSImage] = [:]

    static func image(_ name: String) -> NSImage {
        if let image = loaded[name] { return image }
        let url = Bundle.module.url(forResource: name, withExtension: "svg", subdirectory: "Icons")
        let image = url.flatMap { NSImage(contentsOf: $0) } ?? NSImage()
        image.isTemplate = true
        loaded[name] = image
        return image
    }
}
