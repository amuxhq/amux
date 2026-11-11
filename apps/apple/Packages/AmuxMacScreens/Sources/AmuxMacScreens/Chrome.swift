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

/// The window's few colours, light and dark. Everything else is the
/// system's.
enum Palette {
    static let sidebar = Color(nsColor: dynamic(light: 0xF4F4F5, dark: 0x1B1B1D))
    static let content = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x232325))
    /// A selected row.
    static let selection = Color.primary.opacity(0.09)
    /// A row under the pointer.
    static let hover = Color.primary.opacity(0.045)
    /// The floating card and the composer's field.
    static let raised = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x2C2C2F))
    static let line = Color(nsColor: .separatorColor)

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
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(active ? .primary : .secondary)
                .frame(width: 28, height: 26)
                .contentShape(Rectangle())
        }
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
