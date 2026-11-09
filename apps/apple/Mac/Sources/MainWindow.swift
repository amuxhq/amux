import AmuxCore
import AmuxMacScreens
import AmuxValues
import SwiftUI

/// The app's window: amux installing or attaching, then setting up while
/// it is under way, then the profiles and their agents. The screens are
/// drawn from what the model holds and report back to it.
struct AppWindow: View {
    @Bindable var model: AppModel

    var body: some View {
        switch model.phase {
        case .installing: launch(.installing)
        case .attaching: launch(.attaching)
        case .failed(let reason): launch(.failed(reason))
        case .attached:
            if let setup = model.setup, let content = setup.content {
                SetupHost(setup: setup, content: content)
            } else {
                MainScreen(
                    content: main, selected: $model.selected, selectedAgent: $model.selectedAgent,
                    actions: actions)
            }
        }
    }

    private func launch(_ state: LaunchState) -> some View {
        LaunchScreen(
            state: state, updateReady: model.update != nil,
            retry: { Task { await model.start() } }, restartToUpdate: model.restartToUpdate)
    }

    private var main: MainContent {
        MainContent(
            profiles: model.profiles, agents: model.agents, toFinish: model.setup?.toFinish ?? 0,
            problem: model.problem, note: model.note, signingIn: model.signingIn,
            updateReady: model.update != nil)
    }

    private var actions: MainActions {
        MainActions(
            signIn: { Task { await model.signInFromMenu() } },
            signOut: { Task { await model.signOut() } },
            finishSettingUp: { model.setup?.finishSettingUp() },
            dismissFinishing: { model.setup?.dismissFinishing() },
            restartToUpdate: model.restartToUpdate)
    }
}

private struct SetupHost: View {
    @Bindable var setup: Setup
    let content: SetupContent

    var body: some View {
        SetupScreen(content: content, choices: $setup.choices, actions: setup.actions)
    }
}
