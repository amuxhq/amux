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
    /// Its own interface in Terminal, through `amux attach`; nil for an
    /// agent that has no terminal interface of its own.
    public var openTerminal: (@MainActor () -> Void)?
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
        openTerminal: (@MainActor () -> Void)? = nil, reveal: (@MainActor () -> Void)? = nil,
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

    /// Whether anything opens the agent elsewhere: its folder, or its own
    /// interface.
    var opens: Bool { openEditor != nil || reveal != nil || openTerminal != nil }
}

/// What the window's bars show of the open agent beyond its row: the
/// branch its folder is on now, its changes, its relatives, and what can
/// be done about it from around its chat.
public struct OpenAgentBar {
    public var branch: String?
    public var changes: ChangeTotals?
    public var family: FamilyHeader?
    public var actions: AgentActions

    public init(
        branch: String? = nil, changes: ChangeTotals? = nil, family: FamilyHeader? = nil,
        actions: AgentActions = AgentActions()
    ) {
        self.branch = branch
        self.changes = changes
        self.family = family
        self.actions = actions
    }
}

/// The open agent's folder in the tools a developer already has: one
/// button in the chat's bar showing the editor's own icon, its menu the
/// editor, Finder, and the agent's own interface in Terminal.
struct OpenMenu: View {
    let actions: AgentActions

    var body: some View {
        Menu {
            if let openEditor = actions.openEditor {
                Button(action: openEditor) {
                    Label { Text("Open in \(actions.editor)") } icon: { app(actions.editorIcon) }
                }
            }
            if let reveal = actions.reveal {
                Button(action: reveal) {
                    Label { Text("Show in Finder") } icon: { app(actions.finderIcon) }
                }
            }
            if let openTerminal = actions.openTerminal {
                if actions.openEditor != nil || actions.reveal != nil { Divider() }
                Button(action: openTerminal) {
                    Label { Text("Attach in Terminal") } icon: { app(actions.terminalIcon) }
                }
                .keyboardShortcut("t", modifiers: [.command, .shift])
            }
        } label: {
            HStack(spacing: 4) {
                if let icon = actions.openEditor != nil ? actions.editorIcon : actions.terminalIcon {
                    Image(nsImage: icon).resizable().interpolation(.high).frame(width: 16, height: 16)
                } else {
                    Icon("square-terminal", size: 16).foregroundStyle(Palette.textSecondary)
                }
                Icon("chevron-down", size: 12, optical: false).foregroundStyle(Palette.textTertiary)
            }
            .padding(.horizontal, 6)
            .frame(height: 26)
            .contentShape(Rectangle())
        }
        .menuStyle(.button)
        .buttonStyle(HoverButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help(actions.openEditor != nil ? "Open in \(actions.editor)" : "Attach in Terminal")
    }

    @ViewBuilder private func app(_ icon: NSImage?) -> some View {
        if let icon { Image(nsImage: icon) }
    }
}

/// The open agent's parent and children, for the menu on its name: each
/// opens its chat.
struct LineageItems: View {
    let family: FamilyHeader?
    let open: @MainActor (AgentKey) -> Void

    var body: some View {
        if let family, family.parent != nil || !family.children.isEmpty {
            Divider()
            if let parent = family.parent {
                Section("Started by") {
                    Button(name(parent)) { open(parent.agent) }
                }
            }
            if !family.children.isEmpty {
                Section("Agents it started") {
                    ForEach(family.children, id: \.agent) { child in
                        Button("\(name(child)) · \(ChatWords.childState(child))") { open(child.agent) }
                    }
                }
            }
        }
    }

    private func name(_ card: FleetCard) -> String { card.name.isEmpty ? "agent" : card.name }
}

/// "+6 −3", or "No changes".
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
            Text("No changes").foregroundStyle(Palette.textTertiary)
        }
    }
}
