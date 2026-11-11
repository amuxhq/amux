import AmuxValues
import AmuxWords
import SwiftUI

/// Where the person writes to the agent: a field that grows with what is
/// typed, Return to send and Shift-Return for a new line, with the agent's
/// model and the send or stop button under it.
struct Composer: View {
    let chat: any ChatSurface
    let name: String
    let host: String
    let reach: Reach?

    @FocusState private var focused: Bool

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
                .font(.system(size: 13.5))
                .lineLimit(1...12)
                .focused($focused)
                .padding(.horizontal, 14)
                .padding(.top, 12)
                .padding(.bottom, 6)
                .onKeyPress(.return, phases: .down) { press in
                    if press.modifiers.contains(.shift) || press.modifiers.contains(.option) {
                        chat.draft += "\n"
                        return .handled
                    }
                    submit()
                    return .handled
                }
            HStack(spacing: 10) {
                if let chip = ChatWords.chip(chat.frame?.controls ?? empty, context: chat.frame?.context) {
                    HStack(spacing: 4) {
                        Text(chip.model).foregroundStyle(.secondary)
                        if !chip.detail.isEmpty { Text(chip.detail).foregroundStyle(.tertiary) }
                        ForEach(chip.unusual, id: \.self) { word in
                            Text("· \(word)").foregroundStyle(.orange)
                        }
                    }
                    .font(.system(size: 12))
                    .lineLimit(1)
                }
                Spacer()
                if working {
                    Button {
                        chat.interrupt()
                    } label: {
                        Image(systemName: "stop.fill")
                            .font(.system(size: 10))
                            .frame(width: 26, height: 26)
                            .background(Circle().fill(Color.primary.opacity(0.12)))
                    }
                    .buttonStyle(.plain)
                    .help("Stop")
                    .keyboardShortcut(".", modifiers: .command)
                }
                Button(action: submit) {
                    Image(systemName: chat.canResume ? "play.fill" : "arrow.up")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(ready ? Color(nsColor: .textBackgroundColor) : .secondary)
                        .frame(width: 26, height: 26)
                        .background(Circle().fill(ready ? Color.primary : Color.primary.opacity(0.12)))
                }
                .buttonStyle(.plain)
                .disabled(!ready)
                .help(chat.canResume ? "Resume" : "Send")
            }
            .padding(.horizontal, 10)
            .padding(.bottom, 10)
        }
        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Palette.line))
        .onTapGesture { focused = true }
        .onAppear { focused = true }
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

    private var ready: Bool {
        chat.canResume || (chat.canSend && !chat.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private var placeholder: String {
        ChatWords.placeholder(
            chat.frame?.composer.mode, agent: name, host: host, reach: reach ?? .online(.direct), working: working)
    }

    private func submit() {
        if chat.canResume {
            chat.resume()
        } else if ready {
            chat.send()
        }
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
                        Text(ChatWords.command(match.command)).font(.system(size: 12.5, design: .monospaced))
                        Text(match.command.description).font(.system(size: 12)).foregroundStyle(.secondary)
                            .lineLimit(1)
                        Spacer()
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 5)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
            }
        }
        .padding(.top, 6)
    }
}

/// Something going with the draft, which can be taken off it.
private struct Chip: View {
    let text: String
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "paperclip").font(.system(size: 10)).foregroundStyle(.secondary)
            Text(text).lineLimit(1)
            Button(action: remove) {
                Image(systemName: "xmark").font(.system(size: 8, weight: .bold)).foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Remove")
        }
        .font(.system(size: 12))
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(Capsule().fill(Palette.hover))
    }
}
