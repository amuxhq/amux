import AmuxValues
import AppKit
import SwiftUI

/// What the Settings window shows.
public enum SettingsState {
    case connecting
    case failed(String)
    case ready(MachineSettings)
}

/// The account row of the Settings window.
public struct SettingsAccount {
    /// The profile on screen; nil while none is open.
    public var profile: ProfileView?
    public var signingIn: Bool

    public init(profile: ProfileView?, signingIn: Bool = false) {
        self.profile = profile
        self.signingIn = signingIn
    }
}

/// What the Settings window can ask for.
public struct SettingsActions {
    /// Applies a change, answering why it did not take, or nil once it did.
    public var apply: @MainActor (SettingsChange) async -> String?
    /// Pins a folder among this Mac's repositories, or unpins it, answering
    /// why it did not take, or nil once it did.
    public var pin: @MainActor (_ path: String, _ pinned: Bool) async -> String?
    public var signIn: @MainActor () -> Void
    public var signOut: @MainActor () -> Void

    public init(
        apply: @escaping @MainActor (SettingsChange) async -> String? = { _ in nil },
        pin: @escaping @MainActor (_ path: String, _ pinned: Bool) async -> String? = { _, _ in nil },
        signIn: @escaping @MainActor () -> Void = {},
        signOut: @escaping @MainActor () -> Void = {}
    ) {
        self.apply = apply
        self.pin = pin
        self.signIn = signIn
        self.signOut = signOut
    }
}

/// The Settings window: this Mac's everyday settings, in the words setting
/// up used, and the account of the profile on screen.
public struct SettingsScreen: View {
    let state: SettingsState
    let account: SettingsAccount
    /// This Mac's repositories, as its entry pins them.
    let repositories: [String]
    let actions: SettingsActions

    public init(
        state: SettingsState, account: SettingsAccount, repositories: [String] = [],
        actions: SettingsActions = SettingsActions()
    ) {
        self.state = state
        self.account = account
        self.repositories = repositories
        self.actions = actions
    }

    public var body: some View {
        Group {
            switch state {
            case .ready(let settings):
                SettingsForm(
                    settings: settings, account: account, repositories: repositories, actions: actions)
            case .failed(let reason):
                VStack(spacing: 8) {
                    Text("amux isn’t running").font(.ui(.title, weight: .semibold))
                    Text(LaunchScreen.marked(reason))
                        .foregroundStyle(Palette.textSecondary)
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                }
                .padding(Metrics.gutter)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .connecting:
                HStack(spacing: 8) {
                    Working(size: 12)
                    Text("Connecting…").foregroundStyle(Palette.textSecondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .font(.ui(.body))
        .textTiers()
        .background(Palette.content)
        .frame(width: 540)
        .frame(minHeight: 680)
    }
}

private struct SettingsForm: View {
    let settings: MachineSettings
    let account: SettingsAccount
    let repositories: [String]
    let actions: SettingsActions
    @State private var name = ""
    @State private var problem: String?
    @FocusState private var naming: Bool

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                SettingsGroup("This Mac") {
                    SettingRow("Name", note: "How this Mac appears on your other devices.") {
                        TextField("Name", text: $name, prompt: Text("Name"))
                            .labelsHidden()
                            .textFieldStyle(.plain)
                            .multilineTextAlignment(.trailing)
                            .frame(maxWidth: 200)
                            .focused($naming)
                            .onSubmit(rename)
                            .onChange(of: naming) { _, focused in if !focused { rename() } }
                    }
                    if let startAtLogin = settings.startAtLogin {
                        SettingRow("Start amux when you log in") {
                            Toggle("Start amux when you log in", isOn: change(startAtLogin) { .with(startAtLogin: $0) })
                        }
                    }
                    SettingRow(
                        "Keep this Mac awake while it’s plugged in",
                        note: "On battery, only while an agent is working."
                    ) {
                        Toggle("Keep this Mac awake", isOn: change(settings.keepAwake) { .with(keepAwake: $0) })
                    }
                }

                SettingsGroup(
                    "Repositories", footer: "Agents work in these, whether you start them here or from your phone.",
                    trailing: { Button("Add repository…", action: addFolder) }
                ) {
                    if repositories.isEmpty {
                        SettingRow("No repositories yet") { EmptyView() }
                    }
                    ForEach(repositories, id: \.self) { path in
                        RepositoryLine(path: path) { remove(path) }
                    }
                }

                SettingsGroup("Help improve amux") {
                    SettingRow("Send usage counts", note: "Never includes your code or chats.") {
                        Toggle("Send usage counts", isOn: change(settings.telemetry) { .with(telemetry: $0) })
                    }
                    SettingRow(
                        "Send a report automatically when something goes wrong",
                        note: "Never includes your chats, code or file names."
                    ) {
                        Toggle("Send reports", isOn: change(settings.autoReports) { .with(autoReports: $0) })
                    }
                }

                SettingsGroup("Account") {
                    Account(account: account, actions: actions)
                }

                if !settings.experiments.isEmpty {
                    SettingsGroup(
                        "Experiments",
                        footer: settings.applyingExperiments
                            ? "amux restarts to apply the change. Agents keep running." : nil
                    ) {
                        ForEach(settings.experiments, id: \.name) { experiment in
                            SettingRow(experiment.name.capitalizedFirst, note: experiment.description) {
                                if experiment.applying {
                                    // amux restarts to run what the switch now
                                    // says; until it has, neither on nor off is
                                    // true.
                                    HStack(spacing: 6) {
                                        ProgressView().controlSize(.small)
                                        Text("Applying…").foregroundStyle(Palette.textSecondary)
                                    }
                                } else {
                                    Toggle(experiment.name.capitalizedFirst, isOn: change(experiment.enabled) { on in
                                        .with(experiments: settings.switching(experiment.name, on: on))
                                    })
                                }
                            }
                        }
                    }
                }

                if let problem {
                    Text(problem).foregroundStyle(Palette.failure).textSelection(.enabled)
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 24)
        }
        // The main window's ground and controls, so Settings reads as the
        // same app: black, with blue switches and the window's pills.
        .background(Palette.content)
        .toggleStyle(AccentSwitch())
        .buttonStyle(PillButtonStyle(small: true))
        .onAppear {
            name = settings.hostName
            // The window opens with nothing in hand: the name field would
            // otherwise take the focus and show its words selected.
            Task { @MainActor in naming = false }
        }
        .onChange(of: settings.hostName) { _, now in if !naming { name = now } }
    }

    /// A toggle that applies its change at once, showing what the daemon
    /// answered rather than what was asked.
    private func change(_ value: Bool, _ make: @escaping (Bool) -> SettingsChange) -> Binding<Bool> {
        Binding(get: { value }, set: { apply(make($0)) })
    }

    private func apply(_ change: SettingsChange) {
        Task { problem = await actions.apply(change) }
    }

    private func rename() {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != settings.hostName else { return }
        apply(.with(hostName: trimmed))
    }

    private func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"
        guard panel.runModal() == .OK else { return }
        let paths = panel.urls.map(\.standardizedFileURL.path).filter { !repositories.contains($0) }
        Task {
            for path in paths {
                if let failed = await actions.pin(path, true) {
                    problem = failed
                    return
                }
            }
            problem = nil
        }
    }

    private func remove(_ path: String) {
        Task { problem = await actions.pin(path, false) }
    }
}

/// The profile on screen's account: who it is signed in as, and Sign In or
/// Sign Out.
private struct Account: View {
    let account: SettingsAccount
    let actions: SettingsActions

    var body: some View {
        if let profile = account.profile {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    switch profile.account.binding {
                    case .signedIn:
                        Text(profile.account.name.isEmpty ? profile.account.email : profile.account.name)
                        if !profile.account.name.isEmpty {
                            Text(profile.account.email).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
                        }
                    case .signedOut:
                        Text("Signed out of \(profile.account.email)")
                    case .unbound:
                        Text("Not signed in")
                        Text("Sign in to reach this Mac from your phone on any network.")
                            .font(.ui(.meta))
                            .foregroundStyle(Palette.textSecondary)
                    }
                }
                Spacer()
                if profile.account.binding == .signedIn {
                    Button("Sign out") { actions.signOut() }
                } else {
                    Button("Sign in…") { actions.signIn() }
                        .disabled(account.signingIn)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(minHeight: 44)
        } else {
            // No profile yet: amux is starting, or not running at all.
            SettingRow("Waiting for amux…") { EmptyView() }
        }
    }
}

/// A titled group of settings on one card, split by hairlines, as the
/// host page groups its facts; a footnote and an action may follow it.
private struct SettingsGroup<Content: View, Trailing: View>: View {
    let title: String
    var footer: String?
    @ViewBuilder let trailing: Trailing
    @ViewBuilder let content: Content

    init(
        _ title: String, footer: String? = nil, @ViewBuilder trailing: () -> Trailing = { EmptyView() },
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.footer = footer
        self.trailing = trailing()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.ui(.meta, weight: .medium)).foregroundStyle(Palette.textSecondary).padding(.leading, 2)
            Separated { content }
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.card))
                .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.line))
            HStack(alignment: .firstTextBaseline) {
                if let footer {
                    Text(footer).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
                }
                Spacer(minLength: 12)
                trailing
            }
            .padding(.horizontal, 2)
        }
    }
}

/// One setting: its title over a quiet note on the left, its control on
/// the right, centred against both.
private struct SettingRow<Control: View>: View {
    let title: String
    var note: String?
    @ViewBuilder let control: Control

    init(_ title: String, note: String? = nil, @ViewBuilder control: () -> Control) {
        self.title = title
        self.note = note
        self.control = control()
    }

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).foregroundStyle(Palette.text)
                if let note {
                    Text(note).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 12)
            control
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
    }
}

/// A repository new agents start in: its name over where it is, and a
/// remove button while the pointer is on it.
private struct RepositoryLine: View {
    let path: String
    let remove: () -> Void
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            Icon("folder", size: 14, optical: false).foregroundStyle(Palette.textSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text((path as NSString).lastPathComponent).foregroundStyle(Palette.text).lineLimit(1)
                Text(tilde((path as NSString).deletingLastPathComponent))
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .help(path)
            Spacer(minLength: 12)
            Button(action: remove) {
                Icon("x", size: 12)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 24, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Remove \(tilde(path))")
            .accessibilityLabel("Remove \(tilde(path))")
            .opacity(hovering ? 1 : 0)
        }
        .padding(.leading, 14)
        .padding(.trailing, 8)
        .padding(.vertical, 8)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

extension SettingsChange {
    /// A change to only the settings named.
    public static func with(
        hostName: String? = nil, startAtLogin: Bool? = nil, keepAwake: Bool? = nil,
        telemetry: Bool? = nil, autoReports: Bool? = nil, experiments: [String]? = nil
    ) -> SettingsChange {
        SettingsChange(
            autoReports: autoReports, experiments: experiments, hostName: hostName,
            keepAwake: keepAwake, startAtLogin: startAtLogin, telemetry: telemetry)
    }
}

extension MachineSettings {
    /// The names of the experiments on once `name` is switched `on`: the
    /// whole list a change sends.
    func switching(_ name: String, on: Bool) -> [String] {
        experiments.filter { $0.name == name ? on : $0.enabled }.map(\.name)
    }

    /// amux is restarting to apply an experiment switch: until it has, the
    /// settings are worth reading again.
    public var applyingExperiments: Bool {
        experiments.contains(where: \.applying)
    }
}

/// A path under the home folder as `~/…`.
func tilde(_ path: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    guard path == home || path.hasPrefix(home + "/") else { return path }
    return "~" + path.dropFirst(home.count)
}

/// A switch in the accent whether or not the window is in front, as the
/// main window's controls are; the system's goes grey in a background
/// window. It draws the switch alone: the row says what it switches, and
/// the label stays as its accessible name.
struct AccentSwitch: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack {
            Capsule()
                .fill(configuration.isOn ? Palette.accent : Color.primary.opacity(0.16))
                .frame(width: 34, height: 20)
                .overlay(alignment: configuration.isOn ? .trailing : .leading) {
                    Circle().fill(.white).frame(width: 16, height: 16).padding(2)
                        .shadow(color: .black.opacity(0.2), radius: 1, y: 0.5)
                }
                .animation(.snappy(duration: 0.15), value: configuration.isOn)
                .onTapGesture { configuration.isOn.toggle() }
        }
        .accessibilityRepresentation {
            Toggle(isOn: configuration.$isOn) { configuration.label }
        }
    }
}
