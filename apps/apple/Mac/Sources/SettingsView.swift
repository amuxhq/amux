import AmuxCore
import AmuxValues
import AppKit
import SwiftUI

/// The Settings window: this Mac's everyday settings, in the words setting
/// up used, and the account of the profile on screen. Every change is
/// applied at once through the daemon, which keeps them in amux's config
/// file, so `amux config` and this window always agree.
struct SettingsWindow: View {
    let model: AppModel

    var body: some View {
        Group {
            if let settings = model.settings {
                SettingsForm(model: model, settings: settings)
            } else if case .failed(let reason) = model.phase {
                ContentUnavailableView(
                    "amux is not running", systemImage: "exclamationmark.triangle",
                    description: Text(reason))
            } else {
                ProgressView("Connecting to amux")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(width: 540)
        .frame(minHeight: 680)
        // Changed beside the app (`amux config`, the terminal's settings),
        // so read again whenever the window is opened.
        .task { await model.loadSettings() }
    }
}

private struct SettingsForm: View {
    let model: AppModel
    let settings: MachineSettings
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
                Account(model: model)
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
        Task { problem = await model.apply(change) }
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
    let model: AppModel

    var body: some View {
        if let profile = model.current {
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
                    Button("Sign Out") { Task { await model.signOut() } }
                } else {
                    Button("Sign In…") { Task { await model.signInFromMenu() } }
                        .disabled(model.signingIn)
                }
            }
        } else {
            Text("No profile open").foregroundStyle(.secondary)
        }
    }
}
