import AppKit
import SwiftUI

/// The sizes the main window is laid out with, in one place so the frame,
/// the windows on it and their bars line up.
///
/// The window is a frame the system's buttons sit on, with the app's
/// content on it as rounded windows: the workspace (agents and chat) and,
/// beside it, the tools. Everything on the title row is centred on the
/// system buttons' centre line, and nothing moves them.
enum Metrics {
    /// The frame's left column, from the window's edge to the panel: an
    /// 8-point margin each side of a 36-point button.
    static let rail: CGFloat = 52
    /// A rail button's side.
    static let railButton: CGFloat = 36
    /// The frame showing at the panel's right and bottom edges.
    static let gap: CGFloat = 6
    /// Where the title row's own buttons start: past the window's buttons,
    /// which end 72 points in.
    static let windowButtons: CGFloat = 80
    /// A bar across the top of a column inside the panel: the sidebar's
    /// search, a tool's own toolbar.
    static let bar: CGFloat = 44
    static let sidebar: CGFloat = 256
    static let sidebarRange: ClosedRange<CGFloat> = 220...420
    static let inspector: CGFloat = 520
    static let inspectorRange: ClosedRange<CGFloat> = 340...900
    /// The chat and composer's widest line: about 90 characters of body text,
    /// the most that still reads comfortably.
    static let reading: CGFloat = 736
    /// The room either side of a page's column.
    static let gutter: CGFloat = 32
    /// A small button's side: attach, a chip's height.
    static let control: CGFloat = 28
    /// The send and stop button's side: a chip's height, as desktop controls are
    /// sized for a pointer rather than a finger.
    static let send: CGFloat = 28
}

/// The window's colours: true black in dark (white in light), the way the
/// developer tools this app sits beside are drawn. No hue in the ground:
/// colour is kept for meaning.
///
/// - The frame (title row, rail, gutters) is a step off black; the panel on
///   it is black, its sidebar a step lighter than the chat and tools.
/// - What is lifted off the panel (the composer, cards, menus) is a dark
///   grey; what is set into it (code, a command's output) barely lighter
///   than black. The person's prompt sits on a lighter grey.
/// - Text has four tiers of our own, set at the window's root as the
///   primary, secondary and tertiary styles.
/// - Blue is amux at work and the send button. Amber is the person: an
///   approval or anything else that needs them. Indigo is a question. Red
///   is failure; orange a setting that acts without asking; green a turn
///   done and not yet seen. Green and red otherwise appear only in diffs.
/// - A repository's monogram takes one of eight tints that none of these
///   states use, so identity never reads as state.
enum Palette {
    /// The frame: the title row, the rail and the gutters.
    static let frame = Color(nsColor: dynamic(light: 0xF0F0F1, dark: 0x0E0E0E))
    /// The panel on the frame: the chat and the tools.
    static let content = Color(nsColor: dynamic(light: 0xFCFCFC, dark: 0x000000))
    /// The sidebar's ground, inside the panel.
    static let sidebar = Color(nsColor: dynamic(light: 0xF8F8F9, dark: 0x060606))
    /// The panel's edge against the frame, and a raised surface's.
    static let edge = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.08, 0.07)))
    /// The composer and menus: lifted off the panel.
    static let raised = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x0E0E0E))
    /// A card over the panel: an ask, a floating summary.
    static let card = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0x131313))
    /// A fill on the panel: the prompt, a chip, a field.
    static let well = Color(nsColor: dynamic(light: 0xEBEBED, dark: 0x1E1E1E))
    /// Set into the panel: code, a command and its output, changed files.
    static let inset = Color(nsColor: dynamic(light: 0xF5F5F6, dark: 0x0A0A0A))
    /// The pill behind inline code in prose.
    static let codePill = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.06, 0.1)))
    /// A hairline inside the panel.
    static let line = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.08, 0.08)))
    /// A row or button under the pointer, pressed, and a row selected:
    /// neutral, so blue keeps meaning work.
    static let hover = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.045, 0.07)))
    static let pressed = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.08, 0.1)))
    /// A selected row: lifted in dark, a white card in light, which takes
    /// `selectionRing` round it.
    static let selection = Color(nsColor: dynamic(light: 0xFFFFFF, dark: 0xFFFFFF, alpha: (1, 0.1)))
    static let selectionRing = Color(nsColor: dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: (0.08, 0)))

    /// Text, from what is read to what is barely there.
    static let text = Color(nsColor: dynamic(light: 0x18181B, dark: 0xF5F5F5))
    static let textSecondary = Color(nsColor: dynamic(light: 0x71717B, dark: 0xA1A1A1))
    static let textTertiary = Color(nsColor: dynamic(light: 0xA1A1AA, dark: 0x737373))
    static let textQuaternary = Color(nsColor: dynamic(light: 0xC4C4CA, dark: 0x4A4A4A))
    /// The agent's prose: a touch under the text, so the person's own
    /// words and the headings stand out of it.
    /// Claude's own terracotta, for its mark only.
    static let claude = Color(nsColor: dynamic(light: 0xD97757, dark: 0xD97757))
    static let prose = Color(nsColor: dynamic(light: 0x18181B, dark: 0xF5F5F5, alpha: (0.9, 0.9)))

    // One hue carries attention: blue is the person's turn (send, the
    // expected answer, an agent that needs them, the rail's count). Work
    // in progress is not news and stays neutral; red is for failure only,
    // orange for a permission that never asks. The tones are softened
    // from full saturation so they sit on the near-black ground without
    // glowing.
    /// The send button, the expected answer, focus, links.
    static let accent = Color(nsColor: dynamic(light: 0x2563EB, dark: 0x3E7BEE))
    /// An agent at work: neutral, since work going on is not news.
    static let working = textSecondary
    /// The person: whatever needs them, as words and marks.
    static let you = Color(nsColor: dynamic(light: 0x2563EB, dark: 0x6EA8FE))
    /// A question waiting on the person: the person's colour, told apart
    /// by its glyph.
    static let question = you
    /// A turn finished and not yet looked at.
    static let done = Color(nsColor: dynamic(light: 0x15803D, dark: 0x4ADE80))
    static let failure = Color(nsColor: dynamic(light: 0xDC2626, dark: 0xF26B6B))
    /// A setting that acts without asking.
    static let caution = Color(nsColor: dynamic(light: 0xC2410C, dark: 0xF08A4B))
    static let added = Color(nsColor: dynamic(light: 0x1A7F37, dark: 0x07C480))
    static let removed = Color(nsColor: dynamic(light: 0xCF222E, dark: 0xFB414A))

    /// The repository tints, by the index every client agrees on: indigo,
    /// violet, magenta, sand, jade, teal, steel, lime. None is a state's
    /// colour, and none is grey: a grey swatch reads as disabled.
    static func tint(_ index: UInt8) -> Color {
        let tints: [(Int, Int)] = [
            (0x4F46E5, 0x8E8CFF), (0x7C3AED, 0xB98CFF), (0xBE185D, 0xF07BCB), (0x8C6A2F, 0xD9B77E),
            (0x0F7A5C, 0x4FC7A0), (0x0E7C8A, 0x46C6D6), (0x4A5A7A, 0x94A9D6), (0x5E8A12, 0xA9D46B),
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
    /// background of its own, as Mac apps with a sidebar draw theirs: the
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
            // columns' top bars and centres the window's buttons in it.
            if window.toolbar == nil {
                let toolbar = NSToolbar(identifier: "sh.amux.main")
                toolbar.showsBaselineSeparator = false
                window.toolbar = toolbar
            }
            window.toolbarStyle = .unifiedCompact
        }
    }
}

/// A window's bar across its top: one height in every window, its content
/// centred in it, a hairline under it. The window is not dragged from it:
/// the frame around the windows is the window's to drag.
struct WindowBar<Content: View>: View {
    var leading: CGFloat = 12
    var rule = true
    @ViewBuilder let content: Content

    var body: some View {
        HStack(spacing: 6) { content }
            .padding(.leading, leading)
            .padding(.trailing, 8)
            .frame(height: Metrics.bar)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) {
                if rule { Rectangle().fill(Palette.line).frame(height: 1) }
            }
    }
}

extension View {
    /// Sets what it holds on the frame as one of the window's rounded
    /// windows: its own ground, clipped to the window's corner, with an
    /// edge where it meets the frame.
    func onFrame() -> some View {
        background(Palette.content)
            .clipShape(RoundedRectangle(cornerRadius: Radius.window, style: .continuous))
            .overlay(
                // Straddling the edge, so it reads on the frame's side too.
                RoundedRectangle(cornerRadius: Radius.window, style: .continuous)
                    .stroke(Palette.edge, lineWidth: 1))
    }

    /// The window's text tiers as the primary, secondary and tertiary
    /// styles, so `.secondary` and `.tertiary` below read at our contrast.
    func textTiers() -> some View {
        foregroundStyle(Palette.text, Palette.textSecondary, Palette.textTertiary)
    }
}

/// A borderless icon button in a bar: 28 points wide, its glyph drawn at
/// `size`. One that is on shows it in the glyph's colour, not with a fill
/// that would read as the pointer resting on it.
struct BarButton: View {
    let symbol: String
    let help: String
    var active = false
    var size: CGFloat = 16
    var disabled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Icon(symbol, size: size, optical: false)
                .foregroundStyle(active ? AnyShapeStyle(.primary) : AnyShapeStyle(Palette.textSecondary))
                .opacity(disabled ? 0.4 : 1)
                .frame(width: 28, height: 26)
                .contentShape(Rectangle())
        }
        .disabled(disabled)
        .buttonStyle(HoverButtonStyle())
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

/// A draggable edge between two columns, changing `width` by the drag:
/// a hairline inside a window, or the frame's gap between two windows.
struct ColumnEdge: View {
    @Binding var width: CGFloat
    let range: ClosedRange<CGFloat>
    /// The column grows when the edge moves left: it is on the right.
    var trailing = false
    /// The frame's gap between two windows rather than a hairline.
    var gap = false
    @State private var start: CGFloat?

    var body: some View {
        Rectangle()
            .fill(gap ? Color.clear : Palette.line)
            .frame(width: gap ? Metrics.gap : 1)
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
/// as a Mac sidebar's commands are. Its icon centres on the
/// rows' status column and its words start where their names do.
struct SidebarAction: View {
    let symbol: String
    let title: String
    var shortcut: String?
    var count: Int?
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Icon(symbol, size: 16)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 14)
                Text(title).font(.ui(.body))
                Spacer(minLength: 4)
                if let count {
                    Text("\(count)").font(.ui(.caption)).foregroundStyle(Palette.textTertiary).monospacedDigit()
                }
                if let shortcut, hovering {
                    Text(shortcut).font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
                }
            }
            .padding(.leading, 6)
            .padding(.trailing, 8)
            .frame(height: 30)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
        .onHover { hovering = $0 }
    }
}

/// A heading over a group of rows in a sidebar: small and quiet, so it
/// reads as a label for the rows rather than as one of them, starting on
/// the rows' status column.
struct SectionTitle: View {
    let text: String
    var first = false

    var body: some View {
        Text(text)
            .font(.ui(.caption, weight: .semibold))
            .foregroundStyle(Palette.textTertiary)
            .padding(.leading, 6)
            .padding(.top, first ? 8 : 16)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Work under way: a blue dot that breathes, in a slot the size of a
/// glyph. Every one on screen breathes from one clock, so a list of working
/// rows pulses together rather than out of step, and the frames are
/// stepped (15 a second) rather than redrawn at the display's rate. Under
/// Reduce Motion it holds still.
struct Working: View {
    var size: CGFloat = 14
    @Environment(\.accessibilityReduceMotion) private var still

    var body: some View {
        Group {
            if still {
                dot(1)
            } else {
                TimelineView(.periodic(from: .distantPast, by: 1.0 / 15)) { context in
                    dot(Self.level(at: context.date))
                }
            }
        }
        .frame(width: size, height: size)
        .accessibilityLabel("Working")
    }

    private func dot(_ level: Double) -> some View {
        Circle()
            .fill(Palette.working)
            .frame(width: 6, height: 6)
            .opacity(0.35 + 0.65 * level)
            .scaleEffect(0.85 + 0.15 * level)
    }

    /// Where the breath is, from 0 to 1, on a 1.4-second cycle every dot
    /// shares.
    static func level(at date: Date) -> Double {
        let phase = date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1.4) / 1.4
        return 0.5 + 0.5 * cos(phase * 2 * .pi)
    }
}

/// One of the app's icons: Lucide's line drawings, which the system draws
/// from their SVG and tints like text. Named as Lucide names them.
///
/// Every icon draws its lines 1.25 points wide whatever its size, so a
/// chevron beside a rail icon is not a hairline beside a marker: the SVG's
/// stroke is set for the size it is drawn at.
struct Icon: View {
    let name: String
    var size: CGFloat = 14
    /// A provider's mark in its own colour, where the provider is being
    /// chosen or named; elsewhere it takes the text's.
    var brand = false
    /// Drawn beside words, so set a little low. Centring a glyph on a line
    /// puts it on the capitals' middle, which reads high beside lowercase
    /// words; it sits halfway down to the lowercase middle instead. An
    /// icon alone in a button is centred in it.
    var optical = true

    init(_ name: String, size: CGFloat = 14, brand: Bool = false, optical: Bool = true) {
        self.name = name
        self.size = size
        self.brand = brand
        self.optical = optical
    }

    /// Half the distance from the capitals' middle to the lowercase
    /// middle at the text size an icon of `size` sits beside.
    private var drop: CGFloat {
        guard optical else { return 0 }
        if size <= 14 { return max(0, (size - 10) * 0.25) }
        return size <= 16 ? 1 : 0
    }

    var body: some View {
        if let symbol = Icons.symbols[name] {
            // The system's own symbols, drawn in the text's weight so a
            // glyph sits with the words beside it; a size is the square it
            // fills, as the line icons' was.
            Image(systemName: symbol)
                .font(.system(size: size * 0.86, weight: size <= 12 ? .medium : .regular))
                .imageScale(.medium)
                .frame(width: size, height: size)
                .offset(y: drop)
                .accessibilityHidden(true)
        } else {
            // Claude's mark keeps its own colour where it is branded, as the
            // providers' pickers draw it; Codex's is drawn in ink.
            Image(nsImage: Icons.image(name, size: size))
                .renderingMode(.template)
                .resizable()
                .interpolation(.high)
                .frame(width: size, height: size)
                .offset(y: drop)
                .foregroundStyle(brand && name == "provider-claude" ? AnyShapeStyle(Palette.claude) : AnyShapeStyle(.foreground))
                .accessibilityHidden(true)
        }
    }
}

/// The marks of the agent programs the app carries one for, each a drawing
/// in `Icons/` named `provider-<id>` after the id its host lists it by. Only
/// a mark whose owner's terms let the app carry it is there (`Icons/NOTICE`
/// says where each came from); any other agent, a person's own among them,
/// is named in its mark's place, never given one made up for it.
@MainActor
enum ProviderMark {
    /// The icon name of `provider`'s mark; nil when the app carries none.
    static func glyph(_ provider: String) -> String? {
        let name = "provider-\(provider)"
        return Icons.has(name) ? name : nil
    }
}

/// The icons, read once each per drawn size from the package's resources.
@MainActor
enum Icons {
    /// The SF Symbol drawn for each of the app's icon names. A name not
    /// here is one of the app's own drawings in `Icons/`: the providers'
    /// marks.
    static let symbols: [String: String] = [
        "agents": "sparkles.rectangle.stack",
        "archive-restore": "tray.and.arrow.up",
        "arrow-down": "arrow.down",
        "arrow-left": "arrow.left",
        "arrow-left-right": "arrow.left.arrow.right",
        "arrow-right": "arrow.right",
        "arrow-up": "arrow.up",
        "at-sign": "at",
        "ban": "nosign",
        "book-open": "book",
        "bot": "cpu",
        "brain": "brain",
        "check": "checkmark",
        "chevron-down": "chevron.down",
        "chevron-left": "chevron.left",
        "chevron-right": "chevron.right",
        "chevron-up": "chevron.up",
        "circle": "circle",
        "circle-alert": "exclamationmark.circle",
        "circle-check": "checkmark.circle",
        "circle-dashed": "circle.dashed",
        "circle-dot": "record.circle",
        "circle-help": "questionmark.circle",
        "circle-stop": "stop.circle",
        "clock": "clock",
        "cloud": "cloud",
        "cloud-off": "icloud.slash",
        "code": "chevron.left.forwardslash.chevron.right",
        "copy": "doc.on.doc",
        "corner-down-left": "arrow.turn.down.left",
        "corner-down-right": "arrow.turn.down.right",
        "corner-left-up": "arrow.turn.left.up",
        "corner-up-right": "arrow.turn.up.right",
        "diff": "plus.forwardslash.minus",
        "ellipsis": "ellipsis",
        "ellipsis-vertical": "ellipsis",
        "file-diff": "plus.forwardslash.minus",
        "file-minus": "trash",
        "file-pen": "doc.badge.ellipsis",
        "file-plus": "doc.badge.plus",
        "file-question": "doc.questionmark",
        "file-symlink": "doc.badge.arrow.up",
        "file-text": "doc.text",
        "file-x": "xmark.square",
        "folder": "folder",
        "folder-git-2": "folder",
        "folder-open": "folder",
        "git-branch": "arrow.triangle.branch",
        "globe": "globe",
        "hand": "hand.raised",
        "image": "photo",
        "info": "info.circle",
        "laptop": "laptopcomputer",
        "layers": "square.stack",
        "lightbulb": "lightbulb",
        "link": "link",
        "list-checks": "checklist",
        "lock": "lock",
        "lock-open": "lock.open",
        "message-circle-question": "questionmark.bubble",
        "message-square-text": "text.bubble",
        "messages-square": "bubble.left.and.bubble.right",
        "monitor": "display",
        "monitor-off": "display.trianglebadge.exclamationmark",
        "panel-left": "sidebar.left",
        "panel-left-close": "sidebar.left",
        "panel-left-open": "sidebar.left",
        "panel-right": "sidebar.right",
        "panel-right-close": "sidebar.right",
        "panel-right-open": "sidebar.right",
        "paperclip": "paperclip",
        "pencil": "pencil",
        "pin": "pin",
        "pin-off": "pin.slash",
        "play": "play",
        "plus": "plus",
        "refresh-cw": "arrow.clockwise",
        "search": "magnifyingglass",
        "server": "server.rack",
        "settings": "gearshape",
        "shield": "hand.raised",
        "shield-alert": "exclamationmark.shield",
        "shield-check": "checkmark.shield",
        "slash": "line.diagonal",
        "smartphone": "iphone",
        "sparkles": "sparkles",
        "square": "square",
        "square-check": "checkmark.square",
        "square-pen": "square.and.pencil",
        "square-terminal": "apple.terminal",
        "terminal": "terminal",
        "trash": "trash",
        "trash-2": "trash",
        "triangle-alert": "exclamationmark.triangle",
        "user-minus": "person.badge.minus",
        "user-plus": "person.badge.plus",
        "wrench": "wrench.adjustable",
        "x": "xmark"
    ]

    private static var loaded: [String: NSImage] = [:]
    /// Each drawing's source by name; empty for a name with none.
    private static var sources: [String: String] = [:]

    /// One of the app's own drawings, as a template image.
    static func image(_ name: String, size: CGFloat) -> NSImage {
        if let image = loaded[name] { return image }
        let image = source(name).flatMap { NSImage(data: Data($0.utf8)) } ?? NSImage()
        image.isTemplate = true
        loaded[name] = image
        return image
    }

    /// Whether the app carries a drawing by this name.
    static func has(_ name: String) -> Bool {
        source(name) != nil
    }

    private static func source(_ name: String) -> String? {
        // A name looked for and not found is remembered too: a fleet row
        // asks after its agent's mark each time it draws.
        if let svg = sources[name] { return svg.isEmpty ? nil : svg }
        let url = Bundle.module.url(forResource: name, withExtension: "svg", subdirectory: "Icons")
        let svg = url.flatMap { try? String(contentsOf: $0, encoding: .utf8) }
        sources[name] = svg ?? ""
        return svg
    }
}

/// The window's one button: a pill, the action expected filled in the
/// accent as the send button is, whether or not the window is in front
/// (the system's prominent style goes grey there); the others quiet fills;
/// a destructive one red words on the quiet fill, or filled red when it is
/// the expected one.
struct PillButtonStyle: ButtonStyle {
    var prominent = false
    var destructive = false
    var small = false
    /// Its trailing corners square, for a menu joined on its right.
    var joined = false
    @Environment(\.isEnabled) private var enabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let radius = small ? Radius.row : Radius.control
        let shape = UnevenRoundedRectangle(
            topLeadingRadius: radius, bottomLeadingRadius: radius, bottomTrailingRadius: joined ? 0 : radius,
            topTrailingRadius: joined ? 0 : radius, style: .continuous)
        configuration.label
            .font(.ui(small ? .meta : .body, weight: prominent ? .medium : .regular))
            .lineLimit(1)
            .foregroundStyle(prominent ? Color.white : (destructive ? Palette.failure : Color.primary))
            .padding(.horizontal, small ? 10 : 12)
            .frame(height: small ? 24 : 28)
            .background(shape.fill(fill))
            .opacity(configuration.isPressed ? 0.75 : (enabled ? 1 : 0.45))
            .contentShape(shape)
            .onHover { hovering = $0 }
    }

    private var fill: Color {
        if prominent {
            return (destructive ? Palette.failure : Palette.accent).opacity(hovering && enabled ? 0.88 : 1)
        }
        return Color.primary.opacity(hovering && enabled ? 0.1 : 0.065)
    }
}
