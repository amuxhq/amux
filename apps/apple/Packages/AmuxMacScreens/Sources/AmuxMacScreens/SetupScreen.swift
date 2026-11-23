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
    /// The repository it is a checkout of, as its agents will be marked.
    public var repository: Repository?
    public var id: String { path }

    public init(path: String, ticked: Bool, repository: Repository? = nil) {
        self.path = path
        self.ticked = ticked
        self.repository = repository
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

/// Setting up, one step a screen, in the window: a tile with the step's
/// glyph, the step's title and what it is for, its choices, and Later
/// beside the step's own action. Every step is one centred column, sitting
/// a little above the window's middle. Escape is Later and Return the
/// step's action, as on the terminal.
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
            // A window too short for the column scrolls it rather than
            // crushing it.
            ViewThatFits(in: .vertical) {
                centred(step)
                ScrollView { column(step).padding(.vertical, 28) }
            }
            Buttons(content: content, actions: actions)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .font(.ui(.prose))
        .textTiers()
        .background(Palette.content.ignoresSafeArea())
        .windowChrome()
        .id(step)
        .transition(.opacity)
    }

    private func centred(_ step: SetupStep) -> some View {
        VStack(spacing: 0) {
            Spacer(minLength: 24)
            column(step)
                // A little above the middle, where the eye expects it.
                .padding(.bottom, 40)
            Spacer(minLength: 24)
        }
    }

    private func column(_ step: SetupStep) -> some View {
        VStack(spacing: 0) {
            // The pairing step's code is its picture.
            if step != .pair_phone {
                Icon(glyph(step), size: 28)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 56, height: 56)
                    .background(RoundedRectangle(cornerRadius: Radius.sheet, style: .continuous).fill(Palette.well))
                    .overlay(RoundedRectangle(cornerRadius: Radius.sheet, style: .continuous).strokeBorder(Palette.line))
                    .padding(.bottom, 18)
            }
            Text(title(step))
                .font(.ui(.heading, weight: .semibold))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            stepContent(step)
                .padding(.top, 14)
            if let error = content.error {
                Text(error)
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.failure)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .padding(.top, 14)
            }
        }
        .multilineTextAlignment(.center)
        .frame(maxWidth: 440)
        .padding(.horizontal, Metrics.gutter)
        .frame(maxWidth: .infinity)
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

    private func glyph(_ step: SetupStep) -> String {
        switch step {
        case .pair_phone: "smartphone"
        case .keep_reachable: "laptop"
        case .sign_in: "globe"
        case .repositories: "folder-git-2"
        case .improve: "lightbulb"
        case .first_agent: "smartphone"
        }
    }

    /// What a step is called on its screen.
    private func title(_ step: SetupStep) -> String {
        switch step {
        case .pair_phone: "Connect your iPhone"
        case .keep_reachable: "Keep your Mac ready"
        case .sign_in: "Reach your Mac through the relay"
        case .repositories: "Where should agents work?"
        case .improve: "Help improve amux"
        case .first_agent: "Start an agent from your phone"
        }
    }
}

/// Later on the left, the step's own action on the right.
private struct Buttons: View {
    let content: SetupContent
    let actions: SetupActions

    var body: some View {
        HStack(spacing: 8) {
            // The last step only waits on the phone: leaving it is skipping
            // the try, not putting a choice off.
            Button(content.step == .first_agent ? "Skip" : "Not now") { actions.later() }
                .buttonStyle(PillButtonStyle())
                .keyboardShortcut(.cancelAction)
                .disabled(content.busy)
            Spacer()
            // Signing in says it is waiting on the page itself.
            if content.busy {
                ProgressView().controlSize(.small).padding(.trailing, 4)
            }
            primary
        }
        .padding(.horizontal, 20)
        .frame(height: 60)
        .overlay(alignment: .top) { Rectangle().fill(Palette.line).frame(height: 1) }
    }

    @ViewBuilder
    private var primary: some View {
        switch content.step {
        case .pair_phone, .first_agent:
            EmptyView()
        case .keep_reachable:
            Button("Continue") { actions.confirmReachable() }
                .buttonStyle(PillButtonStyle(prominent: true))
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy)
        case .sign_in:
            // An offered account can be turned down for another one.
            if content.offer != nil {
                Button("Use another account") { actions.declineOffer() }
                    .buttonStyle(PillButtonStyle())
                    .disabled(content.signingIn || content.accepting)
            }
            Button(content.offer == nil ? "Sign in…" : "Sign in") { actions.signIn() }
                .buttonStyle(PillButtonStyle(prominent: true))
                .keyboardShortcut(.defaultAction)
                .disabled(content.signingIn || content.accepting)
        case .repositories:
            Button("Continue") { actions.confirmRepositories() }
                .buttonStyle(PillButtonStyle(prominent: true))
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy || !content.repositoriesLoaded)
        case .improve:
            Button("Continue") { actions.confirmImprove() }
                .buttonStyle(PillButtonStyle(prominent: true))
                .keyboardShortcut(.defaultAction)
                .disabled(content.busy)
        }
    }
}

/// What a step is for, under its title: centred, a step quieter than it.
private struct Explain: View {
    let words: String

    init(_ words: String) { self.words = words }

    var body: some View {
        Text(words)
            .foregroundStyle(Palette.textSecondary)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A note under a step's choices: the quietest words on the page.
private struct Note: View {
    let words: String

    init(_ words: String) { self.words = words }

    var body: some View {
        Text(words)
            .font(.ui(.meta))
            .foregroundStyle(Palette.textSecondary)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// Checkboxes, left-aligned in a narrower box under the centred words, so
/// their boxes line up.
private struct Choices<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) { content }
            .toggleStyle(AccentCheckbox())
            .font(.ui(.body))
            .multilineTextAlignment(.leading)
            .padding(14)
            .frame(maxWidth: 360, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.raised))
            .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.edge))
    }
}

/// A checkbox's own words with a quieter line under them.
private struct Choice: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            Text(detail).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: - Connect your iPhone

private struct PairPhone: View {
    let content: SetupContent
    let actions: SetupActions

    var body: some View {
        VStack(spacing: 0) {
            MachineName(content: content, actions: actions)
            code
                .padding(.top, 20)
            Text("Scan with your iPhone’s camera to pair.")
                .padding(.top, 18)
            pin
                .padding(.top, 10)
            status
                .font(.ui(.meta))
                .frame(minHeight: 18)
                .padding(.top, 12)
            Note("Anyone with this code can run agents on this Mac as you, so keep it to your own phone.")
                .frame(maxWidth: 340)
                .padding(.top, 8)
        }
        .onAppear { actions.pairingShown() }
        .onDisappear { actions.pairingHidden() }
    }

    @ViewBuilder
    private var pin: some View {
        if let pin = content.pairing.window?.pin, !pin.isEmpty {
            HStack(spacing: 10) {
                Text("Or enter").foregroundStyle(Palette.textSecondary)
                Text(spaced(pin))
                    .font(.mono(.heading, weight: .semibold))
                    .tracking(2)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .frame(height: 40)
                    .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.well))
                    .accessibilityLabel(pin)
                Text("in the amux app.").foregroundStyle(Palette.textSecondary)
            }
            .accessibilityElement(children: .combine)
        } else {
            Text("Or enter the code in the amux app.")
                .foregroundStyle(Palette.textSecondary)
                .frame(height: 40)
        }
    }

    @ViewBuilder
    private var status: some View {
        let pairing = content.pairing
        if let why = pairing.failed {
            HStack(spacing: 6) {
                Icon("triangle-alert", size: 14)
                Text("Couldn’t start pairing: \(why)")
            }
            .foregroundStyle(Palette.caution)
        } else if pairing.lockedOut {
            HStack(spacing: 10) {
                Text("Too many wrong codes.").foregroundStyle(Palette.caution)
                Button("New code") { actions.newCode() }
                    .buttonStyle(PillButtonStyle(small: true))
            }
        } else if pairing.joining {
            HStack(spacing: 8) {
                Working(size: 12).offset(y: 1.25)
                Text("Your iPhone is pairing…")
            }
        } else if pairing.window != nil {
            Text("Waiting for your iPhone…").foregroundStyle(Palette.textSecondary)
        } else {
            Text("Getting a code…").foregroundStyle(Palette.textSecondary)
        }
    }

    /// The code, dimmed under a spinner while a phone is pairing with it:
    /// it has done its work, and scanning it again would not help.
    @ViewBuilder
    private var code: some View {
        if let link = content.pairing.window?.qrLink, !link.isEmpty {
            QRCode(link: link)
                .frame(width: 168, height: 168)
                .opacity(content.pairing.joining ? 0.3 : 1)
                .overlay {
                    if content.pairing.joining {
                        Working(size: 20)
                            .frame(width: 44, height: 44)
                            .background(Circle().fill(Palette.raised))
                            .elevation(.raised)
                    }
                }
                .animation(.easeOut(duration: 0.2), value: content.pairing.joining)
                .accessibilityLabel("QR code to pair your iPhone, or get amux from the App Store")
        } else {
            RoundedRectangle(cornerRadius: Radius.sheet, style: .continuous)
                .fill(Palette.well)
                .frame(width: 168, height: 168)
                .overlay { ProgressView().controlSize(.regular) }
        }
    }

    /// A PIN as people read it aloud: two groups of three.
    private func spaced(_ pin: String) -> String {
        guard pin.count == 6 else { return pin }
        return "\(pin.prefix(3)) \(pin.suffix(3))"
    }
}

/// "This Mac is called …", with Rename: the name every paired device sees.
/// Renaming swaps the name for a field of the same height in place, so
/// nothing under it moves.
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
            Text("This Mac is called").foregroundStyle(Palette.textSecondary)
            if let binding = Binding($editing) {
                TextField("Name", text: binding)
                    .textFieldStyle(.plain)
                    .multilineTextAlignment(.leading)
                    .font(.ui(.prose, weight: .semibold))
                    .padding(.horizontal, 8)
                    .frame(width: 200, height: 26)
                    .background(RoundedRectangle(cornerRadius: Radius.control, style: .continuous).fill(Palette.well))
                    .overlay(
                        RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                            .strokeBorder(Palette.accent.opacity(focused ? 0.6 : 0)))
                    .focused($focused)
                    .onSubmit(save)
                    .onExitCommand { editing = nil }
                    .onAppear { focused = true }
                Button("Save", action: save)
                    .buttonStyle(PillButtonStyle(small: true))
            } else {
                Text(content.machineName ?? "this Mac").fontWeight(.semibold)
                Button("Rename…") { editing = content.machineName ?? "" }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.accent)
                    .pointerStyle(.link)
                    .padding(.leading, 2)
            }
        }
        .frame(height: 28)
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
                .background(.white, in: RoundedRectangle(cornerRadius: Radius.sheet, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: Radius.sheet, style: .continuous).strokeBorder(Palette.line))
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
        VStack(spacing: 16) {
            if let phone = content.paired {
                HStack(spacing: 6) {
                    Icon("circle-check", size: 14).foregroundStyle(Palette.accent)
                    Text("Paired with \(phone.name)")
                }
                .font(.ui(.prose, weight: .medium))
            }
            Explain("So your phone can always reach \(content.machineName ?? "this Mac"):")
            Choices {
                if content.canStartAtLogin {
                    Toggle("Start amux when you log in", isOn: $choices.startAtLogin)
                }
                Toggle(isOn: $choices.keepAwake) {
                    Choice(
                        title: "Keep this Mac awake while it’s plugged in",
                        detail: "On battery, only while an agent is working.")
                }
            }
        }
    }
}

// MARK: - Reach it from anywhere

private struct SignIn: View {
    let content: SetupContent
    @Binding var choices: SetupChoices

    var body: some View {
        let name = content.machineName ?? "this Mac"
        VStack(spacing: 16) {
            if let offer = content.offer {
                Text("Continue as \(offer.email.isEmpty ? offer.accountName : offer.email)?")
                    .font(.ui(.title, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                Explain("You’re signed in on \(offer.hostName). Use the same account here so your phone can reach this Mac through the relay.")
                if content.accepting {
                    Waiting("Signing in…")
                }
            } else {
                Explain("Sign in so your phone can reach \(name) from any network.")
                if let phone = content.also {
                    Choices {
                        Toggle("Sign in on \(phone.name) too", isOn: $choices.alsoTicked)
                    }
                }
                if content.signingIn {
                    Waiting("Continue in your browser…")
                }
            }
        }
    }
}

/// Something under way that the screen waits on.
private struct Waiting: View {
    let words: String

    init(_ words: String) { self.words = words }

    var body: some View {
        HStack(spacing: 8) {
            // A dot reads as a bullet, on the lowercase middle.
            Working(size: 12).offset(y: 1.25)
            Text(words).foregroundStyle(Palette.textSecondary)
        }
        .font(.ui(.meta))
    }
}

// MARK: - Where should agents work?

private struct Repositories: View {
    let content: SetupContent
    @Binding var choices: SetupChoices
    let actions: SetupActions

    var body: some View {
        VStack(spacing: 16) {
            Explain("These are the repositories you’ve used with Claude Code and Codex lately.")
            if !content.repositoriesLoaded {
                Waiting("Looking for recent repositories…")
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Choices {
                        if choices.repositories.isEmpty {
                            Text("Nothing yet. Add a repository below.").foregroundStyle(Palette.textSecondary)
                        }
                        ScrollView {
                            VStack(alignment: .leading, spacing: 10) {
                                ForEach($choices.repositories) { $row in
                                    Toggle(isOn: $row.ticked) {
                                        HStack(spacing: 8) {
                                            DirectoryMark(
                                                directory: Directory(
                                                    path: row.path, name: "", repository: row.repository),
                                                size: 16)
                                            Text((row.path as NSString).lastPathComponent)
                                                .lineLimit(1)
                                            Text(tilde((row.path as NSString).deletingLastPathComponent))
                                                .font(.ui(.meta))
                                                .foregroundStyle(Palette.textSecondary)
                                                .lineLimit(1)
                                                .truncationMode(.head)
                                        }
                                        .help(row.path)
                                    }
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .scrollBounceBehavior(.basedOnSize)
                        .frame(maxHeight: 170)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                    Button("Add repository…", action: addFolder)
                        .buttonStyle(PillButtonStyle(small: true))
                }
                .frame(maxWidth: 360)
            }
        }
    }

    private func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"
        panel.message = "Choose folders for agents to work in."
        guard panel.runModal() == .OK else { return }
        actions.addFolders(panel.urls)
    }
}

// MARK: - Help improve amux

private struct Improve: View {
    @Binding var choices: SetupChoices

    var body: some View {
        VStack(spacing: 16) {
            Choices {
                Toggle(isOn: $choices.autoReports) {
                    Choice(
                        title: "Send a report automatically when something goes wrong",
                        detail: "Reports never include your chats, code or file names.")
                }
            }
            // Usage counts are on unless turned off in Settings; this only
            // says so, and nothing waits for it.
            Note("amux also sends usage counts, never your code or chats. Turn them off anytime in Settings.")
                .frame(maxWidth: 360)
        }
    }
}

// MARK: - Try starting an agent from your phone

private struct FirstAgent: View {
    var body: some View {
        VStack(spacing: 16) {
            Explain("On your phone, tap + and pick a repository.")
            Waiting("Waiting for your phone…")
        }
    }
}

/// A checkbox in the accent whether or not the window is in front; the
/// system's goes grey in a background window.
struct AccentCheckbox: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(configuration.isOn ? Palette.accent : Color.clear)
                .overlay(
                    RoundedRectangle(cornerRadius: 4, style: .continuous)
                        .strokeBorder(configuration.isOn ? Color.clear : Color.primary.opacity(0.3)))
                .overlay {
                    if configuration.isOn {
                        Icon("check", size: 11).foregroundStyle(.white)
                    }
                }
                .frame(width: 16, height: 16)
                .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 3 }
            configuration.label
        }
        .contentShape(Rectangle())
        .onTapGesture { configuration.isOn.toggle() }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(configuration.isOn ? "On" : "Off")
        .accessibilityAction { configuration.isOn.toggle() }
    }
}
