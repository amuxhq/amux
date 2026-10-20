import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did about giving up an account.
public enum DeleteAccountAction: Equatable, Sendable {
    case cancel
    /// Delete it, with the address that has been typed.
    case confirm
    /// Leave for wherever the subscription is billed, to stop it renewing.
    case manageBilling(URL)
}

/// Giving up an account, with what that actually does spelled out.
///
/// It states what happens rather than asking whether you are sure, because
/// "are you sure" asks a question nobody can answer without knowing what is at
/// stake. Here what is at stake is unusually reassuring and goes first: an
/// amux account is a credential for reaching machines, so deleting one stops
/// no agent and reverts no edit — it stops this phone reaching them.
///
/// The address is typed because the button is one press away from something
/// nothing can undo, and a person who cannot say which account they are in is
/// a person about to delete the wrong one.
struct DeleteAccountCard: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let model: DeletionStore
    let actions: @MainActor (DeleteAccountAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Delete this account?")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 9) {
                Consequence("checkmark", kept: true,
                            "Your agents and files stay on your hosts.")
                Consequence("xmark", kept: false, "This phone can no longer reach them.")
                if let billing {
                    Consequence("xmark", kept: false, billing)
                }
            }
            trouble
            // Blocked, the address and Delete would only be refused again;
            // what there is to do is stop the renewal, and the question comes
            // back, address kept, on the way out to do that.
            if !blocked { confirmation }
            buttons
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(
            RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous),
            wash: 0.9)
        .accessibilityElement(children: .contain)
        .identified("delete", value: state)
    }

    private var state: String {
        switch model.phase {
        case .asking: "asking"
        case .working: "deleting"
        case .blocked(let source, _): "blocked by \(source.named)"
        case .deleted: "deleted"
        case .failed: "failed"
        }
    }

    /// What deleting does to what has been paid for, which is different for
    /// every state a subscription can be in — and in none of them does
    /// deleting an account stop the billing. A renewal is stopped where it was
    /// bought, by the person, and saying otherwise here would be a promise
    /// this app cannot keep.
    private var billing: String? {
        switch entry.entitlement {
        case .active(.purchased(let source), .some(let renews)):
            "Deleting this account leaves your subscription renewing on \(Self.day(renews)) through \(source.place)."
        case .active(.purchased(let source), nil):
            "Your subscription through \(source.place) is not refunded."
        // Nothing was paid for, so there is nothing to warn about losing money
        // over — only the access itself, which the sentence above already says
        // goes.
        case .active(.granted, _):
            nil
        case .lapsed(.purchased, _):
            "Nothing already paid for is refunded."
        case .lapsed(.granted, _), .none:
            nil
        }
    }

    /// A renewal date as a person says it: the day and the month, with no year
    /// on a date inside the next twelve months and no time of day on something
    /// a billing system settles overnight.
    private static func day(_ date: Date) -> String {
        date.formatted(.dateTime.day().month(.wide))
    }

    /// Why the last press did not delete anything.
    ///
    /// A blocked deletion is not a failure and does not read as one: the
    /// account service will not delete an account while its subscription is
    /// still set to renew, and the one place that can be stopped is where it
    /// was bought. So this says where to go and offers to go there, as the
    /// one thing on the card to press.
    @ViewBuilder
    private var trouble: some View {
        switch model.phase {
        case .blocked(let source, let manageURL):
            VStack(alignment: .leading, spacing: 8) {
                Text("Cancel renewal first")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Explain("Cancel renewal through \(source.place), then return to delete this account.")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain)
            .identified("delete.blocked", value: source.named)
        case .failed(let reason):
            VStack(alignment: .leading, spacing: 4) {
                Text("Nothing was deleted")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                // The account service's own words. This app does not know what
                // went wrong on amux.sh, and a friendlier sentence invented
                // here would be a guess about somebody else's system.
                Explain(reason)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .identified("delete.failed", value: reason)
        case .asking, .working, .deleted:
            EmptyView()
        }
    }

    /// The address, typed. The field says whose it must be rather than making
    /// somebody go back and look, and it holds the account's own address as
    /// its placeholder — not as its contents, which would be the app typing
    /// the confirmation for you.
    private var confirmation: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Type your email to confirm")
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
                .fixedSize(horizontal: false, vertical: true)
            TextField(entry.account.email, text: Bindable(model).typed)
                .textFieldStyle(.plain)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.emailAddress)
                .submitLabel(.done)
                .onSubmit { if confirmed { actions(.confirm) } }
                .padding(.horizontal, 14)
                .frame(height: 52)
                .background {
                    RoundedRectangle(
                        cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("delete.email", label: "Your email", value: model.typed)
        }
    }

    private var blocked: Bool {
        if case .blocked = model.phase { true } else { false }
    }

    private var confirmed: Bool {
        model.confirms(entry.account.email) && !model.working
    }

    private var buttons: some View {
        ButtonPair {
            // "Not Now" beside a renewal to cancel, so the two buttons do not
            // both start with the same word for different things.
            choiceButton(
                blocked ? String(localized: "Not Now") : String(localized: "Cancel"),
                kind: .outline, id: "delete.cancel"
            ) { actions(.cancel) }
            if case .blocked(let source, let manageURL) = model.phase {
                choiceButton(
                    String(localized: "Cancel Renewal in \(source.place)"), kind: .primary, id: "delete.manage"
                ) { actions(.manageBilling(manageURL)) }
            } else {
            // Until the address is this account's, the one press that cannot
            // be undone is not available at all — greyed rather than hidden,
            // so what the field is for is obvious from the button it unlocks.
            choiceButton(
                model.working ? String(localized: "Deleting…") : String(localized: "Delete"),
                kind: .destructive, id: "delete.confirm", enabled: confirmed
            ) { actions(.confirm) }
            }
        }
        .padding(.top, 2)
    }

}

/// The question over the page it was asked from.
///
/// The page behind is dimmed as one thing rather than screen by screen: what
/// is being asked is about the account the whole page is about, and a card
/// that only greyed the rows under it would read as a question about those
/// rows. The scrim covers the safe areas too, so nothing under the card stays
/// bright enough to look pressable.
public struct DeleteAccountOverlay<Content: View>: View {
    private let entry: AccountEntry?
    private let model: DeletionStore
    private let actions: @MainActor (DeleteAccountAction) -> Void
    private let content: Content

    public init(
        entry: AccountEntry?,
        model: DeletionStore,
        actions: @escaping @MainActor (DeleteAccountAction) -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.entry = entry
        self.model = model
        self.actions = actions
        self.content = content()
    }

    public var body: some View {
        ZStack {
            content
            if let entry, model.account == entry.id {
                Color.black.opacity(Glass.scrim)
                    .ignoresSafeArea()
                    .onTapGesture { actions(.cancel) }
                    .accessibilityHidden(true)
                DeleteAccountCard(entry: entry, model: model, actions: actions)
                    .padding(.horizontal, 12)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                    .padding(.bottom, 10)
            }
        }
        .moving(value: entry.map { model.account == $0.id } ?? false)
    }
}
