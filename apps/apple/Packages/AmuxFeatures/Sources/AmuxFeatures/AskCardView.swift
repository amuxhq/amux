import AmuxCore
import AmuxDesign
import Foundation
import SwiftUI

/// What the person did on an ask card.
public enum AskAction: Equatable, Sendable {
    /// The choice at this position on the card, with the note it takes.
    case choose(Int, note: String?)
    /// One response per question, in order: a pick (none skips it) and
    /// its note.
    case respond([QuestionResponse])
    /// The person's own words instead of answering, with what they had
    /// answered so far.
    case reply(String, soFar: [QuestionResponse])
    /// A form's Submit, with one value per field in the form's order.
    case submit([FormValue])
    /// The interrupt: the turn ends, the ask is dismissed, the agent stays.
    case stop
    /// The answer was not confirmed: send it again, or forget it.
    case resend
    case discard
    /// The answer is held while the host cannot be reached: take it back
    /// and answer again.
    case cancelHeld
}

/// Where a card starts, for a capture of one part-way through: a note being
/// written, a question's "Something else" open, an option highlighted, or
/// the answers under review.
public enum AskPreset: Equatable, Sendable {
    case noting(Int)
    case other(String)
    case highlighted(UInt32)
    case reviewing([QuestionResponse])
    case autoAccept
    /// The first question's note being written.
    case questionNote(String)
    /// A reply instead being written.
    case replying(String)
}

/// Where a question card keeps its progress: what it was left at, and where
/// each change goes.
public struct QuestionKeeping: Sendable {
    let kept: QuestionDraft?
    let keep: @MainActor @Sendable (QuestionDraft) -> Void

    public init(kept: QuestionDraft?, keep: @escaping @MainActor @Sendable (QuestionDraft) -> Void) {
        self.kept = kept
        self.keep = keep
    }

    /// Kept only while the card is on screen.
    public static let none = QuestionKeeping(kept: nil) { _ in }
}

/// Where a tool server's form keeps what was entered: the values it was
/// left at, one per field, and where each change goes.
public struct FormKeeping: Sendable {
    let kept: [FormValue]?
    let keep: @MainActor @Sendable ([FormValue]) -> Void

    public init(kept: [FormValue]?, keep: @escaping @MainActor @Sendable ([FormValue]) -> Void) {
        self.kept = kept
        self.keep = keep
    }

    /// Kept only while the card is on screen.
    public static let none = FormKeeping(kept: nil) { _ in }
}

/// The head ask, docked where the composer was. Every kind fills one
/// anatomy: the head (an accent mark with the kind's glyph, what it wants,
/// "1 of 3", and the ⋯ menu that always carries Stop), the subject verbatim
/// in a sunken box, why it asks, the choices it offers as rows stated as
/// outcomes, and a pair of buttons with the likely one filled.
public struct AskCardView: View {
    @Environment(\.design) private var design
    let card: AskCard
    let preset: AskPreset?
    let questions: QuestionKeeping
    let form: FormKeeping
    let act: (AskAction) -> Void

    /// `questions` and `form` hold a question card's progress and a form's
    /// values somewhere that outlives the card, so leaving the chat and
    /// coming back finds it as it was.
    public init(
        card: AskCard, preset: AskPreset? = nil, questions: QuestionKeeping = .none,
        form: FormKeeping = .none, act: @escaping (AskAction) -> Void
    ) {
        self.card = card
        self.preset = preset
        self.questions = questions
        self.form = form
        self.act = act
    }

    public var body: some View {
        Group {
            if case .sending = card.state {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.up.circle")
                        .font(.system(size: 15))
                        .foregroundStyle(design.inkMuted.color)
                    Text(String(localized: "Sending your answer · \(ChatWords.headline(card))"))
                        .designFont(.detail, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 13)
                .identified("ask.sending", label: ChatWords.headline(card))
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    head
                    stateLine
                    if showsBody { AskBodyView(card: card, preset: preset, questions: questions, form: form, act: act) }
                }
                .padding(16)
            }
        }
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .id(card.key)
        .identified("ask", label: ChatWords.headline(card), value: stateName)
    }

    private var showsBody: Bool {
        switch card.state {
        case .open, .rejected: true
        case .sending, .notConfirmed, .dismissed, .held: false
        }
    }

    private var stateName: String {
        switch card.state {
        case .open: "open"
        case .sending: "sending"
        case .rejected: "rejected"
        case .notConfirmed: "not-confirmed"
        case .held: "held"
        case .dismissed: "dismissed"
        }
    }

    private var head: some View {
        HStack(alignment: .center, spacing: 10) {
            AskMark(glyph: AskMark.glyph(card.body))
            Text(ChatWords.headline(card))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 6)
            if let position = ChatWords.position(card) {
                Text(position)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                    .fixedSize()
            }
            MenuButton(
                name: String(localized: "More"), identifier: "ask.more",
                items: [MenuItem(
                    title: String(localized: "Interrupt"), systemImage: "stop.circle",
                    destructive: true
                ) { act(.stop) }]
            ) {
                Image(systemName: "ellipsis")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 28, height: 28)
                    .overlay(Circle().strokeBorder(design.hairline.color, lineWidth: 1))
                    .thumbTarget(x: 8, y: 8)
            }
            .reclaimingThumbTarget(x: 8, y: 8)
        }
    }

    @ViewBuilder
    private var stateLine: some View {
        switch card.state {
        case .rejected(let reason):
            let why = ChatModel.refusal(reason)
            Text(String(localized: "Not sent · \(why)"))
                .designFont(.detail, design)
                .foregroundStyle(design.accent.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("ask.rejected", label: why)
        case .notConfirmed:
            VStack(alignment: .leading, spacing: 12) {
                Text("Your answer was not confirmed. The connection dropped before the agent replied.")
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                ButtonPair {
                    choiceButton(String(localized: "Resend"), kind: .primary, id: "ask.resend") {
                        act(.resend)
                    }
                    choiceButton(String(localized: "Discard"), kind: .outline, id: "ask.discard") {
                        act(.discard)
                    }
                }
            }
        case .held:
            VStack(alignment: .leading, spacing: 12) {
                Text("Your answer is held: the agent’s host can’t be reached. It sends when the host is back.")
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("ask.held", label: String(localized: "Your answer is held"))
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "ask.held.cancel") {
                    act(.cancelHeld)
                }
            }
        case .dismissed:
            Text("The agent exited with this open. Resume it to carry on.")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        case .open, .sending:
            EmptyView()
        }
    }
}

/// The accent circle an ask opens with, carrying its kind's glyph.
struct AskMark: View {
    @Environment(\.design) private var design
    let glyph: String

    var body: some View {
        Image(systemName: glyph)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(design.onAccent.color)
            .frame(width: 24, height: 24)
            .background(Circle().fill(design.accent.color))
            .accessibilityHidden(true)
    }

    static func glyph(_ body: AskBody) -> String {
        switch body {
        case .command, .edit, .tool: "hand.raised.fill"
        case .question: "questionmark"
        case .plan: "list.bullet"
        case .form: "list.bullet.rectangle"
        case .link: "link"
        case .access: "lock.fill"
        case .unanswerable: "exclamationmark"
        }
    }
}

/// A full-width button on a card.
@MainActor
func choiceButton(
    _ title: String, kind: ActionLabel.Kind, id: String, enabled: Bool = true,
    action: @escaping () -> Void
) -> some View {
    Button(action: action) { ActionLabel(title, kind: kind, fill: true) }
        .buttonStyle(.amuxControl)
        .disabled(!enabled)
        .identified(id, label: title, enabled: enabled)
}

/// A card's buttons side by side, sharing the width equally.
struct ButtonPair<Content: View>: View {
    @ViewBuilder let content: () -> Content

    var body: some View {
        HStack(spacing: 8) { content() }
    }
}

/// One offered choice as a row: a repeat mark, what happens, and under it how
/// far it reaches. A tap answers with it. The mark says the choice lasts
/// beyond this ask; a tick there read as a choice already made.
struct ChoiceRow: View {
    @Environment(\.design) private var design
    let title: String
    let detail: String?
    let id: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: "repeat")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                    if let detail {
                        Text(detail)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, minHeight: 50, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius + 2, style: .continuous)
                    .strokeBorder(design.hairline.color, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel([title, detail].compactMap { $0 }.joined(separator: ", "))
        .identified(id, label: [title, detail].compactMap { $0 }.joined(separator: " "))
    }
}

/// What an ask is about, verbatim, in a sunken mono box.
struct SubjectBox: View {
    @Environment(\.design) private var design
    let text: String
    var lines: Int? = 4
    /// An edit's added and removed line counts, at the end of its path.
    var counts: (added: UInt32, removed: UInt32)?
    /// Where a subject too long for its lines is cut. A path is cut in the
    /// middle, never at its end, so the file's name stays.
    var truncation: Text.TruncationMode = .tail

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(text)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(lines)
                .truncationMode(truncation)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            if let counts {
                Spacer(minLength: 4)
                HStack(spacing: 5) {
                    Text(verbatim: "+\(counts.added)").foregroundStyle(design.added.color)
                    Text(verbatim: "−\(counts.removed)").foregroundStyle(design.removed.color)
                }
                .designFont(.monoSmall, design)
                .fixedSize()
            }
        }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .identified("ask.subject", label: text)
    }
}

/// The subject, why it asks, the offered choices and the buttons, filled
/// per body from the one anatomy.
private struct AskBodyView: View {
    @Environment(\.design) private var design
    @Environment(\.openURL) private var openURL
    let card: AskCard
    let preset: AskPreset?
    let questions: QuestionKeeping
    let form: FormKeeping
    let act: (AskAction) -> Void
    /// The deny step is open, with the note it takes.
    @State private var noting: Bool
    @State private var note = ""
    @State private var wholeDiff = false
    @State private var autoAccept = false
    @State private var forSession = false
    @State private var opened = false
    @State private var fields: [FormEntry]?

    init(
        card: AskCard, preset: AskPreset?, questions: QuestionKeeping, form: FormKeeping,
        act: @escaping (AskAction) -> Void
    ) {
        self.card = card
        self.preset = preset
        self.questions = questions
        self.form = form
        self.act = act
        if case .noting? = preset { _noting = State(initialValue: true) } else { _noting = State(initialValue: false) }
        _autoAccept = State(initialValue: preset == .autoAccept)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            subject
            if noting {
                noteStep
            } else {
                choices
            }
        }
    }

    // MARK: The subject and why

    @ViewBuilder
    private var subject: some View {
        switch card.body {
        case .command(let command, let cwd, let reason, let description):
            SubjectBox(text: command)
            let purpose = [description.isEmpty ? reason : description,
                           cwd.isEmpty ? "" : String(localized: "in \(cwd)")]
                .filter { !$0.isEmpty }.joined(separator: " · ")
            if !purpose.isEmpty { why(purpose) }
        case .edit(let path, _, let added, let removed, let lines, let reason, _, _):
            SubjectBox(text: path, lines: 2, counts: (added, removed), truncation: .middle)
            if !lines.isEmpty { DiffPreview(lines: lines, whole: wholeDiff) }
            if lines.count > DiffPreview.lines {
                link(wholeDiff ? String(localized: "Show less") : String(localized: "Show the whole diff"),
                     id: "ask.diff", value: wholeDiff ? "open" : "folded") { wholeDiff.toggle() }
            }
            if !reason.isEmpty { why(reason) }
        case .tool(_, _, let arguments):
            if !arguments.isEmpty { SubjectBox(text: arguments, lines: 8) }
        case .question:
            EmptyView()
        case .plan:
            // The plan reads under its own heading in the chat; the card is
            // only the decision.
            EmptyView()
        case .form(_, let message, _):
            if !message.isEmpty { why(message) }
        case .link(_, let message, let url):
            if !message.isEmpty { why(message) }
            SubjectBox(text: url, lines: 2)
        case .access(let reason, let read, let write, let network, let hosts):
            if !reason.isEmpty { why(reason) }
            VStack(spacing: 0) {
                ForEach(write, id: \.self) { path in fact(String(localized: "Write to"), path) }
                ForEach(read, id: \.self) { path in fact(String(localized: "Read"), path) }
                if network {
                    fact(
                        String(localized: "Network access"),
                        hosts.isEmpty ? String(localized: "Any host") : hosts.joined(separator: ", "))
                }
            }
            .padding(.horizontal, 14)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
        case .unanswerable(let reason):
            why(reason.isEmpty
                ? String(localized: "The agent is showing a menu this build can’t read. Attach from a terminal to answer it, or interrupt the turn.")
                : reason)
        }
    }

    private func why(_ text: String) -> some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// One thing an access request asks for, on its own line.
    private func fact(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
            Spacer(minLength: 6)
            Text(value)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(2)
                .multilineTextAlignment(.trailing)
        }
        .padding(.vertical, 11)
    }

    private func link(_ title: String, id: String, value: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Text(title)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
            }
            .thumbTarget(x: 4, y: 12)
        }
        .buttonStyle(.amuxControl)
        .identified(id, label: title, value: value)
        .reclaimingThumbTarget(x: 4, y: 12)
    }

    // MARK: The choices

    @ViewBuilder
    private var choices: some View {
        switch card.body {
        case .question(let questions):
            QuestionCard(
                questions: questions, takesNote: card.questionNote, skips: card.questionSkip,
                replies: card.questionReply, preset: preset, keeping: self.questions,
                send: { act(.respond($0)) }, reply: { act(.reply($0, soFar: $1)) })
        case .plan:
            planChoices
        case .access:
            accessChoices
        case .form(_, _, let asked):
            formChoices(asked)
        case .link(_, _, let url):
            linkChoices(url)
        case .unanswerable:
            choiceButton(String(localized: "Interrupt"), kind: .outline, id: "ask.stop") {
                act(.stop)
            }
        case .command, .edit, .tool:
            permissionChoices
        }
    }

    private func isDeny(_ choice: Choice) -> Bool {
        switch choice.outcome {
        case .deny, .denyAndStop, .decline, .sendBack: true
        default: false
        }
    }

    /// The scopes the agent offered as rows, then Allow beside Deny. Deny
    /// opens its own step when there is a note to write or a choice between
    /// carrying on and stopping; a single deny without a note answers at once.
    @ViewBuilder
    private var permissionChoices: some View {
        let primary = card.choices.firstIndex { $0.primary }
        let denies = card.choices.indices.filter { isDeny(card.choices[$0]) }
        let scopes = card.choices.indices.filter { $0 != primary && !denies.contains($0) }
        VStack(spacing: 8) {
            ForEach(scopes, id: \.self) { index in
                let row = ChatWords.scopeRow(card.choices[index])
                ChoiceRow(title: row.title, detail: row.detail, id: "ask.choice.\(index)") {
                    act(.choose(index, note: nil))
                }
            }
            ButtonPair {
                if let primary {
                    choiceButton(ChatWords.button(card.choices[primary]), kind: .primary, id: "ask.choice.\(primary)") {
                        act(.choose(primary, note: nil))
                    }
                }
                if let deny = denies.first {
                    if denies.count == 1 && !card.choices[deny].takesNote {
                        choiceButton(ChatWords.button(card.choices[deny]), kind: .outline, id: "ask.choice.\(deny)") {
                            act(.choose(deny, note: nil))
                        }
                    } else {
                        choiceButton(String(localized: "Deny…"), kind: .outline, id: "ask.deny") {
                            note = ""
                            noting = true
                        }
                    }
                }
            }
        }
    }

    /// Approve, with a switch to the permission the agent offers to move
    /// to as the work starts, and Send back.
    @ViewBuilder
    private var planChoices: some View {
        let approve = card.choices.indices.filter {
            if case .approvePlan = card.choices[$0].outcome { true } else { false }
        }
        let switched = approve.first {
            if case .approvePlan(let mode, _) = card.choices[$0].outcome { !mode.isEmpty } else { false }
        }
        let plain = approve.first {
            if case .approvePlan(let mode, _) = card.choices[$0].outcome { mode.isEmpty } else { false }
        }
        VStack(alignment: .leading, spacing: 12) {
            if let switched, plain != nil {
                let title = ChatWords.planSwitch(card.choices[switched])
                Toggle(isOn: $autoAccept) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(title)
                            .designFont(.body, design)
                            .foregroundStyle(design.ink.color)
                        Text("As the work starts")
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkMuted.color)
                    }
                }
                .tint(design.ink.color)
                .oneSwitch(
                    String(localized: "\(title), As the work starts"),
                    isOn: $autoAccept)
                .identified("ask.plan.auto", value: autoAccept ? "on" : "off")
            }
            ButtonPair {
                if let index = (autoAccept ? switched : plain) ?? approve.first {
                    choiceButton(String(localized: "Approve"), kind: .primary, id: "ask.approve") {
                        act(.choose(index, note: nil))
                    }
                }
                ForEach(card.choices.indices.filter { !approve.contains($0) }, id: \.self) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: .outline,
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    /// Grant for this turn or this session, or deny.
    @ViewBuilder
    private var accessChoices: some View {
        let turn = card.choices.firstIndex { $0.outcome == .grantForTurn }
        let session = card.choices.firstIndex { $0.outcome == .grantForSession }
        VStack(alignment: .leading, spacing: 12) {
            if turn != nil, session != nil {
                Picker(String(localized: "How long"), selection: $forSession) {
                    Text("This turn").tag(false)
                    Text("This session").tag(true)
                }
                .pickerStyle(.segmented)
                .identified("ask.grant.length", value: forSession ? "session" : "turn")
            }
            ButtonPair {
                if let index = (forSession ? session : turn) ?? turn ?? session {
                    choiceButton(String(localized: "Grant"), kind: .primary, id: "ask.grant") {
                        act(.choose(index, note: nil))
                    }
                }
                ForEach(
                    card.choices.indices.filter { $0 != turn && $0 != session }, id: \.self
                ) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: .outline,
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    /// Native fields from the tool server's schema. Whether the form can go,
    /// and what is wrong with a field, is the shared view's check.
    @ViewBuilder
    private func formChoices(_ asked: [FormField]) -> some View {
        let current = fields ?? FormEntry.entries(asked, kept: form.kept)
        let values = current.map(\.value)
        let problems = Bridge.formProblems(asked, values)
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(current.enumerated()), id: \.offset) { index, field in
                FormFieldView(entry: field, problem: FormEntry.shown(problems, at: index, of: field)) { value in
                    // From the form as it is now: a control can hold on to
                    // this closure from an earlier drawing (a menu's
                    // choices), and the copy drawn then lacks later answers.
                    var edited = fields ?? FormEntry.entries(asked, kept: form.kept)
                    edited[index].value = value
                    fields = edited
                    form.keep(edited.map(\.value))
                }
            }
            ButtonPair {
                ForEach(Array(card.choices.enumerated()), id: \.offset) { index, choice in
                    if choice.outcome == .submit {
                        choiceButton(
                            String(localized: "Submit"), kind: .primary, id: "ask.submit",
                            enabled: problems?.isEmpty == true
                        ) { act(.submit(values)) }
                    } else {
                        choiceButton(
                            ChatWords.choice(choice), kind: .outline, id: "ask.choice.\(index)"
                        ) { pick(index) }
                    }
                }
            }
        }
    }

    /// Open the link, then say it is done.
    @ViewBuilder
    private func linkChoices(_ url: String) -> some View {
        let done = card.choices.indices.filter { card.choices[$0].outcome == .openLink }
        let rest = card.choices.indices.filter { !done.contains($0) }
        VStack(spacing: 8) {
            if let link = URL(string: url), !opened {
                choiceButton(String(localized: "Open link"), kind: .primary, id: "ask.open") {
                    opened = true
                    openURL(link)
                }
            }
            ButtonPair {
                ForEach(done, id: \.self) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: opened ? .primary : .outline,
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
                ForEach(rest, id: \.self) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: .outline, id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    private func pick(_ index: Int) {
        if card.choices[index].takesNote {
            note = ""
            noting = true
        } else {
            act(.choose(index, note: nil))
        }
    }

    /// The deny step, or the note a plan's send-back needs: the note field
    /// when a choice takes one, then the choices that end it side by side,
    /// carrying on filled.
    private var noteStep: some View {
        let ends = card.choices.indices.filter { isDeny(card.choices[$0]) }
        let takesNote = ends.contains { card.choices[$0].takesNote }
        let required = ends.contains { card.choices[$0].outcome == .sendBack }
        return VStack(alignment: .leading, spacing: 10) {
            if takesNote {
                HStack(alignment: .firstTextBaseline) {
                    Text(required ? String(localized: "What should change") : String(localized: "Tell it why (optional)"))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                    Spacer(minLength: 6)
                    backButton
                }
                TextField("", text: $note, axis: .vertical)
                    .lineLimit(2...6)
                    .designFont(.body, design)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.raised.color)
                            .strokeBorder(design.ink.color, lineWidth: 1)
                    }
                    .accessibilityLabel(required ? String(localized: "What should change") : String(localized: "Tell it why (optional)"))
                    .identified("ask.note", value: note)
            } else {
                HStack {
                    Spacer()
                    backButton
                }
            }
            ButtonPair {
                // Stopping first, carrying on filled beside it.
                ForEach(ends.sorted { stops(card.choices[$0]) && !stops(card.choices[$1]) }, id: \.self) { index in
                    let choice = card.choices[index]
                    let blank = note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    choiceButton(
                        ChatWords.choice(Choice(outcome: choice.outcome, primary: choice.primary, takesNote: false)),
                        kind: stops(choice) && ends.count > 1 ? .outline : .primary,
                        id: "ask.choice.\(index)",
                        enabled: !(choice.outcome == .sendBack && blank)
                    ) {
                        act(.choose(index, note: choice.takesNote && !blank ? note : nil))
                    }
                }
            }
        }
    }

    private func stops(_ choice: Choice) -> Bool {
        switch choice.outcome {
        case .denyAndStop, .deny(stops: true): true
        default: false
        }
    }

    private var backButton: some View {
        Button {
            noting = false
        } label: {
            Text("Back")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .thumbTarget(x: 6, y: 12)
        }
        .buttonStyle(.amuxControl)
        .identified("ask.note.back", label: String(localized: "Back"))
        .reclaimingThumbTarget(x: 6, y: 12)
    }
}

/// A short diff inline, green and red, with the whole of it one tap away.
/// Where each hunk starts reads as its first line in the new file.
struct DiffPreview: View {
    @Environment(\.design) private var design
    static let lines = 12
    let lines: [EditLine]
    let whole: Bool

    var body: some View {
        let shown = whole ? Array(lines.prefix(400)) : Array(lines.prefix(Self.lines))
        ScrollView(.horizontal) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(shown.enumerated()), id: \.offset) { _, line in
                    Text(text(line))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(ink(line))
                        .padding(.horizontal, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(wash(line))
                }
            }
            .padding(.vertical, 6)
        }
        .scrollIndicators(.hidden)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .clipShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous))
    }

    private func text(_ line: EditLine) -> String {
        switch line {
        case .file(let file): ChatWords.fileHeader(file)
        case .hunk(_, let newStart): String(localized: "Line \(newStart)")
        case .line(let line):
            switch line.kind {
            case .added: "+" + line.text
            case .removed: "-" + line.text
            case .context: " " + line.text
            }
        }
    }

    private func ink(_ line: EditLine) -> Color {
        if case .hunk = line { return design.inkFaint.color }
        if case .file = line { return design.inkMuted.color }
        return design.ink.color
    }

    private func wash(_ line: EditLine) -> Color {
        guard case .line(let line) = line else { return .clear }
        switch line.kind {
        case .added: return design.added.color.opacity(0.18)
        case .removed: return design.removed.color.opacity(0.18)
        case .context: return .clear
        }
    }
}

/// One field of a tool server's form, as the shared view reads it from the
/// schema, with what it holds now.
struct FormEntry: Equatable {
    let field: FormField
    var value: FormValue

    var name: String { field.name }
    var title: String { field.title }
    var required: Bool { field.required }
    var kind: FormFieldKind { field.kind }

    /// The form's fields, each holding what was kept from an earlier drawing,
    /// else what the shared view starts it at.
    static func entries(_ fields: [FormField], kept: [FormValue]? = nil) -> [FormEntry] {
        // Kept values belong to this form only while there is one per field.
        let kept = kept?.count == fields.count ? kept : nil
        return fields.enumerated().map { index, field in
            FormEntry(field: field, value: kept?[index] ?? field.initial)
        }
    }

    /// The problem to show under a field: one the person can act on in the
    /// field itself, once it was changed. A required field left empty is
    /// marked on its label and holds Submit back instead.
    static func shown(_ problems: [FormProblem]?, at index: Int, of entry: FormEntry) -> FieldProblem? {
        guard entry.value != entry.field.initial,
              let problem = problems?.first(where: { Int($0.field) == index })?.problem,
              problem != .required
        else { return nil }
        return problem
    }

    /// Typed text, for a text or number field.
    var typed: String {
        if case .text(let text) = value { return text }
        return ""
    }

    var on: Bool {
        if case .toggle(let on) = value { return on }
        return false
    }

    var chosen: UInt32? {
        if case .choice(let at) = value { return at }
        return nil
    }

    var picked: [UInt32] {
        if case .many(let picked) = value { return picked }
        return []
    }

    /// What the field holds, as the driver reads it: the text, "true" or
    /// "false", the chosen option, or the picked ones a line each.
    var reading: String {
        switch kind {
        case .text, .number: return typed
        case .toggle: return on ? "true" : "false"
        case .choice(let options): return chosen.flatMap { options.indices.contains(Int($0)) ? options[Int($0)] : nil } ?? ""
        case .many(let options, _, _):
            return picked.filter { options.indices.contains(Int($0)) }.map { options[Int($0)] }.joined(separator: "\n")
        }
    }
}

private struct FormFieldView: View {
    @Environment(\.design) private var design
    let entry: FormEntry
    let problem: FieldProblem?
    let set: (FormValue) -> Void

    private var on: Binding<Bool> {
        Binding(get: { entry.on }, set: { set(.toggle($0)) })
    }

    /// One option of a field that takes several, on or off.
    private func picks(_ option: UInt32) -> Binding<Bool> {
        Binding(
            get: { entry.picked.contains(option) },
            set: { on in
                let rest = entry.picked.filter { $0 != option }
                set(.many((on ? rest + [option] : rest).sorted()))
            })
    }

    private var label: String { entry.required ? "\(entry.title) *" : entry.title }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            control
            if let problem {
                Text(ChatWords.fieldProblem(problem))
                    .designFont(.detail, design)
                    .foregroundStyle(design.accent.color)
                    .identified("ask.field.\(entry.name).problem")
            }
        }
    }

    @ViewBuilder
    private var control: some View {
        switch entry.kind {
        case .toggle:
            Toggle(isOn: on) {
                Text(entry.title).designFont(.body, design)
            }
            .tint(design.ink.color)
            .oneSwitch(entry.title, isOn: on)
            .identified("ask.field.\(entry.name)", value: entry.reading)
        case .choice(let options):
            HStack {
                Text(label).designFont(.body, design)
                Spacer()
                // Nothing is chosen until the person chooses, unless the
                // server named a default.
                Picker(entry.title, selection: Binding(get: { entry.chosen }, set: { set(.choice($0)) })) {
                    Text(String(localized: "Choose")).tag(UInt32?.none)
                    ForEach(options.indices, id: \.self) { Text(options[$0]).tag(UInt32?(UInt32($0))) }
                }
                .pickerStyle(.menu)
                .tint(design.ink.color)
                .identified("ask.field.\(entry.name)", value: entry.reading)
            }
        case .many(let options, _, _):
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                ForEach(options.indices, id: \.self) { index in
                    let picked = picks(UInt32(index))
                    Toggle(isOn: picked) {
                        Text(options[index]).designFont(.body, design)
                    }
                    .tint(design.ink.color)
                    .oneSwitch(options[index], isOn: picked)
                }
            }
            .identified("ask.field.\(entry.name)", value: entry.reading)
        case .text, .number:
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                TextField(entry.title, text: Binding(get: { entry.typed }, set: { set(.text($0)) }))
                    // Digits with a sign and a point whatever the region: the
                    // shared check reads a number as "-2.5".
                    .keyboardType(entry.kind.isNumber ? .numbersAndPunctuation : .default)
                    .designFont(.body, design)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.raised.color)
                            .strokeBorder(design.hairline.color, lineWidth: 1)
                    }
                    .identified("ask.field.\(entry.name)", value: entry.reading)
            }
        }
    }
}

private extension FormFieldKind {
    var isNumber: Bool {
        if case .number = self { return true }
        return false
    }
}

/// Questions: one at a time with the headers as steps, a review of every
/// answer before sending, previews, "Something else…" and secret answers.
/// What else a card offers comes from the card: a note on each question,
/// skipping one, and replying in the person's own words instead. One
/// pick-one question without previews answers on the tap, unless its note
/// is open.
struct QuestionCard: View {
    @Environment(\.design) private var design
    let questions: [QuestionView]
    let takesNote: Bool
    let skips: Bool
    let replies: Bool
    let keeping: QuestionKeeping
    let send: ([QuestionResponse]) -> Void
    let reply: (String, [QuestionResponse]) -> Void
    @State private var step = 0
    @State private var picks: [Set<UInt32>]
    @State private var others: [String?]
    @State private var highlighted: [UInt32?]
    @State private var skipped: [Bool]
    @State private var notes: [String]
    @State private var noting: [Bool]
    @State private var reviewing = false
    @State private var replying = false
    @State private var replyText = ""

    init(
        questions: [QuestionView], takesNote: Bool, skips: Bool = false, replies: Bool = false,
        preset: AskPreset? = nil, keeping: QuestionKeeping = .none,
        send: @escaping ([QuestionResponse]) -> Void,
        reply: @escaping (String, [QuestionResponse]) -> Void = { _, _ in }
    ) {
        self.questions = questions
        self.takesNote = takesNote
        self.skips = skips
        self.replies = replies
        self.keeping = keeping
        self.send = send
        self.reply = reply
        let count = questions.count
        if let kept = keeping.kept, kept.fits(count) {
            _step = State(initialValue: kept.step)
            _picks = State(initialValue: kept.picks)
            _others = State(initialValue: kept.others)
            _highlighted = State(initialValue: kept.highlighted)
            _skipped = State(initialValue: kept.skipped)
            _notes = State(initialValue: kept.notes)
            _noting = State(initialValue: kept.noting)
            _reviewing = State(initialValue: kept.reviewing)
            _replying = State(initialValue: kept.replying)
            _replyText = State(initialValue: kept.reply)
            return
        }
        var picks = Array(repeating: Set<UInt32>(), count: count)
        var others = Array(repeating: String?.none, count: count)
        var highlighted = Array(repeating: UInt32?.none, count: count)
        var skipped = Array(repeating: false, count: count)
        var notes = Array(repeating: "", count: count)
        var noting = Array(repeating: false, count: count)
        switch preset {
        case .other(let text)?: if !others.isEmpty { others[0] = text }
        case .highlighted(let position)?:
            if !picks.isEmpty {
                picks[0] = [position]
                highlighted[0] = position
            }
        case .reviewing(let answers)?:
            for (index, answer) in answers.enumerated() where index < count {
                let chosen = answer.picked ?? []
                let typed = answer.other.flatMap { $0.isEmpty ? nil : $0 }
                if chosen.isEmpty && typed == nil {
                    skipped[index] = true
                } else {
                    picks[index] = Set(chosen)
                    others[index] = typed
                }
                notes[index] = answer.note ?? ""
            }
            _reviewing = State(initialValue: true)
        case .questionNote(let text)?:
            if !notes.isEmpty {
                notes[0] = text
                noting[0] = true
            }
        case .replying(let text)?:
            _replying = State(initialValue: true)
            _replyText = State(initialValue: text)
        default: break
        }
        _picks = State(initialValue: picks)
        _others = State(initialValue: others)
        _highlighted = State(initialValue: highlighted)
        _skipped = State(initialValue: skipped)
        _notes = State(initialValue: notes)
        _noting = State(initialValue: noting)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if replying {
                replyStep
            } else {
                if questions.count > 1 { steps }
                if reviewing {
                    review
                } else if questions.indices.contains(step) {
                    question(questions[step], at: step)
                }
                if replies { replyLink }
            }
        }
        .onChange(of: progress) { _, progress in keeping.keep(progress) }
    }

    private var progress: QuestionDraft {
        QuestionDraft(
            step: step, picks: picks, others: others, highlighted: highlighted,
            skipped: skipped, notes: notes, noting: noting, reviewing: reviewing,
            replying: replying, reply: replyText)
    }

    /// The questions' headers as steps: done ones carry a check, the one
    /// on screen is filled, and the last step is the review.
    private var steps: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                    Button {
                        reviewing = false
                        step = index
                    } label: {
                        stepChip(
                            question.header.isEmpty ? String(localized: "Question \(index + 1)") : question.header,
                            current: !reviewing && index == step, done: decided(index))
                    }
                    .buttonStyle(.amuxControl)
                    .identified("ask.step.\(index)", value: stepState(index))
                }
                Button { reviewing = true } label: {
                    stepChip(String(localized: "Review"), current: reviewing, done: false)
                }
                .buttonStyle(.amuxControl)
                .identified("ask.step.review")
            }
        }
        .scrollIndicators(.hidden)
    }

    private func stepChip(_ title: String, current: Bool, done: Bool) -> some View {
        HStack(spacing: 5) {
            if done && !current {
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .bold))
            }
            Text(title)
                .font(.custom(design.faces.body, size: 13, relativeTo: .footnote).weight(.medium))
                .lineLimit(1)
        }
        .foregroundStyle(current ? design.ground.color : (done ? design.ink.color : design.inkMuted.color))
        .padding(.horizontal, 11)
        .frame(height: 30)
        .background {
            if current {
                Capsule().fill(design.ink.color)
            } else {
                Capsule().strokeBorder(design.hairline.color, lineWidth: 1)
            }
        }
        .frame(minHeight: 44)
        .contentShape(Rectangle())
    }

    private func answered(_ index: Int) -> Bool {
        guard picks.indices.contains(index) else { return false }
        return !picks[index].isEmpty || !(others[index] ?? "").isEmpty
    }

    /// Answered, or left unanswered on purpose.
    private func decided(_ index: Int) -> Bool {
        answered(index) || (skipped.indices.contains(index) && skipped[index])
    }

    private func stepState(_ index: Int) -> String {
        if answered(index) { return "answered" }
        return decided(index) ? "skipped" : "open"
    }

    private var hasPreviews: Bool { questions.contains { $0.options.contains { !$0.preview.isEmpty } } }

    /// Whose preview the question shows: the option last touched, else the
    /// first that has one. A phone has no hover, so waiting for a touch would
    /// hide every preview until something was already picked.
    private func previewed(_ question: QuestionView, at index: Int) -> Int? {
        let highlight = highlighted.indices.contains(index) ? highlighted[index].map(Int.init) : nil
        if let highlight, question.options.indices.contains(highlight),
           !question.options[highlight].preview.isEmpty {
            return highlight
        }
        return question.options.firstIndex { !$0.preview.isEmpty }
    }

    /// One tap answers only one pick-one question with no previews, while
    /// its note is not being written.
    private var tapAnswers: Bool {
        questions.count == 1 && !questions[0].multiSelect && !hasPreviews && noting.first != true
    }

    @ViewBuilder
    private func question(_ question: QuestionView, at index: Int) -> some View {
        Text(question.question)
            .designFont(.body, design)
            .foregroundStyle(design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
            .identified("ask.question", label: question.question)
        if let shown = previewed(question, at: index) {
            let option = question.options[shown]
            VStack(alignment: .leading, spacing: 4) {
                // Named, because before a pick it is the first option's,
                // which nothing else on the card says.
                Text(option.label)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                Text(option.preview)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.ink.color)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background {
                        RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.sunken.color)
                    }
            }
            .identified("ask.preview", label: option.label)
        }
        VStack(spacing: 6) {
            ForEach(Array(question.options.enumerated()), id: \.offset) { position, option in
                optionButton(question, option, UInt32(position), at: index)
            }
            if question.allowOther { other(question, at: index) }
        }
        if takesNote { noteField(at: index) }
        let typing = others.indices.contains(index) && others[index] != nil
        if !tapAnswers || typing || skips {
            let count = picks.indices.contains(index) ? picks[index].count : 0
            let last = questions.count == 1
            ButtonPair {
                if skips {
                    choiceButton(String(localized: "Skip"), kind: .outline, id: "ask.skip") {
                        skip(index)
                    }
                }
                if !tapAnswers || typing {
                    choiceButton(
                        question.multiSelect && count > 0
                            ? (last ? String(localized: "Send · \(count) selected") : String(localized: "Next · \(count) selected"))
                            : (last ? String(localized: "Send") : String(localized: "Next")),
                        kind: .primary, id: "ask.next", enabled: answered(index)
                    ) { advance(from: index) }
                }
            }
        }
    }

    /// A note on this question, for the agent to read with its answer.
    @ViewBuilder
    private func noteField(at index: Int) -> some View {
        if noting.indices.contains(index) && noting[index] {
            TextField(
                String(localized: "A note on this answer"),
                text: Binding(get: { notes[index] }, set: { notes[index] = $0 }), axis: .vertical
            )
            .lineLimit(1...4)
            .designFont(.body, design)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.raised.color)
                    .strokeBorder(design.hairline.color, lineWidth: 1)
            }
            .identified("ask.note.\(index)", value: notes[index])
        } else if noting.indices.contains(index) {
            textLink(String(localized: "Add a note"), id: "ask.addNote.\(index)") { noting[index] = true }
        }
    }

    /// Leaves the question unanswered and moves on.
    private func skip(_ index: Int) {
        guard skipped.indices.contains(index) else { return }
        picks[index] = []
        others[index] = nil
        skipped[index] = true
        if questions.count == 1 {
            send(responses)
        } else if index + 1 < questions.count {
            step = index + 1
        } else {
            reviewing = true
        }
    }

    private func optionButton(
        _ question: QuestionView, _ option: OptionView, _ position: UInt32, at index: Int
    ) -> some View {
        let selected = picks.indices.contains(index) && picks[index].contains(position)
        return Button {
            select(position, in: question, at: index)
        } label: {
            OptionRow(
                label: option.label, detail: option.description != option.label ? option.description : "",
                recommended: option.recommended, multi: question.multiSelect, selected: selected)
        }
        .buttonStyle(.amuxControl)
        .identified("ask.option.\(position)", label: option.label, value: selected ? "selected" : nil)
    }

    private func select(_ position: UInt32, in question: QuestionView, at index: Int) {
        guard picks.indices.contains(index) else { return }
        skipped[index] = false
        highlighted[index] = position
        if question.multiSelect {
            if picks[index].contains(position) { picks[index].remove(position) } else {
                picks[index].insert(position)
            }
            return
        }
        others[index] = nil
        picks[index] = [position]
        if tapAnswers {
            send(responses)
        } else if !hasPreviews && !(noting.indices.contains(index) && noting[index]) {
            advance(from: index)
        }
    }

    @ViewBuilder
    private func other(_ question: QuestionView, at index: Int) -> some View {
        let open = others.indices.contains(index) && others[index] != nil
        if open {
            let binding = Binding(
                get: { others.indices.contains(index) ? others[index] ?? "" : "" },
                set: {
                    // A multi-select keeps its ticks beside the typed words;
                    // a single choice is one or the other.
                    others[index] = $0
                    if !question.multiSelect { picks[index] = [] }
                    skipped[index] = false
                })
            Group {
                if question.secret {
                    SecureField(String(localized: "Something else"), text: binding)
                } else {
                    TextField(String(localized: "Something else"), text: binding)
                }
            }
            .designFont(.body, design)
            .padding(.horizontal, 12)
            .padding(.vertical, 11)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.raised.color)
                    .strokeBorder(design.ink.color, lineWidth: 1)
            }
            .identified("ask.other", value: question.secret ? nil : others[index])
        } else {
            Button {
                others[index] = ""
                if !question.multiSelect { picks[index] = [] }
            } label: {
                OptionRow(
                    label: String(localized: "Something else…"), detail: "", recommended: false,
                    multi: question.multiSelect, selected: false, quiet: true)
            }
            .buttonStyle(.amuxControl)
            .identified("ask.option.other")
        }
    }

    private func advance(from index: Int) {
        guard answered(index) else { return }
        if questions.count == 1 {
            send(responses)
        } else if index + 1 < questions.count {
            step = index + 1
        } else {
            reviewing = true
        }
    }

    /// One response per question, in order: the options ticked and the
    /// words typed (a multi-select carries both; a single choice one or the
    /// other), nothing for one not answered, and its note.
    private var responses: [QuestionResponse] {
        questions.indices.map { index in
            let note = notes[index].trimmingCharacters(in: .whitespacesAndNewlines)
            let other = others[index].flatMap { $0.isEmpty ? nil : $0 }
            let picked = other != nil && !questions[index].multiSelect ? [] : picks[index].sorted()
            return QuestionResponse(note: note.isEmpty ? nil : note, other: other, picked: picked)
        }
    }

    /// Every answer on one screen; tap one to change it. A question neither
    /// answered nor skipped is marked and blocks Send.
    @ViewBuilder
    private var review: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                Button {
                    reviewing = false
                    step = index
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(question.header.isEmpty ? question.question : "\(question.header) · \(question.question)")
                            .designFont(.detail, design)
                            .foregroundStyle(design.inkMuted.color)
                            .lineLimit(2)
                        Text(summary(question, at: index))
                            .designFont(.body, design)
                            .foregroundStyle(decided(index) ? design.ink.color : design.accent.color)
                        let note = notes[index].trimmingCharacters(in: .whitespacesAndNewlines)
                        if !note.isEmpty {
                            Text(String(localized: "Note: \(note)"))
                                .designFont(.detail, design)
                                .foregroundStyle(design.inkMuted.color)
                                .lineLimit(2)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .identified("ask.review.\(index)", value: stepState(index))
            }
            choiceButton(
                String(localized: "Send answers"), kind: .primary, id: "ask.send",
                enabled: questions.indices.allSatisfy(decided)
            ) {
                send(responses)
            }
        }
    }

    private func summary(_ question: QuestionView, at index: Int) -> String {
        guard picks.indices.contains(index) else { return "" }
        var parts = picks[index].sorted().compactMap { position in
            question.options.indices.contains(Int(position)) ? question.options[Int(position)].label : nil
        }
        if let other = others[index], !other.isEmpty {
            parts.append(question.secret ? String(localized: "answered (hidden)") : "“\(other)”")
        }
        if !parts.isEmpty { return parts.joined(separator: ", ") }
        return skipped[index] ? String(localized: "Skipped") : String(localized: "Not answered")
    }

    // MARK: Replying instead

    private var replyLink: some View {
        textLink(String(localized: "Reply instead"), id: "ask.reply") { replying = true }
    }

    /// The person's own words in place of the answers; what they had
    /// answered so far goes with them.
    private var replyStep: some View {
        let blank = replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Reply in your own words")
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkMuted.color)
                Spacer(minLength: 6)
                Button { replying = false } label: {
                    Text("Back")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .thumbTarget(x: 6, y: 12)
                }
                .buttonStyle(.amuxControl)
                .identified("ask.reply.back", label: String(localized: "Back"))
                .reclaimingThumbTarget(x: 6, y: 12)
            }
            TextField("", text: $replyText, axis: .vertical)
                .lineLimit(2...6)
                .designFont(.body, design)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.raised.color)
                        .strokeBorder(design.ink.color, lineWidth: 1)
                }
                .accessibilityLabel(String(localized: "Reply in your own words"))
                .identified("ask.reply.text", value: replyText)
            choiceButton(
                String(localized: "Send reply"), kind: .primary, id: "ask.reply.send", enabled: !blank
            ) {
                reply(replyText, responses)
            }
        }
    }

    private func textLink(_ title: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Text(title)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
            }
            .frame(minHeight: 44)
        }
        .buttonStyle(.amuxControl)
        .identified(id, label: title)
    }
}

/// A question's option as a row: its box (round for one pick, square for
/// several), the label with a RECOMMENDED tag lifted out of it, and the
/// description under it.
struct OptionRow: View {
    @Environment(\.design) private var design
    let label: String
    let detail: String
    let recommended: Bool
    let multi: Bool
    let selected: Bool
    var quiet = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            box.alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(label)
                        .designFont(quiet ? .body : .bodyEmphasis, design)
                        .foregroundStyle(quiet ? design.inkMuted.color : design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                    if recommended {
                        Text("RECOMMENDED")
                            .designFont(.sectionTitle, design)
                            .foregroundStyle(design.inkMuted.color)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(design.hairline.color, lineWidth: 1))
                    }
                }
                if !detail.isEmpty {
                    Text(detail)
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius + 1, style: .continuous)
                .fill(design.sunken.color)
                .strokeBorder(selected ? design.ink.color : .clear, lineWidth: 1.5)
        }
        .contentShape(Rectangle())
    }

    private var box: some View {
        let shape = RoundedRectangle(cornerRadius: multi ? 5 : 9, style: .continuous)
        return ZStack {
            if selected {
                shape.fill(design.ink.color)
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(design.raised.color)
            } else {
                shape.strokeBorder(design.inkFaint.color, lineWidth: 1.5)
            }
        }
        .frame(width: 18, height: 18)
    }
}
