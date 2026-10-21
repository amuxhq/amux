import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on the paywall.
public enum PaywallAction: Equatable, Sendable {
    case cancel
    case choose(Plan.Period)
    case buy
    case restore
    /// Offer a purchase amux.sh has not confirmed to it again.
    case retry
    /// Open where an App Store subscription is managed.
    case manage
}

/// Subscribing.
///
/// One sentence about what the subscription is, then the plans. There is no
/// list of perks, because a list of perks implies a version without them and
/// there is not one: the subscription is what connects this phone to any host
/// at all. Nothing here is dressed up — no badges, no countdown, no "most
/// popular" — because the decision is whether the app works, and a screen that
/// pushed would be pushing on the only door.
public struct Paywall: View {
    @Environment(\.design) private var design
    private let model: PaywallStore
    private let actions: @MainActor (PaywallAction) -> Void

    public init(model: PaywallStore, actions: @escaping @MainActor (PaywallAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                heading
                if model.entitled {
                    subscribed
                } else if settling {
                    // A purchase already exists. Leaving two plans on screen
                    // to be picked would be offering to buy a second one, and
                    // the button underneath does not buy — it sends the one
                    // that was bought to amux.sh again.
                    EmptyView()
                } else {
                    plans
                    terms
                }
                Spacer(minLength: 22)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, design.metrics.gutter)
            foot
        }
        .backBar("paywall.back") { actions(.cancel) }
        // A screen is a container of the things on it, not a name for all of
        // them: without this the system spreads this identifier over the
        // title, the rows and the buttons underneath.
        .accessibilityElement(children: .contain)
        .identified("paywall", value: state)
    }

    private var state: String {
        switch model.phase {
        case .ready: model.chosen.rawValue
        case .buying: "buying"
        case .awaitingApproval: "awaiting approval"
        case .confirming: "confirming"
        case .unconfirmed(let why): "unconfirmed \(why.named)"
        case .failed: "failed"
        case .entitled(let grant): "entitled by \(grant.named)"
        }
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Reach your agents\nanywhere")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("paywall.title", value: "Reach your agents anywhere")
            Text(explanation)
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("paywall.explanation", value: explanation)
        }
        .padding(.top, 8)
        .padding(.bottom, 22)
    }

    private var explanation: String {
        "A subscription reaches your hosts from anywhere. Hosts on this network work without one."
    }

    private var plans: some View {
        VStack(spacing: 0) {
            ForEach(Array(model.plans.enumerated()), id: \.element.id) { index, plan in
                row(plan)
                if index < model.plans.count - 1 {
                    Rectangle()
                        .fill(design.hairline.color)
                        .frame(height: design.metrics.hairline)
                        .padding(.leading, 40)
                }
            }
        }
    }

    private func row(_ plan: Plan) -> some View {
        let chosen = plan.period == model.chosen
        return Button { actions(.choose(plan.period)) } label: {
            HStack(spacing: 14) {
                // The mark is a radio rather than a check:
                // these two are alternatives, and a check would read as two
                // things that could both be on.
                Radio(chosen: chosen)
                    .frame(width: 26)
                Text(plan.period.title)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                VStack(alignment: .trailing, spacing: 2) {
                    Text(plan.price)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                    if let saving = plan.saving {
                        Text(saving)
                            .designFont(.detail, design)
                            .foregroundStyle(design.accent.color)
                    }
                }
            }
            .padding(.vertical, 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .disabled(model.working)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "paywall.\(plan.period.rawValue)",
            label: "\(plan.period.title) \(plan.price)",
            value: chosen ? "chosen" : "not chosen", enabled: !model.working)
    }

    /// What is true about every subscription, whichever is chosen. Where it is
    /// cancelled, and that a subscription bought anywhere else already counts.
    private var terms: some View {
        Text("Cancel any time in the App Store. A subscription bought on the web carries over.")
            .designFont(.detail, design)
            .foregroundStyle(design.inkFaint.color)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, 14)
            .identified("paywall.terms")
    }

    /// Somebody who already has it. The screen says so and where it came
    /// from, rather than offering to sell a second subscription for the same
    /// thing.
    @ViewBuilder
    private var subscribed: some View {
        if let grant = model.grant {
            VStack(alignment: .leading, spacing: 4) {
                Text(Self.subscribed(grant))
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Explain(Self.honoured(grant))
                // Said because it is the one thing that looks broken
                // otherwise. This phone goes live the moment it re-reads its
                // own standing; every other device finds out at its next
                // re-check, and somebody staring at a laptop that still says
                // "away" needs to know that is the wait rather than a fault.
                Explain(Self.elsewhere)
                    .padding(.top, 6)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 14)
            .accessibilityElement(children: .combine)
            .identified("paywall.subscribed", value: grant.named)
        }
    }

    /// What happens on everything that is not this phone. The relay is asked
    /// again here as soon as a purchase lands, so this device is live at once;
    /// the others ask on their own schedule.
    static let elsewhere = "Your other devices follow within a few minutes."

    /// Where the subscription was bought, in the preposition that place
    /// takes: one is a store you are in, the other a site you are on. Access
    /// that was given was bought nowhere, so it names no place and does not
    /// call itself a subscription.
    static func subscribed(_ grant: Grant) -> String {
        switch grant {
        case .purchased(.appStore): "Subscribed in the App Store"
        case .purchased(.web): "Subscribed on amux.sh"
        case .granted: "Pro is on for this account"
        }
    }

    /// A subscription bought anywhere counts everywhere, and where it is
    /// managed is where it was bought. Access that was given is managed
    /// nowhere, and saying otherwise would send somebody looking for a
    /// subscription that does not exist.
    static func honoured(_ grant: Grant) -> String {
        switch grant {
        case .purchased(.appStore):
            "Manage your subscription in the App Store."
        case .purchased(.web):
            "Manage your subscription on amux.sh."
        case .granted:
            "Relay access is included with this account."
        }
    }

    /// The one thing pressed, and everything said about pressing it.
    ///
    /// Subscribed, the one thing left is managing it where it was bought.
    /// The App Store's page is a press away; a web subscription is managed
    /// on amux.sh and access that was given is managed nowhere, so those
    /// have no button, and going back is the bar's.
    @ViewBuilder
    private var foot: some View {
        if model.entitled {
            if model.grant == .purchased(.appStore) {
                VStack {
                    Spacer(minLength: 0)
                    BottomAction {
                        Button { actions(.manage) } label: {
                            ActionLabel("Manage Subscription", kind: .outline, fill: true)
                        }
                        .buttonStyle(.amuxControl)
                        .identified("paywall.manage", label: "Manage Subscription")
                    }
                }
            }
        } else {
            purchase
        }
    }

    private var purchase: some View {
        VStack {
            Spacer(minLength: 0)
            BottomAction {
                VStack(spacing: 10) {
                    trouble
                    // A wait is said in ink beside the breathing mark rather
                    // than on a greyed button nobody can read.
                    if model.working || waiting {
                        WaitingLine(title: title)
                            .identified("paywall.buy", label: title, value: state, enabled: false)
                    } else {
                        Button { actions(pressing) } label: {
                            ActionLabel(title, kind: .primary, fill: true)
                        }
                        .buttonStyle(.amuxControl)
                        .identified("paywall.buy", label: title, value: state)
                    }
                    if !model.entitled {
                        Button { actions(.restore) } label: {
                            Text("Restore Purchases")
                                .designFont(.body, design)
                                .foregroundStyle(design.accent.color)
                                .thumbTarget(y: 14)
                        }
                        .buttonStyle(.amuxControl)
                        .disabled(model.working)
                        .identified(
                            "paywall.restore", label: "Restore Purchases",
                            enabled: !model.working)
                        .reclaimingThumbTarget(y: 14)
                    }
                }
            }
        }
    }

    /// Whether a purchase has been made and is on its way to amux.sh — posted
    /// and waiting, or waiting to be sent again.
    private var settling: Bool {
        model.phase == .confirming || unconfirmed
    }

    /// A purchase the store has taken and cannot finish. Buying again would be
    /// a second charge for the same month, so the button stops offering it.
    private var waiting: Bool { model.phase == .awaitingApproval }

    /// Whether this purchase is bought and still not this account's. The
    /// button offers it to amux.sh again rather than offering to buy a second
    /// one, which is the one thing that must not happen here.
    private var unconfirmed: Bool {
        if case .unconfirmed = model.phase { return true }
        return false
    }

    /// What pressing the one button means, which is not always buying.
    private var pressing: PaywallAction { unconfirmed ? .retry : .buy }

    private var title: String {
        switch model.phase {
        case .buying: return "Waiting for the App Store…"
        case .awaitingApproval: return "Waiting for approval"
        case .confirming: return "Confirming with amux.sh…"
        case .unconfirmed: return "Retry"
        default: break
        }
        guard let plan = model.plan else { return "Subscribe" }
        return "Subscribe · \(plan.price) \(plan.period.spelled)"
    }

    /// What came back, when it was not a subscription.
    @ViewBuilder
    private var trouble: some View {
        switch model.phase {
        case .failed(let reason):
            note("That did not go through", reason, id: "failed", value: reason)
        case .awaitingApproval:
            // Ask to Buy, or a bank asking for a second factor. Nothing is
            // owed and nothing is bought, and the person has to be told that
            // rather than left looking at a button that stopped working.
            note(
                "Waiting for approval",
                "Your subscription starts when the App Store approves your purchase.",
                id: "pending", value: "pending")
        // Paid for, and amux.sh has not said so yet. The purchase is kept
        // either way: the App Store still holds it, so pressing Retry — or
        // just opening the app again — offers it once more.
        case .unconfirmed(let why):
            note(
                Self.trouble(why), Self.unconfirmed(why), id: "unconfirmed", value: why.named)
        default:
            EmptyView()
        }
    }

    /// What a purchase that went through and is not access yet is called. A
    /// purchase amux.sh has taken is confirmed — saying it is not would be
    /// telling somebody their money went nowhere — so that one is headed by
    /// what is actually still happening.
    static func trouble(_ why: PaywallStore.Unconfirmed) -> String {
        switch why {
        case .unreachable, .refused: "Your purchase is not confirmed yet"
        case .switchingOn: "Your subscription is still switching on"
        }
    }

    /// Why a purchase that went through is not this account's yet, and what
    /// happens next. A phone that could not get through will try again on its
    /// own; amux.sh saying no will not change by waiting, so that one says
    /// where to go instead; and one amux.sh has taken is only waiting, so it
    /// says what to do while it waits.
    static func unconfirmed(_ why: PaywallStore.Unconfirmed) -> String {
        switch why {
        case .unreachable:
            "Your purchase is saved, and the app retries confirmation with amux.sh when it next opens."
        case .refused(let said):
            // Ended for us, because the reason is the account service's
            // sentence and nothing makes it finish one.
            "Your purchase is saved, but amux.sh could not confirm it: \(ended(said))"
        case .switchingOn:
            "amux.sh has your purchase and is still turning your subscription on. Tap Retry in a moment to check again."
        }
    }

    /// Somebody else's words, finished. A reason that already ends in a stop
    /// keeps it; one that does not is given one, so it does not run into the
    /// sentence after it.
    static func ended(_ said: String) -> String {
        let trimmed = said.trimmingCharacters(in: .whitespaces)
        guard let last = trimmed.last, !".!?".contains(last) else { return trimmed }
        return trimmed + "."
    }

    private func note(_ headline: String, _ detail: String, id: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(headline)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain(detail)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        // The words as well as the state, because two states can be the same
        // state and still have to say different things — a purchase amux.sh
        // could not be told about and one it refused are both unconfirmed.
        .identified("paywall.\(id)", label: detail, value: value)
    }
}
