import AmuxValues
import AmuxWords
import SwiftUI

/// Asks what deleting an agent does to its workspace: nil when the
/// workspace stays. The answer can wait on the agent's host.
public typealias DeleteWarningAsk = @MainActor (AgentKey) async -> Result<DeleteWarning?, any Error>

/// The confirmation before an agent is deleted, as a sheet on the window.
///
/// It says what goes in the words the phone's confirmation uses. A system
/// dialog would not do: what it says changes once the agent's host has said
/// what the delete takes, and a dialog's words are fixed when it opens.
/// Until the answer comes the sheet says it is checking, with a spinner by
/// the buttons, and deleting then keeps a worktree with changes on disk,
/// because nothing it said named them.
public struct DeleteSheet: View {
    let card: FleetCard
    let ask: DeleteWarningAsk
    let cancel: @MainActor () -> Void
    let confirm: @MainActor (AgentAct) -> Void

    /// Checking until the agent's host answers, then its answer: nil when
    /// the workspace stays.
    @State private var warning: DeleteWarning? = .checking

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
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top, spacing: 14) {
                Icon("trash-2", size: 26)
                    .foregroundStyle(.secondary)
                    .padding(.top, 2)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Delete \(card.name)?")
                        .font(.ui(.title, weight: .semibold))
                    Text(DeleteWords.message(descendants: card.descendants, warning: warning))
                        .font(.ui(.body))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("fleet.delete.message")
                }
            }
            HStack(spacing: 8) {
                if DeleteWords.awaitsAnswer(warning) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Checking")
                }
                Spacer()
                Button("Cancel", role: .cancel, action: cancel)
                    .keyboardShortcut(.cancelAction)
                // Red words, as a Mac alert draws the button that destroys.
                Button(role: .destructive) {
                    confirm(DeleteWords.discards(warning) ? .deleteDiscardingChanges : .delete)
                } label: {
                    Text("Delete").foregroundStyle(.red)
                }
            }
        }
        .padding(20)
        .frame(width: 420)
        .task {
            guard DeleteWords.awaitsAnswer(warning) else { return }
            let answer = await ask(card.agent)
            // A sheet put away before its answer came has nothing to show it on.
            guard !Task.isCancelled else { return }
            warning = DeleteWords.answered(warning, with: answer)
        }
    }
}

/// Answers that the host could not be asked, for a window given no way to
/// ask: a worktree with changes then stays on disk.
public struct DeleteUnasked: Error {
    public init() {}
}
