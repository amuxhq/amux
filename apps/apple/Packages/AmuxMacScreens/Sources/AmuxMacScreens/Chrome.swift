import AppKit
import SwiftUI

/// The sizes the main window is laid out with, in one place so the columns
/// and their top bars line up.
enum Metrics {
    /// The rail's width: one column of icons.
    static let rail: CGFloat = 58
    static let sidebar: CGFloat = 272
    static let sidebarRange: ClosedRange<CGFloat> = 220...420
    static let inspector: CGFloat = 380
    static let inspectorRange: ClosedRange<CGFloat> = 300...640
    /// Room left at a top bar's leading edge for the window's buttons.
    static let windowButtons: CGFloat = 78
    /// The chat and composer's widest line, as the references read.
    static let reading: CGFloat = 760
    /// The room either side of a page's column.
    static let gutter: CGFloat = 32
    /// A round button's side: send, stop, attach.
    static let control: CGFloat = 28
}

/// The window's colours, one philosophy taken from amux's icon: its
/// blue-black ground, its blue end and its warm end.
///
/// - The ground is cool blue-black (light: cool white) in three steps that
///   lighten toward what the person reads: the rail, then the sidebar,
///   then the content. Surfaces are told apart by those steps and by
///   hairlines, not by grey slabs.
/// - Blue is amux itself: what is selected, the action to take, a link,
///   an agent at work.
/// - Coral is the person: anything that needs them, everywhere, and only
///   that.
/// - Red is failure only; amber a setting that acts without asking. Green
///   and red otherwise appear only in diffs, muted.
/// - A repository's tile takes one of eight tints that none of these
///   states use, so identity never reads as state.
enum Palette {
    static let rail = Color(nsColor: dynamic(light: 0xE8EAF0, dark: 0x101116))
    static let sidebar = Color(nsColor: dynamic(light: 0xF3F4F8, dark: 0x16171D))
    static let content = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x1B1C23))
    /// The floating card, the composer, menus: lifted off the content.
    static let raised = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x23242C))
    /// A code block, a bubble, a field: set into the surface it sits on.
    static let well = Color(nsColor: dynamic(light: 0xF2F3F7, dark: 0x22232B))
    static let line = Color(nsColor: dynamic(light: 0xE2E4EB, dark: 0x2A2B34))
    /// A row under the pointer, and one selected.
    static let hover = Color(nsColor: dynamic(light: 0x0B1230, dark: 0xFFFFFF, alpha: (0.045, 0.045)))
    static let pressed = Color(nsColor: dynamic(light: 0x0B1230, dark: 0xFFFFFF, alpha: (0.09, 0.09)))
    static let selection = Color(nsColor: dynamic(light: 0x2F6BFF, dark: 0x4D8BFF, alpha: (0.11, 0.17)))

    /// amux: selection, the action to take, links, work under way.
    static let accent = Color(nsColor: dynamic(light: 0x2563EB, dark: 0x5B95FF))
    /// The person: whatever needs them.
    static let you = Color(nsColor: dynamic(light: 0xE0582E, dark: 0xFF8A5C))
    static let failure = Color(nsColor: dynamic(light: 0xD23B3B, dark: 0xF26464))
    /// A setting that acts without asking.
    static let caution = Color(nsColor: dynamic(light: 0xB7791F, dark: 0xF0B44C))
    static let added = Color(nsColor: dynamic(light: 0x1F8A4C, dark: 0x5CC98A))
    static let removed = Color(nsColor: dynamic(light: 0xC23B3B, dark: 0xF07878))

    /// The repository tints, by the index every client agrees on: indigo,
    /// violet, magenta, gold, green, teal, sky, lime. No grey: a grey tile
    /// reads as disabled.
    static func tint(_ index: UInt8) -> Color {
        let tints: [(Int, Int)] = [
            (0x4F46E5, 0x8B8CFF), (0x8B3FD9, 0xB98CFF), (0xC0399A, 0xF07BCB), (0xA87410, 0xE8B54A),
            (0x1E8A4F, 0x62CC8E), (0x0F8A8A, 0x4FD0CC), (0x1C7FC4, 0x63B7F2), (0x5E8A12, 0xA9D46B),
        ]
        let (light, dark) = tints[Int(index) % tints.count]
        return Color(nsColor: dynamic(light: light, dark: dark))
    }

    private static func dynamic(light: Int, dark: Int, alpha: (CGFloat, CGFloat) = (1, 1)) -> NSColor {
        NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let hex = isDark ? dark : light
            return NSColor(
                srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                blue: CGFloat(hex & 0xFF) / 255, alpha: isDark ? alpha.1 : alpha.0)
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
                RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                    .fill(
                        configuration.isPressed
                            ? Palette.pressed : (hovering || active ? Palette.hover : .clear)))
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
                Text(title).font(.ui(.body))
                Spacer(minLength: 4)
                if let shortcut {
                    Text(shortcut).font(.ui(.small)).foregroundStyle(.tertiary)
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
            .font(.ui(.meta))
            .foregroundStyle(.secondary)
            .padding(.leading, 8)
            .padding(.top, 14)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Work under way, the size of a glyph in a row: a blue arc turning.
/// Drawn rather than the system's spinner, whose grey dust reads as
/// nothing at this size.
struct Working: View {
    var size: CGFloat = 12
    @State private var turning = false

    var body: some View {
        Circle()
            .trim(from: 0.12, to: 0.82)
            .stroke(Palette.accent, style: StrokeStyle(lineWidth: max(1.5, size / 7), lineCap: .round))
            .frame(width: size, height: size)
            .rotationEffect(.degrees(turning ? 360 : 0))
            .animation(.linear(duration: 0.9).repeatForever(autoreverses: false), value: turning)
            .onAppear { turning = true }
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

/// The window's one button: a pill, the action expected drawn solid in the
/// text colour as the send button is, whether or not the window is in
/// front (the system's prominent style goes grey there); the others quiet
/// fills; a destructive one red words on the quiet fill.
struct PillButtonStyle: ButtonStyle {
    var prominent = false
    var destructive = false
    var small = false
    @Environment(\.isEnabled) private var enabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.ui(small ? .meta : .body, weight: prominent ? .medium : .regular))
            .lineLimit(1)
            .foregroundStyle(
                prominent ? Color(nsColor: .textBackgroundColor) : (destructive ? Palette.failure : Color.primary))
            .padding(.horizontal, small ? 10 : 12)
            .frame(height: small ? 24 : 28)
            .background(
                RoundedRectangle(cornerRadius: small ? Radius.row : Radius.tile, style: .continuous)
                    .fill(prominent ? Color.primary : Color.primary.opacity(hovering ? 0.1 : 0.065)))
            .opacity(configuration.isPressed ? 0.75 : (enabled ? 1 : 0.45))
            .contentShape(RoundedRectangle(cornerRadius: Radius.tile, style: .continuous))
            .onHover { hovering = $0 }
    }
}
