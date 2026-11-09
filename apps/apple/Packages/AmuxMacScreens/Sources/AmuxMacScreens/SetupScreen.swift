import AmuxValues
import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

/// The pairing window as the screen shows it.
public struct PairingContent {
    public var window: PairingWindowView?
    /// Why no window opened.
    public var failed: String?
    public var joining: Bool
    public var lockedOut: Bool

    public init(
        window: PairingWindowView? = nil, failed: String? = nil, joining: Bool = false,
        lockedOut: Bool = false
    ) {
        self.window = window
        self.failed = failed
        self.joining = joining
        self.lockedOut = lockedOut
    }
}

/// A folder offered as a repository, ticked to keep.
public struct SetupRepository: Identifiable, Equatable {
    public let path: String
    public var ticked: Bool
    public var id: String { path }

    public init(path: String, ticked: Bool) {
        self.path = path
        self.ticked = ticked
    }
}

/// What the person has chosen so far on the setting-up screens.
public struct SetupChoices {
    public var startAtLogin: Bool
    public var keepAwake: Bool
    public var alsoTicked: Bool
    public var repositories: [SetupRepository]
    public var autoReports: Bool

    public init(
        startAtLogin: Bool = true, keepAwake: Bool = true, alsoTicked: Bool = false,
        repositories: [SetupRepository] = [], autoReports: Bool = false
    ) {
        self.startAtLogin = startAtLogin
        self.keepAwake = keepAwake
        self.alsoTicked = alsoTicked
        self.repositories = repositories
        self.autoReports = autoReports
    }
}

/// What a setting-up screen shows.
public struct SetupContent {
    public var step: SetupStep
    /// This Mac's name; nil until the daemon has said.
    public var machineName: String?
    /// Whether this installation can start at login.
    public var canStartAtLogin: Bool
    public var error: String?
    public var busy: Bool
    public var signingIn: Bool
    public var accepting: Bool
    public var pairing: PairingContent
    /// The phone this run paired.
    public var paired: PairedDevice?
    /// The phone just paired, still signed out, to offer the account too.
    public var also: PairedDevice?
    /// A paired machine's offer of its account.
    public var offer: SignInOfferView?
    public var repositoriesLoaded: Bool
    /// The name being typed when the screen opens renaming this Mac.
    public var renaming: String?

    public init(
        step: SetupStep, machineName: String? = nil, canStartAtLogin: Bool = true,
        error: String? = nil, busy: Bool = false, signingIn: Bool = false, accepting: Bool = false,
        pairing: PairingContent = PairingContent(), paired: PairedDevice? = nil,
        also: PairedDevice? = nil, offer: SignInOfferView? = nil, repositoriesLoaded: Bool = true,
        renaming: String? = nil
    ) {
        self.step = step
        self.machineName = machineName
        self.canStartAtLogin = canStartAtLogin
        self.error = error
        self.busy = busy
        self.signingIn = signingIn
        self.accepting = accepting
        self.pairing = pairing
        self.paired = paired
        self.also = also
        self.offer = offer
        self.repositoriesLoaded = repositoriesLoaded
        self.renaming = renaming
    }
}

/// What the setting-up screens can ask for.
public struct SetupActions {
    public var later: @MainActor () -> Void
    public var confirmReachable: @MainActor () -> Void
    public var confirmRepositories: @MainActor () -> Void
    public var confirmImprove: @MainActor () -> Void
    public var signIn: @MainActor () -> Void
    public var declineOffer: @MainActor () -> Void
    public var newCode: @MainActor () -> Void
    public var rename: @MainActor (String) -> Void
    public var addFolders: @MainActor ([URL]) -> Void
    public var pairingShown: @MainActor () -> Void
    public var pairingHidden: @MainActor () -> Void

    public init(
        later: @escaping @MainActor () -> Void = {},
        confirmReachable: @escaping @MainActor () -> Void = {},
        confirmRepositories: @escaping @MainActor () -> Void = {},
        confirmImprove: @escaping @MainActor () -> Void = {},
        signIn: @escaping @MainActor () -> Void = {},
        declineOffer: @escaping @MainActor () -> Void = {},
        newCode: @escaping @MainActor () -> Void = {},
        rename: @escaping @MainActor (String) -> Void = { _ in },
        addFolders: @escaping @MainActor ([URL]) -> Void = { _ in },
        pairingShown: @escaping @MainActor () -> Void = {},
        pairingHidden: @escaping @MainActor () -> Void = {}
    ) {
        self.later = later
        self.confirmReachable = confirmReachable
        self.confirmRepositories = confirmRepositories
        self.confirmImprove = confirmImprove
        self.signIn = signIn
        self.declineOffer = declineOffer
        self.newCode = newCode
        self.rename = rename
        self.addFolders = addFolders
        self.pairingShown = pairingShown
        self.pairingHidden = pairingHidden
    }
}

/// Setting up, one step a screen, in the window: a symbol, the step's title
/// and what it is for, its choices, and Later beside the step's own action.
/// Escape is Later and Return the step's action, as on the terminal.
public struct SetupScreen: View {
    let content: SetupContent
    @Binding var choices: SetupChoices
    let actions: SetupActions

    public init(content: SetupContent, choices: Binding<SetupChoices>, actions: SetupActions = SetupActions()) {
        self.content = content
        _choices = choices
        self.actions = actions
    }

    public var body: some View {
        let step = content.step
        VStack(spacing: 0) {
            VStack(spacing: 10) {
                Image(systemName: symbol(step))
                    .font(.system(size: 46, weight: .light))
                    .foregroundStyle(.tint)
                    .symbolRenderingMode(.hierarchical)
                    .frame(height: 56)
                    .padding(.bottom, 6)
                Text(title(step))
                    .font(.system(size: 26, weight: .bold))
                    .multilineTextAlignment(.center)
            }
            .padding(.top, 44)
            stepContent(step)
                .frame(maxWidth: 560)
                .padding(.horizontal, 40)
                .padding(.top, 22)
            if let error = content.error {
                Text(error)
                    .foregroundStyle(.red)
                    .font(.callout)
                    .multilineTextAlignment(.center)
                    .textSelection(.enabled)
                    .padding(.horizontal, 40)
                    .padding(.top, 12)
            }
            Spacer(minLength: 16)
            Buttons(content: content, actions: actions)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .id(step)
        .transition(.opacity)
    }

    @ViewBuilder
    private func stepContent(_ step: SetupStep) -> some View {
        switch step {
        case .pair_phone: PairPhone(content: content, actions: actions)
        case .keep_reachable: KeepReachable(content: content, choices: $choices)
        case .sign_in: SignIn(content: content, choices: $choices)
        case .repositories: Repositories(content: content, choices: $choices, actions: actions)
        case .improve: Improve(choices: $choices)
        case .first_agent: FirstAgent()
        }
    }

    private func symbol(_ step: SetupStep) -> String {
        switch step {
        case .pair_phone: "laptopcomputer.and.iphone"
        case .keep_reachable: "powerplug"
        case .sign_in: "globe"
        case .repositories: "folder"
        case .improve: "chart.bar"
        case .first_agent: "iphone"
        }
    }

    /// What a step is called on its screen, in the terminal's words.
    private func title(_ step: SetupStep) -> String {
        switch step {
        case .pair_phone: "Connect your iPhone"
        case .keep_reachable: "Keep it connected"
        case .sign_in: "Reach it from anywhere"
        case .repositories: "Repositories"
        case .improve: "Help improve amux"
        case .first_agent: "Try starting an agent from your phone"
        }
    }
}

/// Later on the left, the step's own action on the right.
private struct Buttons: View {
    let content: SetupContent
    let actions: SetupActions

    var body: some View {
        HStack(spacing: 12) {
            Button("Later") { actions.later() }
                .keyboardShortcut(.cancelAction)
                .disabled(content.busy)
            Spacer()
            if content.busy || content.signingIn || content.accepting {
                ProgressView().controlSize(.small)
            }
            primary
        }
        .controlSize(.large)
        .padding(.horizontal, 24)
        .padding(.vertical, 18)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }

    @ViewBuilder
    private var primary: some View {
        switch content.step {
        case .pair_phone, .first_agent:
            EmptyView()
        case .keep_reachable:
            Button("Continue") { actions.confirmReachable() }
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy)
        case .sign_in:
            if content.offer != nil {
                Button("Not Now") { actions.declineOffer() }
                    .disabled(content.accepting)
            }
            Button(content.offer == nil ? "Sign In…" : "Sign In") { actions.signIn() }
                .keyboardShortcut(.defaultAction)
                .disabled(content.signingIn || content.accepting)
        case .repositories:
            Button("Continue") { actions.confirmRepositories() }
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy || !content.repositoriesLoaded)
        case .improve:
            Button("Continue") { actions.confirmImprove() }
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy)
        }
    }
}

// MARK: - Connect your iPhone

private struct PairPhone: View {
    let content: SetupContent
    let actions: SetupActions

    var body: some View {
        VStack(spacing: 26) {
            MachineName(content: content, actions: actions)
            HStack(alignment: .center, spacing: 36) {
                VStack(alignment: .leading, spacing: 18) {
                    Text("Scan with your iPhone's camera to pair.")
                    pinLine
                    status
                    Text("Whoever scans this code or enters the PIN can run agents on this Mac as you.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .font(.title3)
                .frame(maxWidth: .infinity, alignment: .leading)
                code
            }
        }
        .onAppear { actions.pairingShown() }
        .onDisappear { actions.pairingHidden() }
    }

    @ViewBuilder
    private var pinLine: some View {
        if let pin = content.pairing.window?.pin, !pin.isEmpty {
            Text("Or enter \(Text(spaced(pin)).monospacedDigit().fontWeight(.semibold)) in the amux app.")
                .textSelection(.enabled)
                .accessibilityLabel("Or enter \(pin) in the amux app.")
        } else {
            Text("Or enter the code in the amux app.")
        }
    }

    @ViewBuilder
    private var status: some View {
        let pairing = content.pairing
        if let why = pairing.failed {
            Label("Pairing could not open: \(why)", systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
                .font(.callout)
        } else if pairing.lockedOut {
            HStack(spacing: 10) {
                Text("Closed after too many wrong codes.").foregroundStyle(.orange)
                Button("New Code") { actions.newCode() }
            }
            .font(.callout)
        } else if pairing.joining {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Your iPhone is pairing…")
            }
            .font(.callout)
        } else if pairing.window != nil {
            Text("Waiting for your iPhone…").font(.callout).foregroundStyle(.secondary)
        } else {
            Text("Opening pairing…").font(.callout).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var code: some View {
        if let link = content.pairing.window?.qrLink, !link.isEmpty {
            QRCode(link: link)
                .frame(width: 196, height: 196)
                .accessibilityLabel("A code that opens amux on your iPhone, or the App Store")
        } else {
            RoundedRectangle(cornerRadius: 14)
                .fill(.quaternary)
                .frame(width: 196, height: 196)
                .overlay { ProgressView() }
        }
    }

    /// A PIN as people read it aloud: two groups of three.
    private func spaced(_ pin: String) -> String {
        guard pin.count == 6 else { return pin }
        return "\(pin.prefix(3)) \(pin.suffix(3))"
    }
}

/// "This Mac is called …", with Rename: the name every paired device sees.
private struct MachineName: View {
    let content: SetupContent
    let actions: SetupActions
    @State private var editing: String?
    @FocusState private var focused: Bool

    init(content: SetupContent, actions: SetupActions) {
        self.content = content
        self.actions = actions
        _editing = State(initialValue: content.renaming)
    }

    var body: some View {
        HStack(spacing: 6) {
            Text("This Mac is called").foregroundStyle(.secondary)
            if let binding = Binding($editing) {
                TextField("Name", text: binding)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
                    .focused($focused)
                    .onSubmit(save)
                    .onExitCommand { editing = nil }
                    .onAppear { focused = true }
                Button("Save", action: save)
            } else {
                Text(content.machineName ?? "this Mac").fontWeight(.semibold)
                Button("Rename…") { editing = content.machineName ?? "" }
                    .buttonStyle(.link)
                    .padding(.leading, 4)
            }
        }
        .font(.title3)
    }

    private func save() {
        if let name = editing { actions.rename(name) }
        editing = nil
    }
}

/// The pairing link as a QR code, drawn by Core Image eight pixels to a
/// module and scaled to fit without smoothing, black on white whatever the
/// appearance, so a phone's camera reads it. Drawn a module to a pixel, a
/// renderer that smooths anyway (as offscreen rendering does) blurs every
/// edge; at eight the smoothing is a sliver.
private struct QRCode: View {
    let link: String

    var body: some View {
        if let image = Self.image(link) {
            Image(decorative: image, scale: 1)
                .interpolation(.none)
                .resizable()
                .scaledToFit()
                .padding(12)
                .background(.white, in: RoundedRectangle(cornerRadius: 14))
                .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(.separator))
        }
    }

    private static let context = CIContext()

    static func image(_ text: String) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?
            .transformed(by: CGAffineTransform(scaleX: 8, y: 8))
        else { return nil }
        return context.createCGImage(output, from: output.extent)
    }
}

// MARK: - Keep it connected

private struct KeepReachable: View {
    let content: SetupContent
    @Binding var choices: SetupChoices

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let phone = content.paired {
                Label {
                    Text("Paired with \(phone.name)")
                } icon: {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                .font(.title3.weight(.medium))
                .frame(maxWidth: .infinity)
                .padding(.bottom, 6)
            }
            Text("So your phone reaches \(content.machineName ?? "this Mac") whenever you need it:")
                .font(.title3)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 10) {
                if content.canStartAtLogin {
                    Toggle("Start amux when you log in", isOn: $choices.startAtLogin)
                }
                Toggle("Keep this Mac awake while it’s plugged in", isOn: $choices.keepAwake)
            }
            .toggleStyle(.checkbox)
            .font(.title3)
            .padding(.leading, 4)
            Text("On battery it stays awake only while an agent works.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Reach it from anywhere

private struct SignIn: View {
    let content: SetupContent
    @Binding var choices: SetupChoices

    var body: some View {
        let name = content.machineName ?? "this Mac"
        VStack(alignment: .leading, spacing: 16) {
            if let offer = content.offer {
                Text("Sign in as \(offer.email.isEmpty ? offer.accountName : offer.email) (from \(offer.hostName))?")
                    .font(.title3.weight(.medium))
                Text("\(offer.hostName) is signed in to this account. Signing in here too lets your phone reach \(name) from anywhere.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if content.accepting {
                    Text("Signing in…").foregroundStyle(.secondary)
                }
            } else {
                Text("Sign in to reach \(name) from your phone when you are away from this network.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let phone = content.also {
                    Toggle("Also sign in \(phone.name)", isOn: $choices.alsoTicked)
                        .toggleStyle(.checkbox)
                        .padding(.leading, 4)
                }
                if content.signingIn {
                    Text("Waiting for you to sign in…").foregroundStyle(.secondary)
                }
            }
        }
        .font(.title3)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Repositories

private struct Repositories: View {
    let content: SetupContent
    @Binding var choices: SetupChoices
    let actions: SetupActions

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New agents start in a repository. These are where Claude Code and Codex ran lately:")
                .font(.title3)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if !content.repositoriesLoaded {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Looking for recent folders…").foregroundStyle(.secondary)
                }
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        if choices.repositories.isEmpty {
                            Text("No recent folders found.").foregroundStyle(.secondary)
                        }
                        ForEach($choices.repositories) { $row in
                            Toggle(isOn: $row.ticked) {
                                Text(tilde(row.path))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                    .help(row.path)
                            }
                            .toggleStyle(.checkbox)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.leading, 4)
                }
                .frame(maxHeight: 190)
                .fixedSize(horizontal: false, vertical: true)
                Button("Add a Folder…", action: addFolder)
            }
        }
        .font(.title3)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"
        panel.message = "Choose the folders new agents can start in."
        guard panel.runModal() == .OK else { return }
        actions.addFolders(panel.urls)
    }
}

// MARK: - Help improve amux

private struct Improve: View {
    @Binding var choices: SetupChoices

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            Toggle(isOn: $choices.autoReports) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Send a report automatically when something goes wrong")
                    Text("Never your chats, code, or file names.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.checkbox)
            .padding(.leading, 4)
            .font(.title3)
            // Usage counts are on unless turned off in Settings; this only
            // says so, and nothing waits for it.
            Text("amux sends usage counts to help improve it, never your code or chats. You can turn this off in Settings.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Try starting an agent from your phone

private struct FirstAgent: View {
    var body: some View {
        VStack(spacing: 18) {
            Text("On your phone, tap + and pick a repository.")
                .font(.title3)
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Waiting for an agent from your phone…").foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity)
    }
}
