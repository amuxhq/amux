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
    public var signIn: @MainActor () -> Void
    public var signOut: @MainActor () -> Void

    public init(
        apply: @escaping @MainActor (SettingsChange) async -> String? = { _ in nil },
        signIn: @escaping @MainActor () -> Void = {},
        signOut: @escaping @MainActor () -> Void = {}
    ) {
        self.apply = apply
        self.signIn = signIn
        self.signOut = signOut
    }
}

/// The Settings window: this Mac's everyday settings, in the words setting
/// up used, and the account of the profile on screen.
public struct SettingsScreen: View {
    let state: SettingsState
    let account: SettingsAccount
    let actions: SettingsActions

    public init(
        state: SettingsState, account: SettingsAccount, actions: SettingsActions = SettingsActions()
    ) {
        self.state = state
        self.account = account
        self.actions = actions
    }

    public var body: some View {
        Group {
            switch state {
            case .ready(let settings):
                SettingsForm(settings: settings, account: account, actions: actions)
            case .failed(let reason):
                ContentUnavailableView(
                    "amux is not running", systemImage: "exclamationmark.triangle",
                    description: Text(reason))
            case .connecting:
                ProgressView("Connecting to amux")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(width: 540)
        .frame(minHeight: 680)
    }
}

private struct SettingsForm: View {
    let settings: MachineSettings
    let account: SettingsAccount
    let actions: SettingsActions
    @State private var name = ""
    @State private var problem: String?
    @FocusState private var naming: Bool

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $name)
                    .focused($naming)
                    .onSubmit(rename)
                    .onChange(of: naming) { _, focused in if !focused { rename() } }
                if let startAtLogin = settings.startAtLogin {
                    Toggle("Start amux when you log in", isOn: change(startAtLogin) { .with(startAtLogin: $0) })
                }
                Toggle(isOn: change(settings.keepAwake) { .with(keepAwake: $0) }) {
                    Text("Keep this Mac awake while it’s plugged in")
                    Text("On battery it stays awake only while an agent works.")
                }
            } header: {
                Text("This Mac")
            } footer: {
                Text("Paired devices see this Mac by its name.")
                    .foregroundStyle(.secondary)
            }

            Section {
                if settings.repositories.isEmpty {
                    Text("No repositories yet.").foregroundStyle(.secondary)
                }
                ForEach(settings.repositories, id: \.self) { path in
                    HStack {
                        Image(systemName: "folder").foregroundStyle(.secondary)
                        Text(tilde(path)).lineLimit(1).truncationMode(.middle).help(path)
                        Spacer()
                        Button {
                            apply(.with(repositories: settings.repositories.filter { $0 != path }))
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("Remove \(tilde(path))")
                    }
                }
                Button("Add a Folder…", action: addFolder)
            } header: {
                Text("Repositories")
            } footer: {
                Text("New agents start in a repository, here and from your phone.")
                    .foregroundStyle(.secondary)
            }

            Section("Help improve amux") {
                Toggle(isOn: change(settings.telemetry) { .with(telemetry: $0) }) {
                    Text("Send usage counts")
                    Text("Never your code or chats.")
                }
                Toggle(
                    "Send a report automatically when something goes wrong",
                    isOn: change(settings.autoReports) { .with(autoReports: $0) })
            }

            Section("Account") {
                Account(account: account, actions: actions)
            }

            if let problem {
                Text(problem).foregroundStyle(.red).textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .onAppear { name = settings.hostName }
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
        var paths = settings.repositories
        for folder in panel.urls {
            let path = folder.standardizedFileURL.path
            if !paths.contains(path) { paths.append(path) }
        }
        apply(.with(repositories: paths))
    }
}

/// The profile on screen's account: who it is signed in as, and Sign In or
/// Sign Out.
private struct Account: View {
    let account: SettingsAccount
    let actions: SettingsActions

    var body: some View {
        if let profile = account.profile {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    switch profile.account.binding {
                    case .signedIn:
                        Text(profile.account.name.isEmpty ? profile.account.email : profile.account.name)
                        if !profile.account.name.isEmpty {
                            Text(profile.account.email).font(.caption).foregroundStyle(.secondary)
                        }
                    case .signedOut:
                        Text("Signed out of \(profile.account.email)")
                    case .unbound:
                        Text("Not signed in")
                        Text("Sign in to reach this Mac from your phone anywhere.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if profile.account.binding == .signedIn {
                    Button("Sign Out") { actions.signOut() }
                } else {
                    Button("Sign In…") { actions.signIn() }
                        .disabled(account.signingIn)
                }
            }
        } else {
            Text("No profile open").foregroundStyle(.secondary)
        }
    }
}

extension SettingsChange {
    /// A change to only the settings named.
    public static func with(
        hostName: String? = nil, startAtLogin: Bool? = nil, keepAwake: Bool? = nil,
        repositories: [String]? = nil, telemetry: Bool? = nil, autoReports: Bool? = nil
    ) -> SettingsChange {
        SettingsChange(
            autoReports: autoReports, hostName: hostName, keepAwake: keepAwake,
            repositories: repositories, startAtLogin: startAtLogin, telemetry: telemetry)
    }
}

/// A path under the home folder as `~/…`.
func tilde(_ path: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    guard path == home || path.hasPrefix(home + "/") else { return path }
    return "~" + path.dropFirst(home.count)
}
