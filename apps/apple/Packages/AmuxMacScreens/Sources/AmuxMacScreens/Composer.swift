import AmuxValues
import AmuxWords
import SwiftUI
import UniformTypeIdentifiers

/// Where the person writes to the agent: a field that grows with what is
/// typed, Return to send and Shift-Return for a new line. Under it:
/// attaching files, the permission the agent works under
/// and the model it thinks with, each a menu that changes the agent, then
/// stop or send. Files dropped on it are attached.
struct Composer: View {
    let chat: any ChatSurface
    let name: String
    let host: String
    let reach: Reach?
    /// Opens the agent's review page in the tool window.
    var openReview: @MainActor () -> Void = {}

    @FocusState private var focused: Bool
    @State private var dropping = false
    /// The model picker is open over the composer.
    @State private var pickingModel = false

    var body: some View {
        ComposerShell(
            placeholder: placeholder, text: draft, lines: 2...12, focused: $focused, highlighted: dropping,
            submit: submit
        ) {
            if !chat.slashMatches.isEmpty {
                commands
            }
            if !chat.attachments.isEmpty || chat.reviewChip != nil {
                FlowLayout(spacing: 6) {
                    if let chip = chat.reviewChip {
                        ReviewChipView(chip: chip, open: openReview, discard: { chat.discardReviewDrafts() })
                    }
                    ForEach(Array(chat.attachments.enumerated()), id: \.offset) { index, attachment in
                        Chip(text: Self.chip(attachment)) { chat.removeAttachment(at: index) }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 10)
            }
        } controls: {
            Button(action: pickFiles) {
                Icon("paperclip", size: 16, optical: false)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: Metrics.control, height: Metrics.control)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Attach files")
            Spacer(minLength: 8)
            model
            permission
        } action: {
            // One button for both: Stop while the agent works and there is
            // nothing to send, else Send (which queues while it works).
            // ⌘. stops either way.
            let stopping = working && !chat.hasMessage
            RoundButton(
                face: stopping ? .stop : (chat.canResume ? .resume : .send), enabled: stopping || ready,
                help: stopping ? "Stop (⌘.)" : (chat.canResume ? "Resume" : (working ? "Queue (↩)" : "Send (↩)"))
            ) {
                if stopping { chat.interrupt() } else { submit() }
            }
            .background {
                Button("Stop") { chat.interrupt() }
                    .keyboardShortcut(".", modifiers: .command)
                    .disabled(!working)
                    .opacity(0)
                    .frame(width: 0, height: 0)
                    .accessibilityHidden(true)
            }
        }
        .onTapGesture { focused = true }
        .onAppear { focused = true }
        .onDrop(of: [.fileURL], isTargeted: $dropping) { providers in
            Self.urls(from: providers) { chat.attach(files: $0) }
            return true
        }
    }

    /// The permission the agent works under: a menu where it can be picked
    /// from here, orange where it acts without asking.
    @ViewBuilder private var permission: some View {
        if let settings = chat.settings, let current = settings.permissions.first(where: \.current) {
            let label = ChipLabel(
                icon: current.neverAsks ? "shield-alert" : "lock", text: ChatWords.permission(current),
                tint: current.neverAsks ? Palette.caution : Palette.textSecondary, menu: settings.changeable.permission == .pick)
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

    /// The model and effort: "Opus 5.5 High ⌄", opening the picker of
    /// what the agent offers.
    @ViewBuilder private var model: some View {
        if let chip = ChatWords.chip(chat.frame?.controls ?? empty, context: chat.frame?.context) {
            let settings = chat.settings
            let picks = settings.map { $0.changeable.model == .pick || $0.changeable.effort == .pick } ?? false
            let label = ChipLabel(
                icon: chat.frame.map { FleetRowView.providerGlyph($0.kind) }, text: chip.model,
                detail: chip.detail.isEmpty ? nil : chip.detail, tint: .primary, menu: picks)
            if settings != nil, picks {
                Button { pickingModel.toggle() } label: { label }
                    .buttonStyle(HoverButtonStyle())
                    .fixedSize()
                    .popover(isPresented: $pickingModel, arrowEdge: .top) {
                        ModelPicker(
                            provider: nil, settings: chat.settings,
                            chooseModel: { chat.change(.model($0)) },
                            chooseEffort: { chat.change(.effort($0)) },
                            chooseMode: { chat.change(.mode($0)) })
                    }
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

    /// Words, an attachment or the review's drafts to send: the review goes
    /// with or without words.
    private var ready: Bool {
        chat.canResume || (chat.canSend && chat.hasMessage)
    }

    private var placeholder: String {
        ChatWords.placeholder(
            chat.frame?.composer.mode, host: host, reach: reach ?? .online(.direct), working: working, device: .mac)
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
        case .page(let page):
            "\(page.title.isEmpty ? page.url : page.title) · \(ChatWords.comments(page.comments.count))"
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
                        Text(match.command.description).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
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

/// The composer's frame, the chat's and the New Agent page's alike: what
/// goes with the message above the field, a field that grows as it is
/// typed into (Return submits, Shift- or Option-Return starts a new line),
/// and under it the controls, with the round button at the end. The
/// controls set their own spacing up to the button.
struct ComposerShell<Above: View, Controls: View, Action: View>: View {
    let placeholder: String
    @Binding var text: String
    let lines: ClosedRange<Int>
    var focused: FocusState<Bool>.Binding
    /// Files are being dragged over it.
    var highlighted = false
    let submit: () -> Void
    @ViewBuilder let above: Above
    @ViewBuilder let controls: Controls
    @ViewBuilder let action: Action
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            above
            TextField(placeholder, text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.ui(.prose))
                .lineSpacing(6)
                .lineLimit(lines)
                .focused(focused)
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 10)
                .onKeyPress(.return, phases: .down) { press in
                    if press.modifiers.contains(.shift) || press.modifiers.contains(.option) {
                        text += "\n"
                        return .handled
                    }
                    submit()
                    return .handled
                }
            HStack(spacing: 2) {
                controls
                action.padding(.leading, 4)
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
        .background(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).fill(Palette.raised))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.composer, style: .continuous)
                .strokeBorder(
                    highlighted ? Palette.accent : (focused.wrappedValue ? Color.primary.opacity(0.16) : Palette.line)))
        // On white a hairline alone leaves it flat; on black a shadow
        // would not show.
        .shadow(color: .black.opacity(scheme == .light ? 0.06 : 0), radius: 10, y: 3)
    }
}

/// The round button at the composer's end: send, resume, stop, or a
/// spinner while an agent starts.
struct RoundButton: View {
    enum Face {
        case send
        case resume
        case stop
        case starting
    }

    let face: Face
    let enabled: Bool
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Group {
                switch face {
                case .stop:
                    // A third of the disc, as the phone's stop is.
                    RoundedRectangle(cornerRadius: 2, style: .continuous)
                        .frame(width: 9, height: 9)
                case .starting:
                    ProgressView().controlSize(.small)
                case .send, .resume:
                    Icon(face == .resume ? "play" : "arrow-up", size: 13, optical: false)
                }
            }
            // Ready: blue, the window's one coloured control. Stopping: the
            // text's ink, as the phone draws it — stopping is ordinary, not
            // an alarm. Not ready: a grey disc, there but not asking.
            .foregroundStyle(glyph)
            .frame(width: Metrics.send, height: Metrics.send)
            .background(Circle().fill(fill))
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        // Not ready is drawn as such, not dimmed again by disabling.
        .allowsHitTesting(enabled)
        .accessibilityAddTraits(enabled ? [] : .isStaticText)
        .help(help)
    }

    private var fill: Color {
        guard enabled else { return Color.primary.opacity(0.12) }
        return face == .stop ? Palette.text : Palette.accent
    }

    private var glyph: Color {
        guard enabled else { return Palette.textSecondary }
        return face == .stop ? Palette.content : .white
    }
}

/// A control under the composer: what it is set to, perhaps an icon, and
/// a chevron where it opens a menu.
struct ChipLabel: View {
    let icon: String?
    let text: String?
    /// Said after the text, quieter: the effort after the model.
    var detail: String?
    var tint: Color = Palette.textSecondary
    var menu = true

    var body: some View {
        HStack(spacing: 5) {
            if let icon { Icon(icon, size: 13, brand: true) }
            if let text { Text(text).lineLimit(1) }
            if let detail { Text(detail).foregroundStyle(Palette.textSecondary).lineLimit(1) }
            if menu { Icon("chevron-down", size: 11).foregroundStyle(Palette.textTertiary) }
        }
        .font(.ui(.body))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .frame(height: Metrics.control)
        .contentShape(Rectangle())
    }
}

/// Something going with the draft, which can be taken off it.
private struct Chip: View {
    let text: String
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 5) {
            Icon("paperclip", size: 12).foregroundStyle(Palette.textSecondary)
            Text(text).lineLimit(1)
            Button(action: remove) {
                Icon("x", size: 11).foregroundStyle(Palette.textSecondary)
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

/// The agent's review riding on the next send: "Review · 3 comments",
/// which opens the page, and its ✕, which discards the drafts once the
/// person says so. There is no taking the review off without discarding:
/// the drafts are the host's, and whatever holds them is what is sent.
private struct ReviewChipView: View {
    let chip: ReviewChip
    let open: @MainActor () -> Void
    let discard: @MainActor () -> Void
    @State private var confirming = false

    var body: some View {
        HStack(spacing: 5) {
            Button(action: open) {
                HStack(spacing: 5) {
                    Icon("message-square-text", size: 12).foregroundStyle(Palette.textSecondary)
                    Text(ChatWords.reviewChip(chip)).lineLimit(1)
                    if chip.moved {
                        Text(ChatWords.reviewChipMoved).foregroundStyle(Palette.textTertiary).lineLimit(1)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Open the review")
            Button {
                confirming = true
            } label: {
                Icon("x", size: 11).foregroundStyle(Palette.textSecondary)
            }
            .buttonStyle(.plain)
            .help("Discard drafts")
        }
        .font(.ui(.meta))
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(RoundedRectangle(cornerRadius: Radius.row, style: .continuous).fill(Palette.well))
        .confirmationDialog(ChatWords.discardDrafts(chip.comments), isPresented: $confirming) {
            Button("Discard Drafts", role: .destructive, action: discard)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(ChatWords.discardDraftsDetail)
        }
    }
}
