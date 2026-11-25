import AmuxMacScreens
import SwiftUI

/// The Settings window. Every change is applied at once through the daemon,
/// which keeps them in amux's config file, so `amux config` and this window
/// always agree.
struct SettingsWindow: View {
    let model: AppModel

    var body: some View {
        SettingsScreen(
            state: state,
            account: SettingsAccount(profile: model.current, signingIn: model.signingIn),
            repositories: model.settings?.repositories ?? [],
            actions: SettingsActions(
                apply: { await model.apply($0) },
                pin: { path, pinned in
                    let failed = await model.pin(path, pinned: pinned)
                    // The config now pins it or not; the settings say so.
                    await model.loadSettings()
                    return failed
                },
                signIn: { Task { await model.signInFromMenu() } },
                signOut: { Task { await model.signOut() } }))
            // Changed beside the app (`amux config`, the terminal's settings),
            // so read again whenever the window is opened.
            .task { await model.loadSettings() }
            // While amux restarts to apply an experiment switch, read again
            // until it runs what the switch says.
            .task(id: applying) {
                while applying, !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    await model.loadSettings()
                }
            }
    }

    private var applying: Bool { model.settings?.applyingExperiments ?? false }

    private var state: SettingsState {
        if let settings = model.settings { return .ready(settings) }
        if case .failed(let reason) = model.phase { return .failed(reason) }
        return .connecting
    }
}
