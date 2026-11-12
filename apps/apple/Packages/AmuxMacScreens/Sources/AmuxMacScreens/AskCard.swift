import AmuxValues
import AmuxWords
import SwiftUI

/// What the agent waits on, above the composer: what it wants, in full,
/// and the answers it offers.
struct AskCardView: View {
    let card: AskCard
    let chat: any ChatSurface

    @State private var noting: Int?
    @State private var note = ""
    @State private var picks: [Set<UInt32>] = []
    @State private var others: [String] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Icon("hand", size: 15).foregroundStyle(Palette.you)
                Text(ChatWords.headline(card)).fontWeight(.semibold)
                Spacer()
                if let position = ChatWords.position(card) {
                    Text(position).foregroundStyle(.tertiary).font(.system(size: 12))
                }
            }
            .font(.system(size: 13))
            content
            state
            if case .question(let questions) = card.body {
                questionButtons(questions)
            } else {
                choices
            }
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Palette.you.opacity(0.5)))
        .onChange(of: card.key, initial: true) { _, _ in reset() }
    }

    private func reset() {
        noting = nil
        note = ""
        if case .question(let questions) = card.body {
            picks = questions.map { question in
                Set(question.options.indices.filter { question.options[$0].recommended }.prefix(1).map(UInt32.init))
            }
            others = questions.map { _ in "" }
        }
    }

    // MARK: - What it wants

    @ViewBuilder private var content: some View {
        switch card.body {
        case .command(let command, let cwd, let reason, let description):
            if !description.isEmpty || !reason.isEmpty {
                Text(description.isEmpty ? reason : description).foregroundStyle(.secondary)
            }
            Mono(text: command)
            if !cwd.isEmpty {
                Text("in \(PlaceNames.path(cwd))").font(.system(size: 11)).foregroundStyle(.tertiary)
            }
        case .edit(let path, _, let added, let removed, let lines, let reason, _, _):
            if !reason.isEmpty { Text(reason).foregroundStyle(.secondary) }
            HStack(spacing: 6) {
                Text(path).font(.system(size: 12, design: .monospaced)).lineLimit(1).truncationMode(.head)
                Text("+\(added)").foregroundStyle(Palette.added)
                Text("−\(removed)").foregroundStyle(Palette.removed)
            }
            .font(.system(size: 12))
            Patch(lines: lines)
        case .tool(_, _, let arguments):
            if !arguments.isEmpty { Mono(text: arguments, lines: 12) }
        case .question(let questions):
            ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                Question(
                    question: question,
                    picked: Binding(
                        get: { index < picks.count ? picks[index] : [] },
                        set: { if index < picks.count { picks[index] = $0 } }),
                    other: Binding(
                        get: { index < others.count ? others[index] : "" },
                        set: { if index < others.count { others[index] = $0 } }))
            }
        case .plan(let body, let title):
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if let title { Text(title).font(.system(size: 15, weight: .semibold)) }
                    ProseView(source: body, streaming: false, quiet: false)
                }
                .padding(.trailing, 8)
            }
            .frame(maxHeight: 280)
        case .form(_, let message, let fields):
            Text(message).foregroundStyle(.secondary)
            Text(fields.map(\.title).joined(separator: ", ")).font(.system(size: 12)).foregroundStyle(.tertiary)
        case .link(_, let message, let url, _):
            Text(message).foregroundStyle(.secondary)
            if let link = URL(string: url) { Link(url, destination: link).font(.system(size: 12)) }
        case .access(let reason, let read, let write, let network, let hosts):
            if !reason.isEmpty { Text(reason).foregroundStyle(.secondary) }
            VStack(alignment: .leading, spacing: 2) {
                if !read.isEmpty { Text("Read \(read.joined(separator: ", "))") }
                if !write.isEmpty { Text("Write \(write.joined(separator: ", "))") }
                if network { Text(hosts.isEmpty ? "Network" : "Network to \(hosts.joined(separator: ", "))") }
            }
            .font(.system(size: 12, design: .monospaced))
        case .unanswerable(let reason):
            Text(reason).foregroundStyle(.secondary)
            Text("Answer it in the agent's own terminal, or stop it.")
                .font(.system(size: 12))
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder private var state: some View {
        switch card.state {
        case .sending:
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text("Sending…").foregroundStyle(.secondary)
            }
            .font(.system(size: 12))
        case .rejected(let refusal):
            Text(refusal.detail ?? "The answer was refused").font(.system(size: 12)).foregroundStyle(Palette.failure)
        case .notConfirmed:
            Text("The answer may not have arrived").font(.system(size: 12)).foregroundStyle(Palette.caution)
        case .open, .dismissed:
            EmptyView()
        }
    }

    // MARK: - Answers

    @ViewBuilder private var choices: some View {
        if let index = noting {
            HStack(spacing: 8) {
                TextField("Say why (optional)", text: $note)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { send(index) }
                Button("Cancel") { noting = nil }
                    .buttonStyle(PillButtonStyle())
                Button(ChatWords.button(card.choices[index]).replacingOccurrences(of: "…", with: "")) { send(index) }
                    .buttonStyle(PillButtonStyle(prominent: true))
                    .keyboardShortcut(.defaultAction)
            }
        } else {
            FlowButtons {
                ForEach(Array(card.choices.enumerated()), id: \.offset) { index, choice in
                    let prominent = choice.primary && !card.defaultToNo
                    Button(ChatWords.button(choice)) {
                        if choice.takesNote { noting = index } else { chat.answer(choice: index, note: nil) }
                    }
                    .buttonStyle(PillButtonStyle(prominent: prominent))
                    .disabled(card.state == .sending)
                }
            }
        }
    }

    private func send(_ index: Int) {
        chat.answer(choice: index, note: note.isEmpty ? nil : note)
        noting = nil
        note = ""
    }

    private func questionButtons(_ questions: [QuestionView]) -> some View {
        HStack(spacing: 8) {
            Spacer()
            if card.questionSkip {
                Button("Skip") {
                    chat.answer(responses: questions.map { _ in QuestionResponse(note: nil, other: nil, picked: nil) })
                }
                .buttonStyle(PillButtonStyle())
            }
            Button("Answer") {
                chat.answer(
                    responses: questions.indices.map { index in
                        let other = index < others.count && !others[index].isEmpty ? others[index] : nil
                        let picked = index < picks.count ? picks[index].sorted() : []
                        return QuestionResponse(note: nil, other: other, picked: picked.isEmpty ? nil : picked)
                    })
            }
            .buttonStyle(PillButtonStyle(prominent: true))
            .keyboardShortcut(.defaultAction)
            .disabled(card.state == .sending)
        }
    }
}

/// One question: its options to pick, and room for another answer.
private struct Question: View {
    let question: QuestionView
    @Binding var picked: Set<UInt32>
    @Binding var other: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(question.question).fixedSize(horizontal: false, vertical: true)
            ForEach(Array(question.options.enumerated()), id: \.offset) { index, option in
                let number = UInt32(index)
                Button {
                    if question.multiSelect {
                        if picked.contains(number) { picked.remove(number) } else { picked.insert(number) }
                    } else {
                        picked = [number]
                    }
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Icon(mark(number), size: 14)
                            .foregroundStyle(picked.contains(number) ? Palette.accent : .secondary)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(option.label)
                            if !option.description.isEmpty {
                                Text(option.description).font(.system(size: 12)).foregroundStyle(.secondary)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            if question.allowOther {
                TextField("Something else", text: $other).textFieldStyle(.roundedBorder)
            }
        }
        .font(.system(size: 13))
    }

    private func mark(_ number: UInt32) -> String {
        let on = picked.contains(number)
        if question.multiSelect { return on ? "square-check" : "square" }
        return on ? "circle-dot" : "circle"
    }
}

/// A command or arguments in mono, a few lines at most.
private struct Mono: View {
    let text: String
    var lines = 6

    var body: some View {
        Text(text)
            .font(.system(size: 12, design: .monospaced))
            .lineLimit(lines)
            .textSelection(.enabled)
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.well))
    }
}

/// An edit's lines, added and removed in their colours.
private struct Patch: View {
    let lines: [EditLine]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    switch line {
                    case .file(let file):
                        Text(file.path).font(.system(size: 11, weight: .semibold)).padding(.vertical, 4)
                    case .hunk:
                        Rectangle().fill(Palette.line).frame(height: 1).padding(.vertical, 4)
                    case .line(let patch):
                        PatchText(kind: patch.kind, text: patch.text)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 220)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.well))
    }
}

/// One line of a diff: its sign, its text, and its colour behind it.
struct PatchText: View {
    let kind: LineKind
    let text: String

    var body: some View {
        HStack(spacing: 0) {
            Text(sign).frame(width: 16).foregroundStyle(signTint)
            Text(text).lineLimit(1)
            Spacer(minLength: 0)
        }
        .font(.system(size: 11.5, design: .monospaced))
        .padding(.vertical, 1)
        .background(tint)
    }

    private var sign: String {
        switch kind {
        case .added: "+"
        case .removed: "−"
        case .context: " "
        }
    }

    private var tint: Color {
        switch kind {
        case .added: Palette.added.opacity(0.09)
        case .removed: Palette.removed.opacity(0.09)
        case .context: .clear
        }
    }

    private var signTint: Color {
        switch kind {
        case .added: Palette.added
        case .removed: Palette.removed
        case .context: .secondary
        }
    }
}

/// Buttons laid out in a row, wrapping onto the next line when they do not
/// fit.
private struct FlowButtons<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        FlowLayout(spacing: 8) { content }
            .controlSize(.regular)
    }
}

struct FlowLayout: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var line: CGFloat = 0
        var widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                y += line + spacing
                x = 0
                line = 0
            }
            x += size.width + spacing
            line = max(line, size.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: min(widest, width), height: y + line)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var line: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += line + spacing
                x = bounds.minX
                line = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            line = max(line, size.height)
        }
    }
}
