import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on a pairing screen. Like every other screen these decide
/// nothing and reach nothing: pairing is the one act in this app that changes
/// what a machine will let this phone do, so no view of it holds a capability
/// or writes trust — it says what happened and whoever owns the connection
/// carries it out.
public enum PairingAction: Equatable, Sendable {
    /// The code as it now stands. A complete one is a code to try.
    case digits(String)
    /// Trust the machine this attempt authenticated.
    case confirm(PendingPair)
    /// Turn it away. Nothing is written on either side.
    case abandon(PendingPair)
    /// Left without an attempt in flight.
    case cancel
    /// Get an account, because the machine the invitation names can only be
    /// reached through the relay and there is no relay without one.
    case signIn
    /// Buy the relay tunnel, because the machine authenticated and this
    /// account may not open one to it.
    case subscribe
}

/// How a machine gets paired, for a screen that has no machine to offer.
///
/// Pairing is always with one particular machine, so it is never a button on
/// its own: a machine on this network is offered by name where it is found,
/// and one that is not found prints a code for the camera. The commands are
/// set in mono because they are typed exactly as written.
public struct PairingHint: View {
    @Environment(\.design) private var design

    public init() {}

    public var body: some View {
        Text("Run \(command("amux pair")) on a host on this network, or scan the code from \(command("amux pair --qr")) with the camera.")
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func command(_ text: String) -> Text {
        Text(verbatim: text).font(design.font(.monoSmall))
    }
}

/// Why nothing on this network can be found, when the reason is that nobody
/// let this app look.
///
/// Said rather than left as an empty list, because the two are
/// indistinguishable on screen and only one of them is fixable: iOS asks once
/// and an app that was refused can never ask again, so the only way back is
/// the system's own settings.
struct LocalNetworkRefused: View {
    @Environment(\.design) private var design
    /// Drawn as the screen's heading when nothing else on it is one.
    let heading: Bool
    let identifier: String
    let openSettings: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("amux can’t see this network")
                .designFont(heading ? .screenTitle : .bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain("""
                Nobody let amux look at the network this phone is on, so no host on it \
                can be found. Turn on Local Network for amux in Settings.
                """)
            Button { openSettings() } label: {
                ActionLabel("Open Settings", kind: .outline)
            }
            .buttonStyle(.plain)
            .identified("\(identifier).settings", label: "Open Settings")
        }
        .accessibilityElement(children: .contain)
        .identified("\(identifier).refused", value: "amux can’t see this network")
    }
}

/// The bar every pairing screen shares: what is being paired and how it is
/// reached, and a way out. Pairing is a short task with an end, so it is a
/// sheet over Hosts, as New Agent is, and closing it at any point writes no
/// trust.
struct PairingChrome: ViewModifier {
    @Environment(\.reducesTransparency) private var reducesTransparency
    let title: String
    let subtitle: String?
    let close: @MainActor () -> Void

    func body(content: Content) -> some View {
        content
            .navigationTitle(title)
            .navigationSubtitle(subtitle ?? "")
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    BarButton("xmark", label: String(localized: "Close"), identifier: "pair.close") { close() }
                }
                .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
            }
    }
}

extension View {
    func pairingChrome(
        _ title: String, subtitle: String? = nil, close: @escaping @MainActor () -> Void
    ) -> some View {
        modifier(PairingChrome(title: title, subtitle: subtitle, close: close))
    }
}

/// How this phone would reach the machine it is pairing with, for the bar
/// under its name. Said because it is the difference between a code that
/// works with no account at all and one carried across the relay: two
/// machines of one name on two routes are otherwise one name.
func pairingRoute(_ machine: HostView?) -> String? {
    guard let machine else { return nil }
    if machine.candidate && machine.reach != .online(.relay) {
        return String(localized: "On this network")
    }
    switch machine.reach {
    case .online(.direct): return String(localized: "On this network")
    case .online(.relay): return String(localized: "Through the relay")
    case .online(.ssh): return String(localized: "Over SSH")
    case .online(.unspecified), .away, .offline: return nil
    }
}

/// A pairing invitation for a machine this phone cannot see.
///
/// An invitation carrying addresses is dialled on the network this phone is
/// already on and needs nothing else. One carrying none names a machine only
/// the relay has ever seen, and there is no relay without an account — so this
/// is not a refusal and not a failure of the invitation: it is the one missing
/// piece, said plainly, with the invitation kept where it was.
public struct PairNeedsAnAccount: View {
    @Environment(\.design) private var design
    private let actions: @MainActor (PairingAction) -> Void

    public init(actions: @escaping @MainActor (PairingAction) -> Void) {
        self.actions = actions
    }

    public var body: some View {
        PairingPanel(
            title: String(localized: "That host is not on this network"),
            detail: String(localized: "Its invitation names no address here, so it can only be reached through the relay. Sign in and this phone can take it up."),
            identifier: "pair-needs-account.title"
        ) {
            Button { actions(.signIn) } label: {
                ActionLabel("Sign In", kind: .primary, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-needs-account.signIn", label: "Sign In")
            Button { actions(.cancel) } label: {
                ActionLabel("Not Now", kind: .outline, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-needs-account.cancel", label: "Not Now")
            Explain(String(localized: "The invitation is kept until then."))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pairingChrome(String(localized: "Pair a Host")) { actions(.cancel) }
        .accessibilityElement(children: .contain)
        .identified("pair-needs-account", value: "not on this network")
    }
}

/// One pairing state as a heading, a sentence under it, and what there is to
/// do at the foot of the sheet. A wait has no buttons and shows the breathing
/// mark beside its heading.
struct PairingPanel<Buttons: View>: View {
    @Environment(\.design) private var design
    let title: String
    let detail: String?
    var waiting = false
    let identifier: String
    @ViewBuilder let buttons: () -> Buttons

    var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    if waiting { StateMark(state: .working) }
                    Text(title)
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .identified(identifier, value: title)
                }
                if let detail {
                    Text(detail)
                        .designFont(.body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, 8)
                }
                Spacer(minLength: 22)
                VStack(spacing: 10) { buttons() }
                    .padding(.bottom, 16)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 16)
        }
    }
}

/// Adding a machine by the six-digit code it printed.
///
/// The code goes into the system's number pad, held up for as long as there
/// is a code to type: it is the screen every phone uses for a code, it
/// pastes one copied on a Mac, and a hardware keyboard, VoiceOver and
/// dictation all reach it. The six boxes are drawn over a field nobody sees,
/// three and three as `amux pair` prints the code, and the code is tried the
/// moment the sixth digit lands.
public struct PairByCode: View {
    @Environment(\.design) private var design
    @FocusState private var typing: Bool
    private let model: PairingStore
    private let actions: @MainActor (PairingAction) -> Void

    public init(model: PairingStore, actions: @escaping @MainActor (PairingAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                heading
                if model.phase == .needsSubscription {
                    subscribe
                } else {
                    boxes
                    caption
                }
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 16)
        }
        .pairingChrome(title, subtitle: pairingRoute(model.machine)) { actions(.cancel) }
        // Up from the first frame, and down only when no code can help.
        .task { typing = model.phase != .needsSubscription }
        .onChange(of: model.phase) { _, phase in typing = phase != .needsSubscription }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("pin", value: model.digits)
    }

    private var title: String {
        guard let machine = model.machine else { return String(localized: "Pair a Host") }
        return String(localized: "Pair \(machine.name)")
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Enter the code")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("pin.title", value: "Enter the code")
            instruction
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("pin.instruction", value: model.machine?.name ?? "")
        }
        .padding(.bottom, 24)
    }

    /// Which machine to run `amux pair` on, by name: a code proves one
    /// machine's offer and is checked against that machine alone, so a code
    /// typed for the laptop and refused by the desktop would otherwise read
    /// as a wrong code.
    private var instruction: Text {
        let command = Text(verbatim: "amux pair").font(design.font(.monoSmall))
        guard let machine = model.machine else { return Text("Run \(command) on the host to get one.") }
        return Text("Run \(command) on \(machine.name) to get one.")
    }

    private var boxes: some View {
        HStack(spacing: 8) {
            group(0..<3)
            Spacer(minLength: 10).frame(maxWidth: 18)
            group(3..<6)
        }
        // The field under the boxes takes the code. It is drawn clear but
        // kept the boxes' size, so a tap anywhere on them brings the pad
        // back, and its text is the store's digits: anything typed or pasted
        // is filtered down to six digits before it is shown.
        .background {
            TextField("", text: Binding(
                get: { model.digits },
                set: { typed in
                    // While a code is with the machine nothing more is
                    // taken: a code sent twice is a code spent twice.
                    if model.taking { actions(.digits(typed)) }
                }))
                .keyboardType(.numberPad)
                .textContentType(.oneTimeCode)
                .focused($typing)
                .foregroundStyle(.clear)
                .tint(.clear)
                .accessibilityLabel(String(localized: "Code"))
                .accessibilityValue(spokenDigits)
                .identified("pin.code", label: String(localized: "Code"), value: model.digits)
        }
        .contentShape(Rectangle())
        .onTapGesture { typing = true }
    }

    private func group(_ indices: Range<Int>) -> some View {
        HStack(spacing: 8) {
            ForEach(indices, id: \.self) { box($0) }
        }
        .accessibilityHidden(true)
    }

    private func box(_ index: Int) -> some View {
        let digits = Array(model.digits)
        let digit = digits.count > index ? String(digits[index]) : ""
        // The box the next digit lands in is outlined in the accent while the
        // pad is up; the field's own caret is hidden.
        let next = index == digits.count && model.taking && typing
        let shape = RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
        return Text(digit)
            .font(.system(size: 28, weight: .medium))
            .monospacedDigit()
            .foregroundStyle(design.ink.color)
            .frame(maxWidth: .infinity)
            .frame(height: 58)
            .background {
                ZStack {
                    shape.fill(design.raised.color)
                    shape.strokeBorder(
                        next ? design.accent.color : design.hairline.color,
                        lineWidth: next ? 1.5 : 1)
                }
            }
    }

    /// The code read out as digits rather than as a number: "419" is four one
    /// nine, not four hundred and nineteen.
    private var spokenDigits: String {
        model.digits.isEmpty
            ? String(localized: "empty") : model.digits.map(String.init).joined(separator: " ")
    }

    /// Under the boxes: when the offer runs out, that the code is with the
    /// machine, or the one thing said about a code that did not work.
    @ViewBuilder
    private var caption: some View {
        Group {
            switch model.phase {
            case .refused:
                // One sentence for a mistyped code, an expired code, a code
                // already used and a code nobody issued. Telling them apart
                // is exactly what somebody guessing codes would want, so the
                // screen does not, and the digits have already gone.
                Text(refused)
                    .designFont(.detail, design)
                    .foregroundStyle(design.removed.color)
                    .identified("pin.refused", value: refused)
            case .checking:
                HStack(spacing: 8) {
                    StateMark(state: .working)
                    Text("Checking…")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                .identified("pin.checking", value: "Checking…")
            default:
                Text(expiry)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("pin.expiry", value: PairingStore.offerWindow)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.top, 12)
    }

    private var refused: String {
        guard let machine = model.machine else {
            return String(localized: "That code didn’t work. Get a new one from the host.")
        }
        return String(localized: "That code didn’t work. Get a new one from \(machine.name).")
    }

    private var expiry: String {
        let host = model.machine?.name ?? String(localized: "the host")
        return String(localized: "The code expires \(PairingStore.offerWindow) after \(host) prints it.")
    }

    /// The code was right and the route is missing: the machine is on the far
    /// side of the relay and this account may not open a tunnel to it, which
    /// no number typed here can change. So the pad goes away and the offer
    /// stands in its place.
    private var subscribe: some View {
        VStack(alignment: .leading, spacing: 14) {
            SubscribeCallToAction(
                host: model.machine?.name, identifier: "pin.subscribe"
            ) { actions(.subscribe) }
            Button { actions(.cancel) } label: {
                ActionLabel("Not Now", kind: .outline, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("pin.notNow", label: "Not Now")
        }
    }
}

/// The machine that answered, and the decision nobody but a person can make.
///
/// This is the whole point of pairing being two acts rather than one. Reaching
/// this screen means a secret authenticated — over a link or over a typed code
/// — and it means nothing else: no trust has been written on this phone or on
/// the machine, and leaving now writes none. The code or the link is what
/// proves this is the machine that started pairing; the screen names it and
/// how it would be reached, so the person agrees to the machine they meant,
/// and keeps its key underneath for anybody who wants to compare it.
public struct PairConfirmation: View {
    @Environment(\.design) private var design
    private let model: PairingStore
    private let actions: @MainActor (PairingAction) -> Void

    public init(model: PairingStore, actions: @escaping @MainActor (PairingAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        Group {
            switch model.phase {
            case .confirming(let peer): offer(peer)
            case .trusted(let name): settled(name)
            case .refused: refused
            case .needsSubscription: needsSubscription
            default: checking
            }
        }
        .pairingChrome(title, subtitle: subtitle) { actions(.cancel) }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("pair-confirm", value: state)
    }

    private var name: String? {
        switch model.phase {
        case .confirming(let peer): peer.name
        case .trusted(let name): name
        default: model.machine?.name
        }
    }

    private var title: String {
        guard let name else { return String(localized: "Pair a Host") }
        return String(localized: "Pair \(name)")
    }

    private var subtitle: String? {
        if case .confirming(let peer) = model.phase { return Self.route(peer.via) }
        return pairingRoute(model.machine)
    }

    static func route(_ via: RouteKind) -> String? {
        switch via {
        case .direct: String(localized: "On this network")
        case .relay: String(localized: "Through the relay")
        case .ssh: String(localized: "Over SSH")
        case .unspecified: nil
        }
    }

    private var state: String {
        switch model.phase {
        case .confirming(let peer): peer.name
        case .trusted(let name): "trusted \(name)"
        case .refused: "refused"
        case .needsSubscription: "needs subscription"
        default: "checking"
        }
    }

    /// The invitation was good and the machine is only on the far side of the
    /// relay. Nothing about the invitation can be retried into working, so the
    /// screen offers the one thing that would.
    private var needsSubscription: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 14) {
                SubscribeCallToAction(
                    host: name, identifier: "pair-confirm.subscribe"
                ) { actions(.subscribe) }
                Button { actions(.cancel) } label: {
                    ActionLabel("Not Now", kind: .outline, fill: true)
                }
                .buttonStyle(.amuxControl)
                .identified("pair-confirm.notNow", label: "Not Now")
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 16)
        }
    }

    private func offer(_ peer: PendingPair) -> some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                Text("Pair with \(peer.name)?")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("pair-confirm.title", value: "Pair with \(peer.name)?")
                Text(about(peer))
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 6)
                    .identified("pair-confirm.name", value: peer.name)
                Explain(String(localized: "Its agents will be on this phone, and you can start new ones on it. This invitation expires \(expiry(peer))."))
                    .padding(.top, 14)
                    .identified("pair-confirm.expiry", value: expiry(peer))
                Spacer(minLength: 22)
                // The key, for anybody who wants to check it against the
                // machine's: whole, because a fingerprint with its middle
                // taken out is not one anybody can check.
                VStack(alignment: .leading, spacing: 4) {
                    Text("Fingerprint")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                    Text(Fingerprint.grouped(peer.fingerprint))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .combine)
                .identified(
                    "pair-confirm.fingerprint", label: String(localized: "Fingerprint"),
                    value: Fingerprint.grouped(peer.fingerprint))
                .padding(.bottom, 18)
                VStack(spacing: 10) {
                    Button { actions(.confirm(peer)) } label: {
                        ActionLabel("Pair", kind: .primary, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("pair-confirm.trust", label: "Pair")
                    Button { actions(.abandon(peer)) } label: {
                        ActionLabel("Not Now", kind: .outline, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("pair-confirm.abandon", label: "Not Now")
                }
                .padding(.bottom, 16)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 16)
        }
    }

    /// "macOS · on this network": what the machine is, where this
    /// phone knows it, and how it would be reached.
    private func about(_ peer: PendingPair) -> String {
        var parts: [String] = []
        if let machine = model.machine, machine.hostId == peer.hostId, let platform = machine.platform?.name {
            parts.append(platform)
        }
        switch peer.via {
        case .direct: parts.append(String(localized: "on this network"))
        case .relay: parts.append(String(localized: "through the relay"))
        case .ssh: parts.append(String(localized: "over SSH"))
        case .unspecified: break
        }
        return parts.joined(separator: " · ")
    }

    /// While the invitation is being authenticated. A link opens straight onto
    /// this, and it is not a screen that pairs: it is the wait before the
    /// machine has said who it is.
    private var checking: some View {
        PairingPanel(
            title: String(localized: "Checking the invitation…"),
            detail: String(localized: "Nothing is trusted yet."), waiting: true,
            identifier: "pair-confirm.checking"
        ) { EmptyView() }
    }

    private var refused: some View {
        PairingPanel(
            title: String(localized: "That invitation didn’t work"),
            detail: String(localized: "Get a new one from the host."),
            identifier: "pair-confirm.refused"
        ) {
            Button { actions(.cancel) } label: {
                ActionLabel("Close", kind: .outline, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-confirm.back", label: "Close")
        }
    }

    /// Drawn for the moment between the machine agreeing and the sheet
    /// going: the sheet closes on its own once the pairing is written.
    private func settled(_ name: String) -> some View {
        PairingPanel(
            title: String(localized: "\(name) is paired"),
            detail: String(localized: "Its agents are on the Agents tab."),
            identifier: "pair-confirm.trusted"
        ) { EmptyView() }
    }

    /// When the offer runs out, as a length of time rather than a clock face:
    /// the machine started it, so what matters is how long is left.
    private func expiry(_ peer: PendingPair) -> String {
        let left = Date(milliseconds: peer.expiresAtMs).timeIntervalSince(model.now)
        return left > 0 ? String(localized: "in \(Elapsed.spelled(left))") : String(localized: "shortly")
    }
}
