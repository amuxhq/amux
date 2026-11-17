import AppKit
import AmuxValues
import AmuxWords
import SwiftUI

/// What can be done about the open agent from around its chat: the
/// changes pane, its relatives' chats, and its folder in the tools a
/// developer already has open.
public struct AgentActions {
    public var openChanges: @MainActor () -> Void
    public var openAgent: @MainActor (AgentKey) -> Void
    /// Its own interface in Terminal, through `amux attach`.
    public var openTerminal: @MainActor () -> Void
    /// Its folder in Finder and in the editor, for an agent on this Mac.
    public var reveal: (@MainActor () -> Void)?
    public var openEditor: (@MainActor () -> Void)?
    /// The editor's name, for its line.
    public var editor: String
    /// The apps' own icons, so each action is known at a glance: the
    /// editor's, Terminal's and Finder's.
    public var editorIcon: NSImage?
    public var terminalIcon: NSImage?
    public var finderIcon: NSImage?

    public init(
        openChanges: @escaping @MainActor () -> Void = {}, openAgent: @escaping @MainActor (AgentKey) -> Void = { _ in },
        openTerminal: @escaping @MainActor () -> Void = {}, reveal: (@MainActor () -> Void)? = nil,
        openEditor: (@MainActor () -> Void)? = nil, editor: String = "Editor", editorIcon: NSImage? = nil,
        terminalIcon: NSImage? = nil, finderIcon: NSImage? = nil
    ) {
        self.editor = editor
        self.editorIcon = editorIcon
        self.terminalIcon = terminalIcon
        self.finderIcon = finderIcon
        self.openChanges = openChanges
        self.openAgent = openAgent
        self.openTerminal = openTerminal
        self.reveal = reveal
        self.openEditor = openEditor
    }
}

/// Where the open agent works, at the chat's top right while the changes
/// pane is closed, as the references' cards are: its host, folder and
/// branch; its changes; its family as lineage; and its folder opened
/// elsewhere.
struct FloatingCard: View {
    nonisolated static let width: CGFloat = 248

    let row: FleetRow
    let branch: String?
    let changes: ChangeTotals?
    let family: FamilyHeader?
    let actions: AgentActions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            group {
                Line(symbol: HostSymbol.of(row), text: PlaceNames.host(row.hostName))
                    .help(PlaceNames.path(row.workingDirectory))
                if let branch {
                    Line(symbol: "git-branch", text: branch)
                }
                // A provider subagent works in its parent's folder and has
                // no changes, terminal or folder of its own to open.
                if !row.native {
                    Button(action: actions.openChanges) {
                        Line(symbol: "diff", text: "Changes") { ChangeCounts(changes: changes) }
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help("Show the changes (⌥⌘I)")
                }
            }
            if let family, family.parent != nil || !family.children.isEmpty {
                Divider().padding(.horizontal, 10)
                VStack(alignment: .leading, spacing: 0) {
                    Text("Lineage")
                        .font(.ui(.small))
                        .foregroundStyle(.tertiary)
                        .padding(.horizontal, 16)
                        .padding(.top, 8)
                        .padding(.bottom, 2)
                    group {
                        if let parent = family.parent {
                            relative(parent, symbol: "corner-left-up")
                        }
                        ForEach(family.children.prefix(6), id: \.agent) { child in
                            relative(child, symbol: "corner-down-right")
                        }
                    }
                }
            }
            if !row.native {
                Divider().padding(.horizontal, 10)
                group { openElsewhere }
            }
        }
        .font(.ui(.callout))
        .frame(width: Self.width)
        .background(RoundedRectangle(cornerRadius: Radius.panel, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.panel, style: .continuous).strokeBorder(Palette.line))
        .elevation(.floating)
    }

    /// The agent's folder in the editor and Finder, and its own interface
    /// in Terminal.
    @ViewBuilder private var openElsewhere: some View {
                if let openEditor = actions.openEditor {
                    Button(action: openEditor) {
                        Line(app: actions.editorIcon, symbol: "code", text: "Open in \(actions.editor)")
                    }
                    .buttonStyle(HoverButtonStyle())
                }
                Button(action: actions.openTerminal) {
                    Line(app: actions.terminalIcon, symbol: "square-terminal", text: "Open in Terminal")
                }
                .buttonStyle(HoverButtonStyle())
                .help("Its own interface, through amux attach (⇧⌘T)")
                if let reveal = actions.reveal {
                    Button(action: reveal) {
                        Line(app: actions.finderIcon, symbol: "folder-open", text: "Show in Finder")
                    }
                    .buttonStyle(HoverButtonStyle())
                }
    }

    private func group(@ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 0) { content() }.padding(.vertical, 6).padding(.horizontal, 6)
    }

    private func relative(_ card: FleetCard, symbol: String) -> some View {
        Button {
            actions.openAgent(card.agent)
        } label: {
            Line(symbol: symbol, text: card.name.isEmpty ? "agent" : card.name) {
                if card.attention == .needsYou {
                    Circle().fill(Palette.you).frame(width: 6, height: 6)
                } else {
                    Text(ChatWords.childState(card)).foregroundStyle(.tertiary).lineLimit(1)
                }
            }
        }
        .buttonStyle(HoverButtonStyle())
    }
}

/// "+6 −3", or "none".
struct ChangeCounts: View {
    let changes: ChangeTotals?

    var body: some View {
        if let changes, changes.files > 0 {
            HStack(spacing: 4) {
                Text("+\(changes.added)").foregroundStyle(Palette.added)
                Text("−\(changes.removed)").foregroundStyle(Palette.removed)
            }
            .monospacedDigit()
        } else {
            Text("none").foregroundStyle(.tertiary)
        }
    }
}

/// The card folded to its changes, where the window is too narrow for it
/// beside the chat.
struct FloatingPill: View {
    let changes: ChangeTotals?
    let open: @MainActor () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 6) {
                Icon("diff", size: 13).foregroundStyle(.secondary)
                ChangeCounts(changes: changes)
            }
            .font(.ui(.meta))
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(Capsule().fill(Palette.raised))
            .overlay(Capsule().strokeBorder(Palette.line))
            .elevation(.raised)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Show the changes (⌥⌘I)")
    }
}

/// One line of the card: an icon, words, and what goes at its end.
private struct Line<Trailing: View>: View {
    var app: NSImage?
    let symbol: String
    let text: String
    @ViewBuilder let trailing: Trailing

    init(
        app: NSImage? = nil, symbol: String, text: String, @ViewBuilder trailing: () -> Trailing = { EmptyView() }
    ) {
        self.app = app
        self.symbol = symbol
        self.text = text
        self.trailing = trailing()
    }

    var body: some View {
        HStack(spacing: 9) {
            Group {
                if let app {
                    Image(nsImage: app).resizable().interpolation(.high).frame(width: 18, height: 18)
                } else {
                    Icon(symbol, size: 14).foregroundStyle(.secondary)
                }
            }
            .frame(width: 18)
            Text(text).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 6)
            trailing
        }
        .padding(.horizontal, 8)
        .frame(height: 28)
        .contentShape(Rectangle())
    }
}

enum HostSymbol {
    static func of(_ row: FleetRow) -> String {
        switch row.hostReach {
        case .online: "monitor"
        case .away: "cloud"
        case .offline: "monitor-off"
        }
    }
}
