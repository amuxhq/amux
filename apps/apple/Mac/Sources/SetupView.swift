import AmuxCore
import AmuxValues
import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

/// Setting up, one step a screen, in the window: a symbol, the step's title
/// and what it is for, its choices, and Later beside the step's own action.
/// Escape is Later and Return the step's action, as on the terminal.
struct SetupScreen: View {
    @Bindable var setup: Setup
    let model: AppModel

    var body: some View {
        if let step = setup.step {
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
                content(step)
                    .frame(maxWidth: 560)
                    .padding(.horizontal, 40)
                    .padding(.top, 22)
                if let error = setup.error {
                    Text(error)
                        .foregroundStyle(.red)
                        .font(.callout)
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                        .padding(.horizontal, 40)
                        .padding(.top, 12)
                }
                Spacer(minLength: 16)
                if setup.noticeOn == step {
                    UsageNoticeLine()
                        .padding(.horizontal, 40)
                        .padding(.bottom, 14)
                }
                Buttons(setup: setup, step: step)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .id(step)
            .transition(.opacity)
        }
    }

    @ViewBuilder
    private func content(_ step: SetupStep) -> some View {
        switch step {
        case .pair_phone: PairPhone(setup: setup, model: model)
        case .keep_reachable: KeepReachable(setup: setup, model: model)
        case .sign_in: SignIn(setup: setup, model: model)
        case .repositories: Repositories(setup: setup)
        case .improve: Improve(setup: setup)
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
}

/// What a step is called on its screen, in the terminal's words.
func title(_ step: SetupStep) -> String {
    switch step {
    case .pair_phone: "Connect your iPhone"
    case .keep_reachable: "Keep it connected"
    case .sign_in: "Reach it from anywhere"
    case .repositories: "Repositories"
    case .improve: "Help improve amux"
    case .first_agent: "Try starting an agent from your phone"
    }
}

/// Later on the left, the step's own action on the right.
private struct Buttons: View {
    @Bindable var setup: Setup
    let step: SetupStep

    var body: some View {
        HStack(spacing: 12) {
            Button("Later") { setup.later() }
                .keyboardShortcut(.cancelAction)
                .disabled(setup.busy)
            Spacer()
            if setup.busy || setup.signingIn || setup.accepting {
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
        switch step {
        case .pair_phone, .first_agent:
            EmptyView()
        case .keep_reachable:
            Button("Continue") { setup.confirmReachable() }
                .keyboardShortcut(.defaultAction)
                .disabled(setup.busy)
        case .sign_in:
            if !setup.offers.isEmpty {
                Button("Not Now") { setup.declineOffer() }
                    .disabled(setup.accepting)
            }
            Button(setup.offers.isEmpty ? "Sign In…" : "Sign In") { setup.signIn() }
                .keyboardShortcut(.defaultAction)
                .disabled(setup.signingIn || setup.accepting)
        case .repositories:
            Button("Continue") { setup.confirmRepositories() }
                .keyboardShortcut(.defaultAction)
                .disabled(setup.busy || !setup.repositoriesLoaded)
        case .improve:
            Button("Continue") { setup.confirmImprove() }
                .keyboardShortcut(.defaultAction)
                .disabled(setup.busy)
        }
    }
}

// MARK: - Connect your iPhone

private struct PairPhone: View {
    @Bindable var setup: Setup
    let model: AppModel

    var body: some View {
        VStack(spacing: 26) {
            MachineName(setup: setup, model: model)
            HStack(alignment: .center, spacing: 36) {
                VStack(alignment: .leading, spacing: 18) {
                    Text("1. Scan to get amux on your iPhone")
                    pinLine
                    status
                    Text("Whoever enters this code can run agents on this Mac as you.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .font(.title3)
                .frame(maxWidth: .infinity, alignment: .leading)
                code
            }
        }
        .onAppear { setup.pairingShown() }
        .onDisappear { setup.pairingHidden() }
    }

    @ViewBuilder
    private var pinLine: some View {
        if let pin = setup.pairing.window?.pin, !pin.isEmpty {
            Text("2. Enter \(Text(spaced(pin)).monospacedDigit().fontWeight(.semibold)) in the app")
                .textSelection(.enabled)
                .accessibilityLabel("2. Enter \(pin) in the app")
        } else {
            Text("2. Enter the code in the app")
        }
    }

    @ViewBuilder
    private var status: some View {
        let pairing = setup.pairing
        if let why = pairing.failed {
            Label("Pairing could not open: \(why)", systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
                .font(.callout)
        } else if pairing.lockedOut {
            HStack(spacing: 10) {
                Text("Closed after too many wrong codes.").foregroundStyle(.orange)
                Button("New Code") { setup.newCode() }
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
        if let link = setup.pairing.window?.qrLink, !link.isEmpty {
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
    @Bindable var setup: Setup
    let model: AppModel
    @State private var editing: String?
    @FocusState private var focused: Bool

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
                Text(model.settings?.hostName ?? "this Mac").fontWeight(.semibold)
                Button("Rename…") { editing = model.settings?.hostName ?? "" }
                    .buttonStyle(.link)
                    .padding(.leading, 4)
            }
        }
        .font(.title3)
    }

    private func save() {
        if let name = editing { setup.rename(name) }
        editing = nil
    }
}

/// The pairing link as a QR code, drawn by Core Image a module to a pixel
/// and scaled up without smoothing, black on white whatever the
/// appearance, so a phone's camera reads it.
struct QRCode: View {
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
        guard let output = filter.outputImage else { return nil }
        return context.createCGImage(output, from: output.extent)
    }
}

// MARK: - Keep it connected

private struct KeepReachable: View {
    @Bindable var setup: Setup
    let model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let phone = setup.paired {
                Label {
                    Text("Paired with \(phone.name)")
                } icon: {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                .font(.title3.weight(.medium))
                .frame(maxWidth: .infinity)
                .padding(.bottom, 6)
            }
            Text("So your phone reaches \(model.settings?.hostName ?? "this Mac") whenever you need it:")
                .font(.title3)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 10) {
                if model.settings?.startAtLogin != nil {
                    Toggle("Start amux when you log in", isOn: $setup.startAtLogin)
                }
                Toggle("Keep this Mac awake while it’s plugged in", isOn: $setup.keepAwake)
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
    @Bindable var setup: Setup
    let model: AppModel

    var body: some View {
        let name = model.settings?.hostName ?? "this Mac"
        VStack(alignment: .leading, spacing: 16) {
            if let offer = setup.offers.first {
                Text("Sign in as \(offer.email.isEmpty ? offer.accountName : offer.email) (from \(offer.hostName))?")
                    .font(.title3.weight(.medium))
                Text("\(offer.hostName) is signed in to this account. Signing in here too lets your phone reach \(name) from anywhere.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if setup.accepting {
                    Text("Signing in…").foregroundStyle(.secondary)
                }
            } else {
                Text("Sign in to reach \(name) from your phone when you are away from this network.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let phone = setup.also {
                    Toggle("Also sign in \(phone.name)", isOn: $setup.alsoTicked)
                        .toggleStyle(.checkbox)
                        .padding(.leading, 4)
                }
                if setup.signingIn {
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
    @Bindable var setup: Setup

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New agents start in a repository. These are where Claude Code and Codex ran lately:")
                .font(.title3)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if !setup.repositoriesLoaded {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Looking for recent folders…").foregroundStyle(.secondary)
                }
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        if setup.repositories.isEmpty {
                            Text("No recent folders found.").foregroundStyle(.secondary)
                        }
                        ForEach($setup.repositories) { $row in
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
        for folder in panel.urls { setup.add(folder) }
    }
}

/// A path under the home folder as `~/…`.
func tilde(_ path: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    guard path == home || path.hasPrefix(home + "/") else { return path }
    return "~" + path.dropFirst(home.count)
}

// MARK: - Help improve amux

private struct Improve: View {
    @Bindable var setup: Setup

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Tell us how amux is used and when it goes wrong, so we can fix what matters.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 10) {
                Toggle("Send usage counts", isOn: $setup.telemetry)
                Toggle("Send a report automatically when something goes wrong", isOn: $setup.autoReports)
            }
            .toggleStyle(.checkbox)
            .padding(.leading, 4)
            Text("Never your prompts, code, or file names.")
                .font(.callout.weight(.medium))
        }
        .font(.title3)
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

// MARK: - The analytics notice

/// What amux sends, said once on the first screen any client shows, in the
/// words every client uses, with the page that says how to turn it off.
struct UsageNoticeLine: View {
    private let notice = Onboarding.notice

    var body: some View {
        VStack(spacing: 2) {
            Text(notice.words).foregroundStyle(.secondary)
            if let url = URL(string: notice.url) {
                Link(notice.url, destination: url)
            }
        }
        .font(.footnote)
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
    }
}
