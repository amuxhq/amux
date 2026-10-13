import AmuxCore
import AmuxDesign
import Foundation
import SwiftUI

/// What the plus offers.
public enum AttachChoice: String, Equatable, Sendable {
    case photo
    case file
}

/// The composer: the activity line while the agent works, or the exited
/// agent's head; the draft's attachments; the field; and under it the plus,
/// the model chip, the microphone and one round button that sends, stops,
/// or on an exited agent resumes it with the draft. Drafting never waits;
/// sending waits for the rows to be current and the agent live.
struct ComposerBox: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    /// The side of every control's target in the row under the field.
    static let slot: CGFloat = 44
    /// The drawn plus, send, stop and resume controls, centred in their slots.
    static let round: CGFloat = 34

    @Environment(\.design) private var design
    @Bindable var model: ChatModel
    let placeholder: String
    let activity: Activity?
    let activitySubject: String?
    /// Where the exited agent ran, for its head.
    let host: String
    /// Whether the plus card stands open above the composer.
    let plusOpen: Bool
    let togglePlus: () -> Void
    /// Dictation's two acts: `.dictate` and `.dictationSettings`.
    let dictate: (ChatAction) -> Void
    /// Opens the settings card, from the model chip or the plus card.
    let openSettings: () -> Void
    var focused: FocusState<Bool>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let activity {
                ActivityLine(activity: activity, subject: activitySubject)
            } else if case .exited(let cause)? = model.frame?.phase {
                exitedHead(cause)
            }
            if let sentence = model.dictation.sentence { dictationLine(sentence) }
            if !model.attachments.isEmpty || model.uploading > 0 { attachments }
            HStack(alignment: .top, spacing: 8) {
                TextField(placeholder, text: Binding(get: { model.draft }, set: { model.type($0) }), axis: .vertical)
                    .lineLimit(1...8)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.sentences)
                    .focused(focused)
                    .padding(.vertical, 2)
                    .identified("chat.field", label: placeholder, value: model.draft)
                if !model.draft.isEmpty || !model.attachments.isEmpty {
                    Button { model.clearDraft() } label: {
                        // The glyph's own box can be a point short of 20 at
                        // some text sizes; fixed, the target is 44 square.
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 17))
                            .foregroundStyle(design.inkFaint.color)
                            .frame(width: 20, height: 20)
                            .thumbTarget(x: 12, y: 12)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Clear")
                    .identified("chat.clear", label: "Clear")
                    .reclaimingThumbTarget(x: 12, y: 12)
                }
            }
            // At an accessibility text size the model's words get a line of
            // their own: beside the buttons there is room for a few letters.
            if typeSize.isAccessibilitySize, let controls = model.frame?.controls,
               let chip = ChatWords.chip(controls) {
                modelChip(chip, warns: controls.permission?.neverAsks == true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Every control here keeps a 44 pt slot, side by side rather than
            // grown over each other. The row then gives back the slack around
            // the drawn circles at its edges, so they sit on the card's padding.
            HStack(spacing: 4) {
                Button(action: togglePlus) {
                    Image(systemName: "plus")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(design.ink.color)
                        .rotationEffect(.degrees(plusOpen ? 45 : 0))
                        .frame(width: ComposerBox.round, height: ComposerBox.round)
                        .background(Circle().fill(design.sunken.color))
                        .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("Attach")
                .identified("chat.attach", label: String(localized: "Attach"), value: plusOpen ? "open" : "closed")
                if !typeSize.isAccessibilitySize, let controls = model.frame?.controls,
                   let chip = ChatWords.chip(controls) {
                    modelChip(chip, warns: controls.permission?.neverAsks == true)
                }
                Spacer(minLength: 4)
                Button { dictate(.dictate) } label: {
                    Image(systemName: model.dictation.active ? "stop.circle.fill" : "mic")
                        .font(.system(size: 17, weight: .regular))
                        .foregroundStyle(model.dictation.active ? design.accent.color : design.inkMuted.color)
                        .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(dictationLabel)
                .identified("chat.dictate", label: dictationLabel,
                            value: model.dictation.active ? "listening" : "idle")
                primary
            }
            .padding(.horizontal, -(ComposerBox.slot - ComposerBox.round) / 2)
            .padding(.vertical, -(ComposerBox.slot - ComposerBox.round) / 2)
        }
        .padding(.horizontal, 16)
        .padding(.top, 14)
        .padding(.bottom, 12)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .onDisappear { if model.dictation.active { model.dictation.stop() } }
    }

    /// The exited agent's state where the activity line stands while it
    /// works: that the process ended, why, and where.
    private func exitedHead(_ cause: ExitCause) -> some View {
        let title = ChatWords.exited(cause)
        return HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "stop.circle")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(design.inkMuted.color)
            Text(title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(1)
            Spacer(minLength: 8)
            if !host.isEmpty {
                Text(verbatim: host)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .identified("chat.exited", label: title, value: host)
    }

    /// The model and its effort, then a permission or mode other than the
    /// normal one, as one mono pill. A permission that stops asking reads
    /// in red. A tap opens the settings card.
    private func modelChip(
        _ chip: (model: String, detail: String, unusual: [String]), warns: Bool
    ) -> some View {
        let label = ([chip.model, chip.detail] + chip.unusual).filter { !$0.isEmpty }
            .joined(separator: " · ")
        return Button(action: openSettings) {
            HStack(spacing: 5) {
                // On its own line at an accessibility size the words wrap
                // whole. Elsewhere, where the row is tight the effort goes
                // first, then the model name shortens; the permission and
                // mode stay.
                if typeSize.isAccessibilitySize {
                    // A no-break space holds each "·" to the word before
                    // it, so a wrapped line never starts with one.
                    chipWords(
                        chip.model, detail: chip.detail, unusual: chip.unusual, warns: warns,
                        joint: "\u{00A0}· ")
                        .designFont(.monoSmall, design)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    ViewThatFits(in: .horizontal) {
                        chipWords(chip.model, detail: chip.detail, unusual: chip.unusual, warns: warns)
                        chipWords(chip.model, detail: "", unusual: chip.unusual, warns: warns)
                        HStack(spacing: 0) {
                            Text(verbatim: chip.model)
                                .foregroundStyle(design.inkMuted.color)
                                .truncationMode(.tail)
                                .frame(minWidth: 52, alignment: .leading)
                            chipWords("", detail: "", unusual: chip.unusual, warns: warns, after: true)
                                .layoutPriority(1)
                        }
                    }
                    .designFont(.monoSmall, design)
                    .lineLimit(1)
                }
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 11)
            .padding(.vertical, typeSize.isAccessibilitySize ? 6 : 0)
            .frame(minHeight: 30)
            .background {
                // A capsule's ends would cut into words on two lines.
                if typeSize.isAccessibilitySize {
                    RoundedRectangle(cornerRadius: 15, style: .continuous).fill(design.sunken.color)
                } else {
                    Capsule().fill(design.sunken.color)
                }
            }
            .frame(minHeight: ComposerBox.slot)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .layoutPriority(1)
        .disabled(model.settings == nil)
        .identified("chat.model", label: label)
    }

    /// The chip's words; `after` when they follow a name drawn apart. The
    /// first of `unusual` is the permission when `warns`, and reads in red.
    private func chipWords(
        _ name: String, detail: String, unusual: [String], warns: Bool, after: Bool = false,
        joint: String = " · "
    ) -> some View {
        var led = after || !name.isEmpty
        var words = Text(verbatim: name).foregroundStyle(design.inkMuted.color)
        if !detail.isEmpty {
            words = words + Text(verbatim: (led ? joint : "") + detail).foregroundStyle(design.inkMuted.color)
            led = true
        }
        for (index, word) in unusual.enumerated() {
            let red = warns && index == 0
            words = words + Text(verbatim: (led ? joint : "") + word)
                .foregroundStyle(red ? design.removed.color : design.ink.color)
            led = true
        }
        return words.truncationMode(.tail)
    }

    private var dictationLabel: String {
        model.dictation.active ? String(localized: "Stop Dictation") : String(localized: "Dictate")
    }

    /// What dictation is doing, and when it was refused, the way to Settings.
    private func dictationLine(_ sentence: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(sentence)
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
            if model.dictation.phase == .denied {
                Button { dictate(.dictationSettings) } label: {
                    Text("Open Settings")
                        .designFont(.caption, design)
                        .foregroundStyle(design.accent.color)
                        .thumbTarget(x: 4, y: 12)
                }
                .buttonStyle(.amuxControl)
                .identified("chat.dictationSettings", label: String(localized: "Open Settings"))
                .reclaimingThumbTarget(x: 4, y: 12)
            }
        }
        .identified("chat.dictation", label: sentence, value: "\(model.dictation.phase)")
    }

    private var attachments: some View {
        ChipFlow(spacing: 6) {
            ForEach(Array(model.attachments.enumerated()), id: \.offset) { index, attached in
                AttachmentChip(view: attached.view, bytes: model.bytes(of:)) {
                    model.removeAttachment(at: index)
                }
            }
            if model.uploading > 0 {
                Text("Attaching…")
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("chat.attaching")
            }
        }
    }

    private var working: Bool {
        if case .working? = model.frame?.phase { return true }
        return false
    }

    @ViewBuilder
    private var primary: some View {
        if model.frame?.composer.mode == .resume {
            let enabled = model.canResume && model.hasDraft
            Button { model.resume() } label: {
                Text("Resume")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(enabled ? design.ground.color : design.inkFaint.color)
                    .padding(.horizontal, 16)
                    .frame(height: ComposerBox.round)
                    .background(Capsule().fill(enabled ? design.ink.color : design.sunken.color))
                    .padding(.horizontal, (ComposerBox.slot - ComposerBox.round) / 2)
                    .frame(height: ComposerBox.slot)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.amuxControl)
            .fixedSize()
            .disabled(!enabled)
            .identified("chat.resume", label: "Resume", enabled: enabled)
        } else if !model.hasDraft && working {
            // It ends the turn, not the agent: Stop is the menu's, and ends it.
            round("stop.fill", label: String(localized: "Interrupt"), id: "chat.stop", enabled: true) {
                model.interrupt()
            }
        } else {
            round(
                "arrow.up",
                label: working ? String(localized: "Queue") : String(localized: "Send"),
                id: "chat.send", enabled: model.canSend
            ) {
                model.send()
            }
        }
    }

    private func round(
        _ glyph: String, label: String, id: String, enabled: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: glyph)
                .font(.system(size: 14, weight: .bold))
                .foregroundStyle(enabled ? design.ground.color : design.inkFaint.color)
                .frame(width: ComposerBox.round, height: ComposerBox.round)
                .background(Circle().fill(enabled ? design.ink.color : design.sunken.color))
                .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .disabled(!enabled)
        .accessibilityLabel(label)
        .identified(id, label: label, enabled: enabled)
    }
}

/// The draft's attachment chips, wrapped onto as many lines as they need
/// so none is cut off at the composer's edge.
struct ChipFlow: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        let lines = arrange(width: width, subviews: subviews)
        let height = lines.last.map { $0.y + $0.height } ?? 0
        let used = lines.map(\.width).max() ?? 0
        return CGSize(width: proposal.width ?? used, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for line in arrange(width: bounds.width, subviews: subviews) {
            var x = bounds.minX
            for index in line.members {
                let size = subviews[index].sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
                subviews[index].place(
                    at: CGPoint(x: x, y: bounds.minY + line.y), proposal: ProposedViewSize(width: min(size.width, bounds.width), height: size.height))
                x += min(size.width, bounds.width) + spacing
            }
        }
    }

    private struct Line {
        var members: [Int] = []
        var y: CGFloat = 0
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(width: CGFloat, subviews: Subviews) -> [Line] {
        var lines: [Line] = []
        var line = Line()
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(ProposedViewSize(width: width, height: nil))
            let chip = min(size.width, width)
            if !line.members.isEmpty, line.width + spacing + chip > width {
                let y = line.y + line.height + spacing
                lines.append(line)
                line = Line(y: y)
            }
            line.width += (line.members.isEmpty ? 0 : spacing) + chip
            line.height = max(line.height, size.height)
            line.members.append(index)
        }
        if !line.members.isEmpty { lines.append(line) }
        return lines
    }
}

extension DraftAttachment {
    var view: AttachmentView {
        switch self {
        case .image(let blob): .image(blob)
        case .file(let blob): .file(blob)
        case .text(let name, let text): .text(name: name, lines: UInt32(ChatModel.lines(text)), text: text)
        case .review(let diff, let comments): .review(comments: UInt32(comments.count), patch: diff.patch)
        }
    }
}

/// "Running cargo test · 12s" over a moving bar. It is not a row: it says
/// what is happening now and goes when it stops. The words carry the subject
/// in mono and the elapsed time at the right end; the bar slides along a
/// track as wide as the composer, and stands still under reduced motion.
struct ActivityLine: View {
    /// The bar's share of the track.
    static let bar: CGFloat = 0.35
    /// One pass of the bar along the track, in seconds.
    static let pass: Double = 1.6
    /// Where a still bar starts, as a share of the track.
    static let still: CGFloat = 0.32

    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reducesMotion
    let activity: Activity
    let subject: String?

    var body: some View {
        if photographed {
            line(elapsed: activity.elapsedMs, start: Self.still)
        } else {
            TimelineView(.animation(minimumInterval: 1.0 / 30, paused: reducesMotion)) { context in
                let seconds = context.date.timeIntervalSince1970
                let now = Int64(seconds * 1_000)
                let elapsed = activity.sinceMs > 0 ? max(0, now - activity.sinceMs) : activity.elapsedMs
                line(
                    elapsed: elapsed,
                    start: reducesMotion
                        ? Self.still
                        : -Self.bar + (1 + Self.bar) * Self.ease(seconds.truncatingRemainder(dividingBy: Self.pass) / Self.pass))
            }
        }
    }

    /// `start` is where the bar's leading edge stands, as a share of the track.
    private func line(elapsed: Int64, start: CGFloat) -> some View {
        let parts = ChatWords.activityParts(activity.kind, elapsedMs: elapsed, subject: subject)
        return VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(parts.words)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                    .layoutPriority(2)
                if let subject = parts.subject {
                    Text(verbatim: subject)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 8)
                if let time = parts.time {
                    Text(verbatim: time)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
            }
            GeometryReader { geometry in
                Capsule().fill(design.inkMuted.color)
                    .frame(width: geometry.size.width * Self.bar, height: 2)
                    .offset(x: geometry.size.width * start)
            }
            .frame(height: 2)
            .clipped()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(ChatWords.activity(activity.kind, elapsedMs: elapsed, subject: subject))
        .identified("chat.activity", label: ChatWords.activity(activity.kind, elapsedMs: 0, subject: subject))
    }

    /// Ease in and out: the bar speeds up across the middle of the track.
    private static func ease(_ t: Double) -> CGFloat {
        CGFloat(t < 0.5 ? 2 * t * t : 1 - pow(-2 * t + 2, 2) / 2)
    }
}

/// What docks above the composer: the agent's task list as one chip that
/// opens to the list and the agents this one started, then the queued
/// prompts and this phone's prompts not yet in the chat. Parts stack in one
/// card with hairlines between them; the card goes when all are empty.
struct ChatDock: View {
    @Environment(\.design) private var design
    let model: ChatModel
    /// The agents this one started.
    let children: [FleetCard]
    /// The agent's host, which a prompt waits for while the link is down.
    let host: String
    let open: (AgentKey) -> Void
    @State private var expanded: Bool

    init(
        model: ChatModel, children: [FleetCard], host: String = "", expanded: Bool = false,
        open: @escaping (AgentKey) -> Void
    ) {
        self.model = model
        self.children = children
        self.host = host
        self.open = open
        _expanded = State(initialValue: expanded)
    }

    var body: some View {
        let tasks = model.overview?.tasks
        let queue = model.frame?.queue ?? []
        let underway = model.landingInQueue
        let head = tasks != nil || !children.isEmpty
        if head || !queue.isEmpty || !underway.isEmpty {
            VStack(alignment: .leading, spacing: 0) {
                if head {
                    if expanded {
                        if let tasks, !tasks.entries.isEmpty {
                            taskList(tasks.entries)
                            rule
                        }
                        if !children.isEmpty {
                            started
                            rule
                        }
                    }
                    chip(tasks)
                }
                ForEach(Array(queue.enumerated()), id: \.offset) { index, row in
                    if head || index > 0 { rule }
                    queued(row, index: index)
                }
                ForEach(Array(underway.enumerated()), id: \.offset) { index, prompt in
                    if head || !queue.isEmpty || index > 0 { rule }
                    onItsWay(prompt, index: index)
                }
            }
            .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
            .identified("chat.dock", value: expanded ? "expanded" : "folded")
        }
    }

    private var rule: some View {
        Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
    }

    // MARK: The chip

    private func chip(_ tasks: TasksView?) -> some View {
        let count = children.count
        let title = tasks.map(ChatWords.headTask) ?? ChatWords.started(count)
        let label = tasks.map { "\($0.done)/\($0.total) \(title)" } ?? title
        return Button { expanded.toggle() } label: {
            HStack(spacing: 10) {
                if let tasks {
                    Text(verbatim: "\(tasks.done)/\(tasks.total)")
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                Text(verbatim: title)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                Spacer(minLength: 6)
                if count > 0 {
                    HStack(spacing: 4) {
                        Image(systemName: "arrow.triangle.branch")
                            .font(.system(size: 11, weight: .semibold))
                        Text(verbatim: "\(count)")
                            .designFont(.monoSmall, design)
                    }
                    .foregroundStyle(design.ink.color)
                    .padding(.horizontal, 10)
                    .frame(height: 26)
                    .background(Capsule().fill(design.sunken.color))
                }
                Image(systemName: expanded ? "chevron.down" : "chevron.up")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 20)
            }
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 50, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(count > 0 ? "\(label), \(ChatWords.started(count))" : label)
        .accessibilityValue(expanded ? String(localized: "Expanded") : String(localized: "Collapsed"))
        .identified("chat.tasks", label: label, value: expanded ? "expanded" : "folded")
    }

    private func taskList(_ entries: [TaskLine]) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(entries.enumerated()), id: \.offset) { index, entry in
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Image(systemName: glyph(entry.mark))
                        .font(.system(size: 13, weight: entry.mark == .done ? .semibold : .regular))
                        .foregroundStyle(entry.mark == .todo ? design.inkMuted.color : design.inkFaint.color)
                        .frame(width: 18)
                    Text(verbatim: entry.subject)
                        .designFont(entry.mark == .current ? .bodyEmphasis : .body, design)
                        .foregroundStyle(entry.mark == .done ? design.inkFaint.color : design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
                .identified("chat.tasks.\(index)", label: entry.subject, value: "\(entry.mark)")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
    }

    private func glyph(_ mark: TaskMark) -> String {
        switch mark {
        case .done: "checkmark"
        case .current: "circle.inset.filled"
        case .todo: "circle"
        }
    }

    private var started: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("STARTED")
                .designFont(.sectionTitle, design)
                .foregroundStyle(design.inkFaint.color)
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 4)
            ForEach(children, id: \.agent) { child in
                Button { open(child.agent) } label: {
                    HStack(spacing: 10) {
                        Group {
                            if child.attention == .needsYou || child.familyAttention == .needsYou {
                                NeedsYouDot()
                            } else {
                                Color.clear
                            }
                        }
                        .frame(width: 18, height: 8)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(verbatim: child.name)
                                .designFont(.identifier, design)
                                .foregroundStyle(design.ink.color)
                                .lineLimit(1)
                            Text(verbatim: ChatWords.childState(child))
                                .designFont(.monoSmall, design)
                                .foregroundStyle(
                                    child.attention == .needsYou ? design.accent.color : design.inkFaint.color)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 6)
                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(design.inkFaint.color)
                    }
                    .padding(.horizontal, 16)
                    .frame(maxWidth: .infinity, minHeight: 54, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("\(child.name), \(ChatWords.childState(child))")
                .identified(
                    "chat.family.\(child.name)", label: child.name,
                    value: child.attention == .needsYou ? "needs-you" : nil)
            }
        }
        .padding(.bottom, 6)
    }

    // MARK: Prompts on their way

    private func queued(_ row: QueuedRow, index: Int) -> some View {
        let id = "chat.queued.\(index)"
        return trayRow(
            text: ChatWords.text(of: row.text), state: ChatWords.queued(row), warn: false, id: id
        ) {
            Image(systemName: "clock")
                .font(.system(size: 14))
                .foregroundStyle(design.inkMuted.color)
        } actions: {
            if row.canSendNow {
                icon("arrow.up.circle", String(localized: "Send now"), id: "\(id).sendNow") { model.sendNow(row) }
            }
            if row.canWithdraw {
                icon("pencil", String(localized: "Withdraw"), id: "\(id).withdraw") { model.withdraw(row) }
            }
        }
    }

    /// A prompt of this client's waiting in the queue: sending, or, when
    /// it may not have arrived, with Resend and Discard.
    private func onItsWay(_ prompt: SentPrompt, index: Int) -> some View {
        let id = "chat.underway.\(index)"
        let lost = prompt.underway == .mayNotHaveArrived
        let held = prompt.underway == .held
        return trayRow(
            text: ChatWords.text(of: prompt.text), state: ChatWords.underway(prompt.underway, host: host),
            warn: lost, id: id
        ) {
            if lost {
                Image(systemName: "exclamationmark.circle")
                    .font(.system(size: 14))
                    .foregroundStyle(design.accent.color)
            } else if held {
                Image(systemName: "clock")
                    .font(.system(size: 14))
                    .foregroundStyle(design.inkMuted.color)
            } else {
                Image(systemName: "paperplane")
                    .font(.system(size: 13))
                    .foregroundStyle(design.inkMuted.color)
            }
        } actions: {
            if lost {
                icon("arrow.clockwise", String(localized: "Resend"), id: "\(id).resend") { model.resend(prompt.inputId) }
                icon("xmark", String(localized: "Discard"), id: "\(id).discard") { model.discard(prompt.inputId) }
            }
            // It never left, so taking it back puts its words in the draft.
            if held {
                icon("xmark", String(localized: "Cancel"), id: "\(id).cancel") { model.cancelHeld(prompt.inputId) }
            }
        }
    }

    /// A prompt on one line after its glyph, its state under it, and its
    /// acts as glyphs at the end of the row.
    private func trayRow<Glyph: View, Actions: View>(
        text: String, state: String, warn: Bool, id: String,
        @ViewBuilder glyph: () -> Glyph, @ViewBuilder actions: () -> Actions
    ) -> some View {
        HStack(alignment: .center, spacing: 12) {
            glyph().frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(text)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                Text(state)
                    .designFont(.caption, design)
                    .foregroundStyle(warn ? design.accent.color : design.inkFaint.color)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            HStack(spacing: 0) { actions() }
                .padding(.trailing, -10)
        }
        .padding(.leading, 16)
        .padding(.trailing, 16)
        .padding(.vertical, 8)
        .frame(minHeight: 52)
        .accessibilityElement(children: .contain)
        .identified(id, label: text, value: state)
    }

    private func icon(_ glyph: String, _ title: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: glyph)
                .font(.system(size: 16, weight: .regular))
                .foregroundStyle(design.inkMuted.color)
                .frame(width: 40, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(title)
        .identified(id, label: title)
    }
}

/// What the plus offers: a photo or a file to attach, and the permission
/// mode, each one tap from the composer.
struct PlusCard: View {
    @Environment(\.design) private var design
    let settings: SettingsView?
    let attach: (AttachChoice) -> Void
    let openSettings: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 10) {
                tile(String(localized: "Photo"), glyph: "photo", id: "chat.attach.photo") { attach(.photo) }
                tile(String(localized: "File"), glyph: "doc", id: "chat.attach.file") { attach(.file) }
            }
            if let settings, !settings.permissions.isEmpty || settings.changeable.permission == .cycle {
                permissions(settings)
            }
        }
        .padding(10)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.plus")
    }

    private func permissions(_ settings: SettingsView) -> some View {
        let current = settings.permissions.first { $0.current }.map(ChatWords.permission)
        return Button(action: openSettings) {
            HStack(spacing: 12) {
                Image(systemName: "lock")
                    .font(.system(size: 16, weight: .regular))
                    .foregroundStyle(design.ink.color)
                    .frame(width: 20)
                Text("Permissions")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                    .layoutPriority(2)
                Spacer(minLength: 8)
                if let current {
                    Text(verbatim: current)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 52)
            .background(
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color))
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .identified("chat.permissions", label: ChatWords.permissionsItem(settings))
    }

    private func tile(_ title: String, glyph: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                Image(systemName: glyph)
                    .font(.system(size: 22, weight: .regular))
                    .foregroundStyle(design.ink.color)
                Text(title)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
            }
            .frame(maxWidth: .infinity, minHeight: 84)
            .background(
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color))
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .identified(id, label: title)
    }
}

/// The facts strip above the composer: a usage limit reached and when it
/// resets, context near its end, what runs in the background, a usage
/// limit coming, a tool server that failed.
/// Each part shows only while it is true.
///
/// The parts are one run of text that wraps rather than a row of labels that
/// truncate: at a phone's width three facts side by side each cut to a few
/// letters, and a fact nobody can read is not shown at all.
///
/// A tap opens the overview, which lists what the strip counts.
struct StripLine: View {
    @Environment(\.design) private var design
    let context: ContextView?
    let overview: Overview
    let open: () -> Void

    var body: some View {
        let parts = ChatWords.strip(context: context, overview: overview, now: Date())
        if !parts.isEmpty {
            Button(action: open) { line(parts) }
                .buttonStyle(.amuxControl)
                .accessibilityHint("Opens the overview")
                .identified("chat.strip", label: parts.map(\.text).joined(separator: ", "))
        }
    }

    private func line(_ parts: [(text: String, warn: Bool)]) -> some View {
        HStack(spacing: 0) {
            parts.enumerated().reduce(Text(verbatim: "")) { line, item in
                let (index, part) = item
                let fact = Text(verbatim: part.text)
                    .foregroundStyle(part.warn ? design.accent.color : design.inkMuted.color)
                guard index > 0 else { return line + fact }
                return line + Text(verbatim: " · ").foregroundStyle(design.inkFaint.color) + fact
            }
            .designFont(.caption, design)
            .lineLimit(3)
            .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 7)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous), as: .glass)
        .accessibilityElement(children: .combine)
    }
}

/// The agent's commands matching a draft that is a leading "/word", one
/// row each; a tap puts the command in the draft.
struct SlashRows: View {
    @Environment(\.design) private var design
    let commands: [CommandView]
    /// Codex's commands are its skills, mentioned by "$name".
    let codex: Bool
    let pick: (CommandView) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(commands, id: \.name) { command in
                Button { pick(command) } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(verbatim: (codex ? "$" : "/") + command.name)
                                .designFont(.mono, design)
                                .foregroundStyle(design.ink.color)
                                .lineLimit(1)
                            if !command.argumentHint.isEmpty {
                                Text(verbatim: command.argumentHint)
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.inkFaint.color)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 0)
                        }
                        if !command.description.isEmpty {
                            Text(verbatim: command.description)
                                .designFont(.detail, design)
                                .foregroundStyle(design.inkMuted.color)
                                .lineLimit(1)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(ChatWords.command(command))
                .identified("chat.slash.\(command.name)", label: ChatWords.command(command))
            }
        }
        .padding(.vertical, 4)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous), as: .glass)
    }
}

/// Stands in place of the composer when the agent cannot take a message
/// until something outside this chat changes: its sign-in.
struct FootCard: View {
    @Environment(\.design) private var design
    let kind: String
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.circle")
                .foregroundStyle(design.accent.color)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                if !detail.isEmpty {
                    Text(detail)
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.foot.\(kind)", label: title, value: detail)
    }
}
