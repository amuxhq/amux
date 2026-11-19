import AmuxValues
import AmuxWords
import SwiftUI

/// Asks what deleting an agent does to its workspace: nil when the
/// workspace stays. The answer can wait on the agent's host.
public typealias DeleteWarningAsk = @MainActor (AgentKey) async -> Result<DeleteWarning?, any Error>

/// The confirmation before an agent is deleted, as a sheet on the window.
///
/// It says what goes in the words the phone's confirmation uses. A system
/// dialog would not do: what it says changes once the host has looked at
/// the worktree, and a dialog's words are fixed when it opens. Until the
/// answer comes the sheet says it is checking, with a spinner by the
/// buttons, and Delete waits for it: deleting is a choice made on what the
/// sheet says, so it is not offered before the sheet can say it. When the
/// answer is that uncommitted work goes, Delete is drawn solid red.
public struct DeleteSheet: View {
    let card: FleetCard
    let ask: DeleteWarningAsk
    let cancel: @MainActor () -> Void
    let confirm: @MainActor (AgentAct) -> Void

    @State private var warning: DeleteWarning?

    public init(
        card: FleetCard,
        ask: @escaping DeleteWarningAsk,
        cancel: @escaping @MainActor () -> Void,
        confirm: @escaping @MainActor (AgentAct) -> Void
    ) {
        self.card = card
        self.ask = ask
        self.cancel = cancel
        self.confirm = confirm
        _warning = State(initialValue: DeleteWords.asking(card.workspaceGoes))
    }

    public var body: some View {
        let checking = DeleteWords.awaitsAnswer(warning)
        let discards = DeleteWords.discards(warning)
        VStack(alignment: .leading, spacing: 18) {
            HStack(alignment: .top, spacing: 14) {
                Icon("trash-2", size: 18)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 36, height: 36)
                    // A quiet fill that reads on the sheet's raised ground, as the
                    // buttons' does.
                    .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Color.primary.opacity(0.065)))
                VStack(alignment: .leading, spacing: 6) {
                    Text("Delete \u{201C}\(card.name)\u{201D}?")
                        .font(.ui(.title, weight: .semibold))
                        .fixedSize(horizontal: false, vertical: true)
                    Text(message)
                        .font(.ui(.body))
                        .foregroundStyle(Palette.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("fleet.delete.message")
                }
            }
            HStack(spacing: 8) {
                if checking {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Checking for uncommitted changes")
                }
                Spacer()
                Button("Cancel", role: .cancel, action: cancel)
                    .buttonStyle(PillButtonStyle())
                    .keyboardShortcut(.cancelAction)
                Button("Delete", role: .destructive) {
                    confirm(discards ? .deleteDiscardingChanges : .delete)
                }
                .buttonStyle(DestroyButtonStyle(solid: true))
                .disabled(checking)
                .help(checking ? "Checking what gets deleted…" : "Delete this agent")
            }
        }
        .padding(20)
        .frame(width: 440)
        .textTiers()
        .background(Palette.raised)
        .task {
            guard DeleteWords.awaitsAnswer(warning) else { return }
            let answer = await ask(card.agent)
            // A sheet put away before its answer came has nothing to show it on.
            guard !Task.isCancelled else { return }
            warning = DeleteWords.answered(warning, with: answer)
        }
    }

    /// The shared words, the branch's name in the monospaced face and
    /// kept on one line: a name like fix-scroll-jump otherwise breaks at
    /// its hyphens and reads as two words.
    private var message: AttributedString {
        var said = AttributedString(DeleteWords.message(descendants: card.descendants, warning: warning))
        guard let branch = Self.branch(warning), !branch.isEmpty,
            let range = said.range(of: "branch \(branch)")
        else { return said }
        var name = AttributedString(branch.replacingOccurrences(of: "-", with: "\u{2011}"))
        name.font = .mono(.callout)
        let start = said.index(range.lowerBound, offsetByCharacters: "branch ".count)
        said.replaceSubrange(start..<range.upperBound, with: name)
        return said
    }

    private static func branch(_ warning: DeleteWarning?) -> String? {
        switch warning {
        case .goes(let goes)?, .checking(let goes)?, .unknown(let goes)?: goes.branch
        case nil: nil
        }
    }
}

/// Delete's button: red words on the quiet fill, or solid red with white
/// words when what it destroys cannot be had back.
private struct DestroyButtonStyle: ButtonStyle {
    let solid: Bool
    @Environment(\.isEnabled) private var enabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.ui(.body, weight: solid ? .medium : .regular))
            .lineLimit(1)
            .foregroundStyle(solid ? Color.white : Palette.failure)
            .padding(.horizontal, 12)
            .frame(height: 28)
            .background(
                RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                    .fill(solid ? Palette.failure : Color.primary.opacity(hovering ? 0.1 : 0.065)))
            .opacity(configuration.isPressed ? 0.75 : (enabled ? 1 : 0.45))
            .contentShape(RoundedRectangle(cornerRadius: Radius.control, style: .continuous))
            .onHover { hovering = $0 }
    }
}

/// Answers that the host could not be asked, for a window given no way to
/// ask: a worktree with changes then stays on disk.
public struct DeleteUnasked: Error {
    public init() {}
}
