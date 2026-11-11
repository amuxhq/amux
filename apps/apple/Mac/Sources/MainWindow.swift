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
                MainScreen(content: main, place: $model.place, actions: actions) {
                    if model.place.surface == .agents, let chat = model.chat, let agent = model.place.agent {
                        ChatPane(
                            chat: chat, row: model.stores?.fleet.row(agent),
                            inspectorOpen: model.place.inspector != nil,
                            openInspector: { model.place.inspector = $0 })
                        .id(agent)
                    } else {
                        EmptyCentre(surface: model.place.surface) { model.newAgent() }
                    }
                } inspector: {
                    if let chat = model.chat, let agent = model.place.agent, let tab = model.place.inspector {
                        InspectorPane(
                            tab: tab, chat: chat, family: model.family(agent),
                            now: model.stores?.fleet.orderedAt ?? Date(),
                            openAgent: { model.place.agent = $0 })
                    }
                }
            }
        }
    }

    private func launch(_ state: LaunchState) -> some View {
        LaunchScreen(
            state: state, updateReady: model.update != nil,
            retry: { Task { await model.start() } }, restartToUpdate: model.restartToUpdate)
    }

    private var main: MainContent {
        let stores = model.stores
        return MainContent(
            profiles: model.profiles, profile: model.selected,
            fleet: FleetContent(
                sections: stores?.fleet.sections ?? [], loaded: stores?.fleet.loaded ?? false,
                now: stores?.fleet.orderedAt ?? Date()),
            hosts: stores?.hosts.hosts ?? [], thisMac: stores?.hosts.local,
            toFinish: model.setup?.toFinish ?? 0, problem: model.problem, note: model.note,
            signingIn: model.signingIn, updateReady: model.update != nil)
    }

    private var actions: MainActions {
        MainActions(
            openProfile: { model.selected = $0 },
            signIn: { Task { await model.signInFromMenu() } },
            signOut: { Task { await model.signOut() } },
            finishSettingUp: { model.setup?.finishSettingUp() },
            dismissFinishing: { model.setup?.dismissFinishing() },
            restartToUpdate: model.restartToUpdate,
            newAgent: { model.newAgent() },
            search: { model.stores?.filterFleet(FleetFilter(words: $0, only: nil)) },
            toggleFamily: { model.stores?.toggleFamily($0) },
            act: { act, agent in Task { await model.act(act, on: agent) } })
    }
}

private struct SetupHost: View {
    @Bindable var setup: Setup
    let content: SetupContent

    var body: some View {
        SetupScreen(content: content, choices: $setup.choices, actions: setup.actions)
    }
}
