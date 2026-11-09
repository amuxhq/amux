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
            actions: SettingsActions(
                apply: { await model.apply($0) },
                signIn: { Task { await model.signInFromMenu() } },
                signOut: { Task { await model.signOut() } }))
            // Changed beside the app (`amux config`, the terminal's settings),
            // so read again whenever the window is opened.
            .task { await model.loadSettings() }
    }

    private var state: SettingsState {
        if let settings = model.settings { return .ready(settings) }
        if case .failed(let reason) = model.phase { return .failed(reason) }
        return .connecting
    }
}
