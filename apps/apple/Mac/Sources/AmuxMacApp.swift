import AmuxCore
import AmuxValues
import SwiftUI

/// The Mac app: a client of the amux daemon on this Mac and nothing more.
///
/// Its screens are a placeholder until the desktop's own design round; this
/// window proves the path every desktop client takes: install amux, attach,
/// sign in through the daemon, switch profiles and read a profile's fleet.
@main
struct AmuxMacApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @State private var model: AppModel

    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        AppDelegate.model = model
        // Started with the app, not with a window: a launch that restores no
        // window still attaches, and a window opened later shows it.
        Task { await model.start() }
    }

    var body: some Scene {
        WindowGroup("Amux") {
            Window(model: model)
                .frame(minWidth: 560, minHeight: 360)
        }
    }
}

/// Applies a staged update as the app quits, however it quits.
final class AppDelegate: NSObject, NSApplicationDelegate {
    @MainActor static var model: AppModel?

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Self.model?.applyUpdate() }
    }
}

private struct Window: View {
    @Bindable var model: AppModel

    var body: some View {
        switch model.phase {
        case .installing:
            ProgressView("Installing amux")
        case .attaching:
            ProgressView("Connecting to amux")
        case .failed(let reason):
            VStack(spacing: 12) {
                Text("amux is not running").font(.headline)
                Text(reason).foregroundStyle(.secondary).textSelection(.enabled)
                Button("Try again") { Task { await model.start() } }
                if model.update != nil {
                    Button("Restart to Complete Update") { model.restartToUpdate() }
                }
            }
            .padding()
        case .attached:
            NavigationSplitView {
                List(model.profiles, id: \.id, selection: $model.selected) { profile in
                    ProfileRow(profile: profile)
                }
                .navigationSplitViewColumnWidth(min: 200, ideal: 240)
                .toolbar {
                    Button("Sign In") { Task { await model.signIn() } }
                        .disabled(model.signingIn)
                }
            } detail: {
                Agents(model: model)
                    .toolbar {
                        if model.update != nil {
                            Button("Restart to Complete Update") { model.restartToUpdate() }
                                .buttonStyle(.borderless)
                                .font(.callout)
                                .foregroundStyle(.secondary)
                        }
                    }
            }
        }
    }
}

private struct ProfileRow: View {
    let profile: ProfileView

    var body: some View {
        HStack(spacing: 8) {
            Picture(url: URL(string: profile.account.picture), name: name)
            VStack(alignment: .leading) {
                Text(name)
                if !profile.account.email.isEmpty {
                    Text(profile.account.email).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }

    private var name: String {
        if !profile.account.name.isEmpty { return profile.account.name }
        return profile.label
    }
}

/// The account's picture, or its initial while there is none.
private struct Picture: View {
    let url: URL?
    let name: String

    var body: some View {
        AsyncImage(url: url) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            Text(name.prefix(1).uppercased())
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(.quaternary)
        }
        .frame(width: 28, height: 28)
        .clipShape(Circle())
    }
}

private struct Agents: View {
    let model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let problem = model.problem {
                Text(problem).foregroundStyle(.red).padding()
            }
            if let profile = model.profiles.first(where: { $0.id == model.selected }) {
                HStack {
                    Text(profile.account.binding == .signedIn ? "Signed in" : "Not signed in")
                        .foregroundStyle(.secondary)
                    Spacer()
                    if profile.account.binding == .signedIn {
                        Button("Sign Out") { Task { await model.signOut() } }
                    }
                }
                .padding()
            }
            List(Array(model.agents.enumerated()), id: \.offset) { _, name in
                Text(name)
            }
            .overlay {
                if model.agents.isEmpty { Text("No agents").foregroundStyle(.secondary) }
            }
        }
    }
}
