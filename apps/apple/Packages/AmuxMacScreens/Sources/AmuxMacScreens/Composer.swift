import AmuxValues
import AmuxWords
import SwiftUI
import UniformTypeIdentifiers

/// Where the person writes to the agent: a field that grows with what is
/// typed, Return to send and Shift-Return for a new line. Under it, as in
/// the references: attaching files, the permission the agent works under
/// and the model it thinks with, each a menu that changes the agent, then
/// stop or send. Files dropped on it are attached.
struct Composer: View {
    let chat: any ChatSurface
    let name: String
    let host: String
    let reach: Reach?

    @FocusState private var focused: Bool
    @State private var dropping = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !chat.slashMatches.isEmpty {
                commands
            }
            if !chat.attachments.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(Array(chat.attachments.enumerated()), id: \.offset) { index, attachment in
                        Chip(text: Self.chip(attachment)) { chat.removeAttachment(at: index) }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 10)
            }
            TextField(placeholder, text: draft, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.ui(.prose))
                .lineLimit(1...12)
                .focused($focused)
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 8)
                .onKeyPress(.return, phases: .down) { press in
                    if press.modifiers.contains(.shift) || press.modifiers.contains(.option) {
                        chat.draft += "\n"
                        return .handled
                    }
                    submit()
                    return .handled
                }
            controls
        }
        .background(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).fill(Palette.raised))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.composer, style: .continuous)
                .strokeBorder(dropping ? Palette.accent : (focused ? Color.primary.opacity(0.14) : Palette.line)))
        .elevation(.raised)
        .onTapGesture { focused = true }
        .onAppear { focused = true }
        .onDrop(of: [.fileURL], isTargeted: $dropping) { providers in
            Self.urls(from: providers) { chat.attach(files: $0) }
            return true
        }
    }

    // MARK: - Under the field

    private var controls: some View {
        HStack(spacing: 4) {
            Button(action: pickFiles) {
                Icon("plus", size: 16)
                    .foregroundStyle(.secondary)
                    .frame(width: Metrics.control, height: Metrics.control)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Attach files")
            permission
            Spacer(minLength: 8)
            model
            // One button where the references have one: Stop while the agent
            // works and nothing is typed, else Send (which queues while it
            // works). ⌘. stops either way.
            let stopping = working && !typed
            Button {
                if stopping { chat.interrupt() } else { submit() }
            } label: {
                Group {
                    if stopping {
                        RoundedRectangle(cornerRadius: 2.5, style: .continuous)
                            .frame(width: 9, height: 9)
                    } else {
                        Icon(chat.canResume ? "play" : "arrow-up", size: 15)
                    }
                }
                .foregroundStyle(stopping || ready ? Color(nsColor: .textBackgroundColor) : Color.secondary)
                .frame(width: Metrics.control, height: Metrics.control)
                .background(Circle().fill(stopping || ready ? Color.primary : Color.primary.opacity(0.1)))
                .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .disabled(!stopping && !ready)
            .help(stopping ? "Stop (⌘.)" : (chat.canResume ? "Resume" : (working ? "Queue (Return)" : "Send (Return)")))
            .background {
                Button("Stop") { chat.interrupt() }
                    .keyboardShortcut(".", modifiers: .command)
                    .disabled(!working)
                    .opacity(0)
                    .frame(width: 0, height: 0)
                    .accessibilityHidden(true)
            }
        }
        .padding(.horizontal, 8)
        .padding(.bottom, 8)
    }

    /// The permission the agent works under: a menu where it can be picked
    /// from here, orange where it acts without asking.
    @ViewBuilder private var permission: some View {
        if let settings = chat.settings, let current = settings.permissions.first(where: \.current) {
            let label = ChipLabel(
                icon: current.neverAsks ? "circle-alert" : "hand", text: ChatWords.permission(current),
                tint: current.neverAsks ? Palette.caution : .secondary, menu: settings.changeable.permission == .pick)
            if settings.changeable.permission == .pick {
                Menu {
                    ForEach(settings.permissions.filter(\.settable), id: \.value) { choice in
                        Toggle(
                            isOn: Binding(get: { choice.current }, set: { _ in chat.change(.permission(choice.value)) })
                        ) {
                            Text(ChatWords.permission(choice))
                            if !ChatWords.permissionDetail(choice).isEmpty {
                                Text(ChatWords.permissionDetail(choice))
                            }
                        }
                    }
                } label: {
                    label
                }
                .menuStyle(.button)
                .buttonStyle(HoverButtonStyle())
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Permission")
            } else {
                label.help("Permission")
            }
        }
    }

    /// The model and effort: "Opus 5.5 High ⌄", a menu of what the agent
    /// offers.
    @ViewBuilder private var model: some View {
        if let chip = ChatWords.chip(chat.frame?.controls ?? empty, context: chat.frame?.context) {
            let text = [chip.model, chip.detail].filter { !$0.isEmpty }.joined(separator: " ")
            let settings = chat.settings
            let picks = settings.map { $0.changeable.model == .pick || $0.changeable.effort == .pick } ?? false
            let label = ChipLabel(icon: nil, text: text, tint: .secondary, menu: picks)
            if let settings, picks {
                Menu {
                    if settings.changeable.model == .pick {
                        Section("Model") {
                            ForEach(settings.models, id: \.value) { choice in
                                Toggle(
                                    choice.displayName,
                                    isOn: Binding(get: { choice.current }, set: { _ in chat.change(.model(choice.value)) }))
                            }
                        }
                    }
                    if settings.changeable.effort == .pick, !settings.efforts.isEmpty {
                        Section("Effort") {
                            ForEach(settings.efforts, id: \.value) { choice in
                                Toggle(
                                    choice.value.capitalized,
                                    isOn: Binding(get: { choice.current }, set: { _ in chat.change(.effort(choice.value)) }))
                            }
                        }
                    }
                    if settings.changeable.mode == .pick, !settings.modes.isEmpty {
                        Section("Mode") {
                            ForEach(settings.modes, id: \.value) { choice in
                                Toggle(
                                    ChatWords.mode(choice),
                                    isOn: Binding(get: { choice.current }, set: { _ in chat.change(.mode(choice.value)) }))
                            }
                        }
                    }
                } label: {
                    label
                }
                .menuStyle(.button)
                .buttonStyle(HoverButtonStyle())
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Model and effort")
            } else {
                label
            }
        }
    }

    private var empty: ControlsSummary { ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil) }

    private var draft: Binding<String> {
        Binding(get: { chat.draft }, set: { chat.draft = $0 })
    }

    private var working: Bool {
        guard let phase = chat.frame?.phase else { return false }
        if case .working = phase { return true }
        return false
    }

    private var typed: Bool { !chat.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    private var ready: Bool {
        chat.canResume || (chat.canSend && !chat.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private var placeholder: String {
        ChatWords.placeholder(
            chat.frame?.composer.mode, host: host, reach: reach ?? .online(.direct), working: working)
    }

    private func submit() {
        if chat.canResume {
            chat.resume()
        } else if ready {
            chat.send()
        }
    }

    private func pickFiles() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.message = "Attach files for \(name)"
        if panel.runModal() == .OK { chat.attach(files: panel.urls) }
    }

    /// The file URLs dropped, read off the main thread and handed back on it.
    static func urls(from providers: [NSItemProvider], _ done: @escaping @MainActor ([URL]) -> Void) {
        let group = DispatchGroup()
        let found = Found()
        for provider in providers where provider.canLoadObject(ofClass: URL.self) {
            group.enter()
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                if let url, url.isFileURL { found.add(url) }
                group.leave()
            }
        }
        group.notify(queue: .main) { MainActor.assumeIsolated { done(found.urls) } }
    }

    private final class Found: @unchecked Sendable {
        private let lock = NSLock()
        private var list: [URL] = []
        func add(_ url: URL) { lock.withLock { list.append(url) } }
        var urls: [URL] { lock.withLock { list } }
    }

    static func chip(_ attachment: DraftAttachment) -> String {
        switch attachment {
        case .image(let blob), .file(let blob): blob.name
        case .text(let name, _): name
        case .review(_, let comments): "Review · \(ChatWords.comments(comments.count))"
        }
    }

    /// The slash commands the draft matches, to pick one.
    private var commands: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(chat.slashMatches.enumerated()), id: \.offset) { _, match in
                Button {
                    chat.pick(match)
                } label: {
                    HStack(spacing: 8) {
                        Text(ChatWords.command(match.command)).font(.mono(.callout))
                        Text(match.command.description).font(.ui(.meta)).foregroundStyle(.secondary)
                            .lineLimit(1)
                        Spacer()
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 26)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
            }
        }
        .padding(.horizontal, 6)
        .padding(.top, 6)
    }
}

/// A control under the composer: words, perhaps an icon, and a chevron
/// where it opens a menu.
private struct ChipLabel: View {
    let icon: String?
    let text: String
    let tint: Color
    let menu: Bool

    var body: some View {
        HStack(spacing: 5) {
            if let icon { Icon(icon, size: 13) }
            Text(text).lineLimit(1)
            if menu { Icon("chevron-down", size: 11).opacity(0.7) }
        }
        .font(.ui(.callout))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .frame(height: 28)
        .contentShape(Rectangle())
    }
}

/// Something going with the draft, which can be taken off it.
private struct Chip: View {
    let text: String
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 5) {
            Icon("paperclip", size: 12).foregroundStyle(.secondary)
            Text(text).lineLimit(1)
            Button(action: remove) {
                Icon("x", size: 11).foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Remove")
        }
        .font(.ui(.meta))
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(RoundedRectangle(cornerRadius: Radius.row, style: .continuous).fill(Palette.well))
    }
}
