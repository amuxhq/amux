import AmuxMacScreens
import SwiftUI

/// The Settings window. Every change is applied at once through the daemon,
/// which keeps them in amux's config file, so `amux config` and this window
/// always agree.
struct SettingsWindow: View {
    let model: AppModel
    /// This Mac's repositories, as its entry last pinned them.
    @State private var repositories: [String] = []

    var body: some View {
        SettingsScreen(
            state: state,
            account: SettingsAccount(profile: model.current, signingIn: model.signingIn),
            repositories: repositories,
            actions: SettingsActions(
                apply: { await model.apply($0) },
                pin: { path, pinned in
                    let failed = await model.pin(path, pinned: pinned)
                    await loadRepositories()
                    return failed
                },
                signIn: { Task { await model.signInFromMenu() } },
                signOut: { Task { await model.signOut() } }))
            // Changed beside the app (`amux config`, the terminal's settings),
            // so read again whenever the window is opened.
            .task {
                await model.loadSettings()
                await loadRepositories()
            }
            // While amux restarts to apply an experiment switch, read again
            // until it runs what the switch says.
            .task(id: applying) {
                while applying, !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    await model.loadSettings()
                }
            }
    }

    /// Reads the pinned folders again. A pin reaches this Mac's entry a
    /// moment after it is answered, so the list is read until it moves.
    private func loadRepositories() async {
        let before = repositories
        for _ in 0..<10 {
            repositories = await model.folders()?.repositories.map(\.path) ?? []
            if repositories != before { return }
            try? await Task.sleep(for: .milliseconds(200))
        }
    }

    private var applying: Bool { model.settings?.applyingExperiments ?? false }

    private var state: SettingsState {
        if let settings = model.settings { return .ready(settings) }
        if case .failed(let reason) = model.phase { return .failed(reason) }
        return .connecting
    }
}
