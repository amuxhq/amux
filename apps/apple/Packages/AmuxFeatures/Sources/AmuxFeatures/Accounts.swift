import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did about an account.
public enum AccountsAction: Equatable, Sendable {
    case select(AccountId)
    case add
    /// Sign back into an account this phone still remembers.
    case signIn(AccountId)
    case signOut(AccountId)
    /// Ask about taking an account off this phone, leaving it on amux.sh.
    case remove(AccountId)
    /// Take it off, once asked. It names the account, because the system's
    /// dialog may report being put away before the press it was put away by.
    case confirmRemoval(AccountId)
    case cancelRemoval
    case delete(AccountId)
    /// Open what this account has bought.
    case subscription
    case appearance(Appearance?)
    /// Turn sending amux.sh usage events on or off.
    case shareUsage(Bool)
    case support
    case report
    /// Write this phone's diagnostic dump and hand it to the share sheet.
    case exportDump
}

/// One account, as a row.
///
/// The second line is what this phone actually knows: how many machines the
/// account reaches where there is a connection to have counted them, that it
/// is signed out where it is, and the address otherwise. Nothing here is
/// inferred — an account nobody has connected to says its own name and no more.
struct AccountRow: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let selected: Bool
    let actions: @MainActor (AccountsAction) -> Void

    var body: some View {
        Button { actions(entry.signedIn ? .select(entry.id) : .signIn(entry.id)) } label: {
            HStack(spacing: 12) {
                InitialsDisc(entry: entry, filled: selected)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.name)
                        .designFont(.body, design)
                        .foregroundStyle(entry.signedIn ? design.ink.color : design.inkMuted.color)
                        .lineLimit(1)
                    Text(entry.line)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                trailing
            }
            .padding(.vertical, 11)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
        .identified(
            "account.\(entry.account.email)",
            label: "\(entry.name), \(entry.line)",
            value: selected ? "selected" : entry.signedIn ? "signed in" : "signed out")
    }

    @ViewBuilder
    private var trailing: some View {
        if selected {
            Image(systemName: "checkmark")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(design.accent.color)
                .accessibilityHidden(true)
        } else if !entry.signedIn {
            Text("Sign In")
                .designFont(.caption, design)
                .foregroundStyle(design.accent.color)
        } else if let waiting = entry.attention, waiting > 0 {
            // Only ever drawn from a count something actually reported. An
            // account this phone has no connection to says nothing here.
            Text("\(waiting)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.onAccent.color)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(Capsule().fill(design.accent.color))
                .accessibilityLabel("\(waiting) need you")
                // Named in its own right: the row's own name carries what the
                // row says about the account, and the number is a fact about
                // somewhere else that changes while the row does not.
                .identified(
                    "account.\(entry.account.email).waiting",
                    label: "\(waiting) need you", value: "\(waiting)")
        }
    }
}

/// What an account is recognised by before its name is read: its profile
/// picture where it has one, and two letters until that arrives or where it
/// has none.
struct InitialsDisc: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let filled: Bool
    var size: CGFloat = 30

    var body: some View {
        Text(Self.initials(of: entry.name))
            .font(.system(size: size * 0.36, weight: .semibold))
            .foregroundStyle(filled ? design.onAccent.color : design.inkMuted.color)
            .frame(width: size, height: size)
            .background(Circle().fill(filled ? design.accent.color : design.sunken.color))
            .overlay { AccountPicture(url: entry.account.picture, size: size) }
            .opacity(entry.signedIn ? 1 : 0.5)
            .accessibilityHidden(true)
    }

    /// Two letters, because one is not enough to tell two accounts apart at a
    /// glance and three stops reading as a monogram. A name of two words gives
    /// one letter from each; a single word gives its first two.
    static func initials(of name: String) -> String {
        let words = name.split(separator: " ").prefix(2)
        guard let first = words.first else { return "?" }
        let letters = words.count > 1
            ? words.compactMap(\.first).map(String.init).joined()
            : String(first.prefix(2))
        return letters.uppercased()
    }
}

/// An account's profile picture as a circle, drawn over whatever stands in
/// for it: nothing at all until it has loaded, or where it fails to.
struct AccountPicture: View {
    let url: URL?
    let size: CGFloat

    var body: some View {
        if let url {
            AsyncImage(url: url) { phase in
                if let image = phase.image {
                    image.resizable().scaledToFill()
                }
            }
            .frame(width: size, height: size)
            .clipShape(Circle())
            .accessibilityHidden(true)
        }
    }
}

/// You: the accounts this phone knows, what the one on screen has, and the
/// things that belong to the phone rather than to any account.
///
/// One page rather than a list of screens. Appearance is a control on its own
/// row instead of a value behind a page, support sits with reporting because
/// "something is wrong" is one intent with two exits, and an account's own
/// actions sit under the list of accounts because they are that list's
/// selected row continued.
public struct YouScreen: View {
    @Environment(\.design) private var design
    private let accounts: AccountRegistry
    private let appearance: Appearance?
    private let shareUsage: Bool
    private let identity: Identity?
    /// The account being asked about removing, if one is.
    private let removing: AccountId?
    private let actions: @MainActor (AccountsAction) -> Void

    public init(
        accounts: AccountRegistry,
        appearance: Appearance? = nil,
        shareUsage: Bool = true,
        identity: Identity? = nil,
        removing: AccountId? = nil,
        actions: @escaping @MainActor (AccountsAction) -> Void
    ) {
        self.accounts = accounts
        self.appearance = appearance
        self.shareUsage = shareUsage
        self.identity = identity
        self.removing = removing
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    accountList
                    if let entry = accounts.selectedAccount { account(entry) }
                    phone
                    help
                }
                .padding(.horizontal, design.metrics.gutter)
                .padding(.bottom, 34)
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them: without this every row underneath answers to the screen's name.
        .accessibilityElement(children: .contain)
        .identified("you", value: accounts.selectedAccount?.account.email ?? "none")
    }

    private var accountList: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Accounts")
            VStack(spacing: 0) {
                ForEach(accounts.accounts) { entry in
                    let selected = entry.id == accounts.selected
                    AccountRow(entry: entry, selected: selected, actions: actions)
                        .padding(.horizontal, 14)
                        // The selected account is asked about from its own
                        // section's row below; any other, from its row here.
                        .removalDialog(entry, asking: !selected && removing == entry.id, actions: actions)
                        // On the row itself, because the account's own section
                        // below is only ever the selected account's, and an
                        // account signed out of is the one most likely to be
                        // taken off a phone.
                        .contextMenu {
                            Button(role: .destructive) { actions(.remove(entry.id)) } label: {
                                Label("Remove from This Phone", systemImage: "minus.circle")
                            }
                        }
                        .accessibilityAction(named: "Remove from This Phone") {
                            actions(.remove(entry.id))
                        }
                    rule(inset: 14)
                }
                if accounts.accounts.isEmpty {
                    signIn
                } else {
                    Button { actions(.add) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "plus")
                                .font(.system(size: 14, weight: .semibold))
                                .frame(width: 30)
                            Text("Add Account")
                                .designFont(.body, design)
                            Spacer(minLength: 0)
                        }
                        .foregroundStyle(design.accent.color)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 13)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.amuxRow)
                    .identified("you.add", label: "Add Account")
                }
            }
        }
    }

    /// The first account, offered where the accounts would be. A phone with
    /// nobody signed in is not a broken phone — it pairs and runs agents on
    /// this network — so it is said as what an account adds rather than as
    /// something missing. An account this phone remembers is signed back into
    /// from its own row instead.
    private var signIn: some View {
        Button { actions(.add) } label: {
            HStack(spacing: 12) {
                Image(systemName: "person.crop.circle")
                    .font(.system(size: 18, weight: .regular))
                    .foregroundStyle(design.accent.color)
                    .frame(width: 30)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Sign In")
                        .designFont(.body, design)
                        .foregroundStyle(design.accent.color)
                    Text("Reach your agents from anywhere")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .accessibilityElement(children: .combine)
        .identified("you.signIn", label: "Sign In")
    }

    /// The selected account's own actions, under the list they continue.
    private func account(_ entry: AccountEntry) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: entry.name)
            VStack(spacing: 0) {
                // Headed by what it buys rather than by what it is called.
                // A subscription is the only thing anybody pays amux for and
                // the relay is the only thing it carries, so the row that
                // reports one says which: an account with nothing bought is
                // not an account with nothing working.
                row(
                    "Relay", value: entry.entitlement.summary, id: "subscription"
                ) {
                    actions(.subscription)
                }
                rule()
                action("Sign Out", id: "signOut") { actions(.signOut(entry.id)) }
                rule()
                action("Remove from This Phone", id: "remove") { actions(.remove(entry.id)) }
                    .removalDialog(entry, asking: removing == entry.id, actions: actions)
                rule()
                // The one destructive thing on the page, in the colour this
                // app keeps for exactly that.
                action("Delete Account", id: "delete", danger: true) {
                    actions(.delete(entry.id))
                }
            }
        }
    }

    private var phone: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "This phone")
            VStack(spacing: 0) {
                appearanceRow
                rule(inset: 14)
                shareUsageRow
                // What the machines this phone pairs with know it by: its
                // name, and its key in full to compare against the one a
                // machine shows. Facts, not places to go: revoking a device
                // happens on the machine, and a machine is revoked from its
                // own page on Hosts.
                if let identity {
                    rule(inset: 14)
                    FieldRow(label: String(localized: "Name"), value: identity.name, chevron: false)
                        .accessibilityElement(children: .combine)
                        .identified("you.name", label: String(localized: "Name"), value: identity.name)
                    rule(inset: 14)
                    fingerprint(identity.fingerprint)
                }
            }
        }
    }

    /// Appearance is the value, not a page holding it: three words, and the
    /// screen changes under your thumb.
    /// When the label and the three words do not fit on one line, as at
    /// the largest text sizes, the words go under the label, and one under
    /// another if even that is too narrow: a word is never broken.
    private var appearanceRow: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                appearanceLabel
                Spacer(minLength: 8)
                appearanceChoices
            }
            VStack(alignment: .leading, spacing: 8) {
                appearanceLabel
                appearanceChoices
            }
            VStack(alignment: .leading, spacing: 8) {
                appearanceLabel
                VStack(alignment: .leading, spacing: 4) {
                    choice("Light", .light)
                    choice("Dark", .dark)
                    choice("System", nil)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .accessibilityElement(children: .contain)
        .identified("you.appearance", value: appearance?.rawValue ?? "system")
    }

    private var appearanceLabel: some View {
        Text("Appearance")
            .designFont(.body, design)
            .foregroundStyle(design.ink.color)
            .fixedSize()
    }

    private var appearanceChoices: some View {
        HStack(spacing: 0) {
            choice("Light", .light)
            choice("Dark", .dark)
            choice("System", nil)
        }
        .padding(2)
        .background(Capsule().fill(design.sunken.color))
    }

    /// A switch, not a page: what it sends is described on amux.sh, and the
    /// caption says what it is in a line.
    private var shareUsageRow: some View {
        let isOn = Binding(get: { shareUsage }, set: { actions(.shareUsage($0)) })
        return Toggle(isOn: isOn) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Share Usage")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Text("Counts and choices, never what you type")
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
            }
        }
        .tint(design.accent.color)
        .oneSwitch(String(localized: "Share Usage, Counts and choices, never what you type"), isOn: isOn)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .identified("you.shareUsage", value: shareUsage ? "on" : "off")
    }

    private func choice(_ title: String, _ wanted: Appearance?) -> some View {
        let chosen = appearance == wanted
        return Button { actions(.appearance(wanted)) } label: {
            Text(title)
                .designFont(.caption, design)
                .foregroundStyle(chosen ? design.ground.color : design.inkMuted.color)
                .fixedSize()
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background {
                    if chosen { Capsule().fill(design.ink.color) }
                }
                .thumbTarget(y: 9)
        }
        .buttonStyle(.amuxControl)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "you.appearance.\(wanted?.rawValue ?? "system")", label: title,
            value: chosen ? "chosen" : "not chosen")
        .reclaimingThumbTarget(y: 9)
    }

    private var help: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Help")
            VStack(spacing: 0) {
                glyphRow("Contact Support", glyph: "envelope", id: "support") {
                    actions(.support)
                }
                rule(inset: 14)
                glyphRow("Report a Problem", glyph: "ladybug", id: "report") {
                    actions(.report)
                }
                rule(inset: 14)
                glyphRow("Export Diagnostics", glyph: "square.and.arrow.up", id: "dump") {
                    actions(.exportDump)
                }
            }
            Text("A report includes a screenshot and this phone’s diagnostic dump.")
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 14)
                .identified("you.help.caption")
        }
    }

    // MARK: - Rows

    private func fingerprint(_ key: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Fingerprint")
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
            Text(Fingerprint.grouped(key))
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
        .identified("you.fingerprint", label: String(localized: "Fingerprint"), value: key)
    }

    private func row(
        _ title: String, value: String, id: String, mono: Bool = false,
        press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            FieldRow(label: title, value: value, mono: mono)
        }
        .buttonStyle(.amuxPush)
        .accessibilityElement(children: .combine)
        .identified("you.\(id)", label: title, value: value)
    }

    private func action(
        _ title: String, id: String, danger: Bool = false,
        press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            FieldRow(
                label: title,
                chevron: false,
                tint: danger ? design.removed.color : design.accent.color)
        }
        .buttonStyle(.amuxRow)
        .identified("you.\(id)", label: title)
    }

    private func glyphRow(
        _ title: String, glyph: String, id: String, press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            FieldRow(label: title, glyph: glyph)
        }
        .buttonStyle(.amuxRow)
        .accessibilityElement(children: .combine)
        .identified("you.\(id)", label: title)
    }

    private func rule(inset: CGFloat = 0) -> some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, inset)
    }
}

extension View {
    /// Asks before an account is taken off this phone, in the system's dialog
    /// held by the row it was asked from, so it points there.
    ///
    /// It sits beside Delete Account and must never be mistaken for it, so
    /// what stays is said first: the account on amux.sh, what it pays for, and
    /// every agent on every host are untouched. What goes is this phone's own
    /// part — the sign-in, and the hosts it paired for that account, which are
    /// paired again if the account comes back. Nothing is typed to confirm
    /// it, because nothing here cannot be put back by signing in.
    func removalDialog(
        _ entry: AccountEntry, asking: Bool, actions: @escaping @MainActor (AccountsAction) -> Void
    ) -> some View {
        confirmationDialog(
            String(localized: "Remove \(entry.account.email) from this phone?"),
            isPresented: Binding(get: { asking }, set: { if !$0, asking { actions(.cancelRemoval) } }),
            titleVisibility: .visible
        ) {
            Button(String(localized: "Remove"), role: .destructive) { actions(.confirmRemoval(entry.id)) }
                .accessibilityIdentifier("remove.confirm")
        } message: {
            Text("Your amux.sh account, its subscription, and your agents and files on your hosts stay as they are. This phone signs out and forgets the hosts it paired for this account.")
        }
    }
}
