import AmuxValues
import AmuxWords
import SwiftUI

/// What the agent waits on, in the composer's place: what it wants, in
/// full, and the answers it offers.
struct AskCardView: View {
    let card: AskCard
    let chat: any ChatSurface

    @State private var noting: Int?
    @State private var note = ""
    @State private var picks: [Set<UInt32>] = []
    @State private var others: [String] = []
    /// Replying in the person's own words instead of answering, and what
    /// they wrote: with the composer away while the ask is open, this is
    /// where they say something else.
    @State private var replying = false
    @State private var reply = ""

    /// It asks a question rather than for leave.
    private var asks: Bool {
        switch card.body {
        case .question, .form: true
        default: false
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Icon(asks ? "message-circle-question" : "shield", size: 14)
                    .foregroundStyle(asks ? Palette.question : Palette.you)
                Text(ChatWords.headline(card)).foregroundStyle(Palette.textSecondary)
                Spacer()
                if let position = ChatWords.position(card) {
                    Text(position).foregroundStyle(Palette.textTertiary)
                }
            }
            .font(.ui(.meta, weight: .medium))
            content
            state
            if case .question(let questions) = card.body {
                questionButtons(questions)
            } else {
                choices
            }
        }
        .font(.ui(.prose))
        .padding(16)
        .background(RoundedRectangle(cornerRadius: Radius.panel, style: .continuous).fill(Palette.card))
        // The hand says it waits on the person; the card itself stays calm.
        .overlay(RoundedRectangle(cornerRadius: Radius.panel, style: .continuous).strokeBorder(Palette.edge))
        .elevation(.floating)
        .onChange(of: card.key, initial: true) { _, _ in reset() }
    }

    private func reset() {
        noting = nil
        note = ""
        replying = false
        reply = ""
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
                Text(description.isEmpty ? reason : description)
                    .lineSpacing(4)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Mono(text: command, folder: cwd.isEmpty ? nil : PlaceNames.path(cwd))
        case .edit(let path, _, let added, let removed, let lines, let reason, _, _):
            if !reason.isEmpty { Text(reason).foregroundStyle(Palette.textSecondary) }
            HStack(spacing: 6) {
                Text(path).font(.mono(.meta)).lineLimit(1).truncationMode(.head)
                Text("+\(added)").foregroundStyle(Palette.added)
                Text("−\(removed)").foregroundStyle(Palette.removed)
            }
            .font(.ui(.meta))
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
                    if let title { Text(title).font(.ui(.title, weight: .semibold)) }
                    ProseView(source: body, streaming: false, quiet: false)
                }
                .padding(.trailing, 8)
            }
            .frame(maxHeight: 280)
        case .form(_, let message, let fields):
            Text(message).foregroundStyle(Palette.textSecondary)
            Text(fields.map(\.title).joined(separator: ", ")).font(.ui(.meta)).foregroundStyle(Palette.textTertiary)
        case .link(_, let message, let url, _):
            Text(message).foregroundStyle(Palette.textSecondary)
            if let link = URL(string: url) { Link(url, destination: link).font(.ui(.meta)) }
        case .access(let reason, let read, let write, let network, let hosts):
            if !reason.isEmpty { Text(reason).foregroundStyle(Palette.textSecondary) }
            VStack(alignment: .leading, spacing: 2) {
                if !read.isEmpty { Text("Read \(read.joined(separator: ", "))") }
                if !write.isEmpty { Text("Write \(write.joined(separator: ", "))") }
                if network { Text(hosts.isEmpty ? "Network" : "Network to \(hosts.joined(separator: ", "))") }
            }
            .font(.mono(.meta))
        case .unanswerable(let reason):
            Text(reason).foregroundStyle(Palette.textSecondary)
            Text("Answer this in the agent’s terminal, or stop the agent.")
                .font(.ui(.meta))
                .foregroundStyle(Palette.textTertiary)
        }
    }

    @ViewBuilder private var state: some View {
        switch card.state {
        case .sending:
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text("Sending…").foregroundStyle(Palette.textSecondary)
            }
            .font(.ui(.meta))
        case .rejected(let refusal):
            Text(refusal.detail ?? "Your answer wasn’t accepted").font(.ui(.meta)).foregroundStyle(Palette.failure)
        case .notConfirmed:
            Text("Your answer may not have arrived").font(.ui(.meta)).foregroundStyle(Palette.caution)
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
            // One row at the right, a refusal first and on Escape, the
            // expected answer last and on Return, as a Mac dialog orders
            // its buttons. The other answers ride on the expected one's
            // chevron, so the row never wraps.
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                ForEach(refusals, id: \.self) { index in
                    choiceButton(index, hint: "Esc")
                }
                if let primary {
                    HStack(spacing: 1) {
                        choiceButton(primary, hint: "⏎", split: !otherChoices.isEmpty)
                        if !otherChoices.isEmpty {
                            Menu {
                                ForEach(otherChoices, id: \.self) { index in
                                    Button(ChatWords.button(card.choices[index])) { pick(index) }
                                }
                            } label: {
                                Icon("chevron-down", size: 12)
                                    .foregroundStyle(.white)
                                    .frame(width: 26, height: 28)
                                    .background(
                                        UnevenRoundedRectangle(
                                            bottomTrailingRadius: Radius.control, topTrailingRadius: Radius.control,
                                            style: .continuous
                                        ).fill(Palette.accent))
                                    .contentShape(Rectangle())
                            }
                            .menuStyle(.button)
                            .buttonStyle(.plain)
                            .menuIndicator(.hidden)
                            .fixedSize()
                            .help("More options")
                            .disabled(card.state == .sending)
                        }
                    }
                } else {
                    ForEach(otherChoices, id: \.self) { index in
                        choiceButton(index, hint: nil)
                    }
                }
            }
        }
    }

    private var refusals: [Int] { card.choices.indices.filter { Self.refuses(card.choices[$0]) } }

    /// The expected answer, when the card has one the person is led to.
    private var primary: Int? {
        guard !card.defaultToNo else { return nil }
        return card.choices.indices.first { card.choices[$0].primary && !Self.refuses(card.choices[$0]) }
    }

    /// The answers that are neither a refusal nor the expected one.
    private var otherChoices: [Int] {
        card.choices.indices.filter { !Self.refuses(card.choices[$0]) && $0 != primary }
    }

    private func pick(_ index: Int) {
        if card.choices[index].takesNote { noting = index } else { chat.answer(choice: index, note: nil) }
    }

    /// A choice as a pill with the key that answers it.
    private func choiceButton(_ index: Int, hint: String?, split: Bool = false) -> some View {
        let choice = card.choices[index]
        let prominent = index == primary
        return Button {
            pick(index)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(ChatWords.button(choice))
                if let hint {
                    Text(hint).font(.ui(.caption)).opacity(0.6)
                }
            }
        }
        .buttonStyle(PillButtonStyle(prominent: prominent, joined: split))
        .disabled(card.state == .sending)
        .modifier(ChoiceKey(prominent: prominent, refuses: Self.refuses(choice)))
    }

    /// The choices in a dialog's order: refusals first, the expected one
    /// last, the rest as the provider gave them.
    private var ordered: [Int] {
        let indices = Array(card.choices.indices)
        func rank(_ index: Int) -> Int {
            let choice = card.choices[index]
            if Self.refuses(choice) { return 0 }
            if choice.primary && !card.defaultToNo { return 2 }
            return 1
        }
        return indices.sorted { rank($0) == rank($1) ? $0 < $1 : rank($0) < rank($1) }
    }

    static func refuses(_ choice: Choice) -> Bool {
        switch choice.outcome {
        case .deny, .denyAndStop, .decline: return true
        case .offered(_, let refuses): return refuses
        default: return false
        }
    }

    private func send(_ index: Int) {
        chat.answer(choice: index, note: note.isEmpty ? nil : note)
        noting = nil
        note = ""
    }

    private func responses(_ questions: [QuestionView]) -> [QuestionResponse] {
        questions.indices.map { index in
            let other = index < others.count && !others[index].isEmpty ? others[index] : nil
            let picked = index < picks.count ? picks[index].sorted() : []
            return QuestionResponse(note: nil, other: other, picked: picked.isEmpty ? nil : picked)
        }
    }

    @ViewBuilder private func questionButtons(_ questions: [QuestionView]) -> some View {
        if replying {
            HStack(spacing: 8) {
                TextField("Reply in your own words", text: $reply)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { sendReply(questions) }
                Button("Cancel") { replying = false }
                    .buttonStyle(PillButtonStyle())
                Button("Reply") { sendReply(questions) }
                    .buttonStyle(PillButtonStyle(prominent: true))
                    .keyboardShortcut(.defaultAction)
                    .disabled(reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        } else {
            answerButtons(questions)
        }
    }

    private func sendReply(_ questions: [QuestionView]) {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        chat.replyInstead(text, soFar: responses(questions))
        replying = false
        reply = ""
    }

    private func answerButtons(_ questions: [QuestionView]) -> some View {
        HStack(spacing: 8) {
            if card.questionReply {
                Button("Reply instead") { replying = true }
                    .buttonStyle(PillButtonStyle())
            }
            Spacer()
            if card.questionSkip {
                Button("Skip") {
                    chat.answer(responses: questions.map { _ in QuestionResponse(note: nil, other: nil, picked: nil) })
                }
                .buttonStyle(PillButtonStyle())
            }
            Button("Answer") {
                chat.answer(responses: responses(questions))
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
                            .foregroundStyle(picked.contains(number) ? Palette.accent : Palette.textSecondary)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(option.label)
                            if !option.description.isEmpty {
                                Text(option.description).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
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
        .font(.ui(.body))
    }

    private func mark(_ number: UInt32) -> String {
        let on = picked.contains(number)
        if question.multiSelect { return on ? "square-check" : "square" }
        return on ? "circle-dot" : "circle"
    }
}

/// A command or arguments in mono, a few lines at most.
/// Code set into the card; a command with the folder it runs in before
/// it, as a shell prompt shows it.
private struct Mono: View {
    let text: String
    var folder: String?
    var lines = 6
    /// Some of the command lies past the right edge.
    @State private var more = false

    var body: some View {
        // Scrolled sideways rather than wrapped: a command broken inside a
        // flag misreads. A fade at the edge says there is more of it, so a
        // cut-off flag is not read as the whole command.
        ScrollView(.horizontal, showsIndicators: false) {
            prompt
                .font(.mono(.callout))
                .lineLimit(lines)
                .lineSpacing(3)
                .textSelection(.enabled)
                .fixedSize()
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
        }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.contentOffset.x + geometry.containerSize.width < geometry.contentSize.width - 1
            } action: { _, now in
                more = now
            }
            .overlay(alignment: .trailing) {
                if more {
                    LinearGradient(colors: [Palette.inset.opacity(0), Palette.inset], startPoint: .leading, endPoint: .trailing)
                        .frame(width: 32)
                        .padding(1)
                        .allowsHitTesting(false)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
            .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
    }

    private var prompt: Text {
        guard let folder else { return Text(text) }
        return Text("\(folder) ❯ ").foregroundStyle(Palette.textTertiary) + Text(text)
    }
}

/// The key a choice answers to: Return for the expected one, Escape for a
/// refusal.
private struct ChoiceKey: ViewModifier {
    let prominent: Bool
    let refuses: Bool

    func body(content: Content) -> some View {
        if prominent {
            content.keyboardShortcut(.defaultAction)
        } else if refuses {
            content.keyboardShortcut(.cancelAction)
        } else {
            content
        }
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
                        Text(file.path).font(.ui(.caption, weight: .semibold)).padding(.vertical, 4)
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
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
    }
}

/// One line of a diff: its sign, its text, and its colour behind it.
struct PatchText: View {
    let kind: LineKind
    let text: String

    var body: some View {
        // Wrapped, never cut: a review that hides the end of a line hides
        // what changed in it. The indent is a column of its own, so a
        // wrapped line hangs under its code and keeps its shape.
        let (indent, code) = Self.split(text)
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(sign).frame(width: 16).foregroundStyle(signTint)
            if !indent.isEmpty { Text(indent).fixedSize() }
            Text(code).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.mono(.small))
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

    /// The line's indent, as no-break spaces with tabs four wide, and the
    /// code after it.
    static func split(_ text: String) -> (indent: String, code: String) {
        let indent = text.prefix { $0 == " " || $0 == "\t" }
        let width = indent.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
        return (String(repeating: "\u{00A0}", count: width), String(text.dropFirst(indent.count)))
    }

    var tint: Color {
        switch kind {
        case .added: Palette.added.opacity(0.1)
        case .removed: Palette.removed.opacity(0.1)
        case .context: .clear
        }
    }

    private var signTint: Color {
        switch kind {
        case .added: Palette.added
        case .removed: Palette.removed
        case .context: Palette.textSecondary
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
