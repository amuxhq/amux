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
                    detail
                } inspector: {
                    changes
                }
                .onChange(of: model.stores?.newAgent.created) { _, created in
                    if let created, model.starting != nil { model.started(created) }
                }
            }
        }
    }

    /// The centre: the open agent's chat, the new-agent page, or a host.
    @ViewBuilder private var detail: some View {
        if model.place.surface == .agents, let chat = model.chat, let agent = model.place.agent {
            let row = model.stores?.fleet.row(agent)
            ChatPane(
                chat: chat, row: row, family: model.family(agent),
                changesOpen: model.place.showsChanges(native: row?.native == true), actions: agentActions(agent)
            )
            .id(agent)
        } else if model.place.surface == .agents, model.place.agent == nil {
            newAgentPage
        } else if model.place.surface == .hosts, let id = model.place.host, let host = host(id) {
            hostPage(host, id: id)
        } else {
            EmptyCentre(surface: model.place.surface) { model.newAgent() }
        }
    }

    @ViewBuilder private var newAgentPage: some View {
        if let starting = model.starting {
            NewAgentPage(surface: starting)
        } else {
            Color.clear.onAppear { model.prepareNewAgent() }
        }
    }

    private func hostPage(_ host: HostView, id: [UInt8]) -> some View {
        HostPage(
            host: host,
            agents: model.stores?.fleet.rows.filter { $0.card.agent.host == id } ?? [],
            now: model.stores?.fleet.orderedAt ?? Date(),
            openAgent: { model.openChat($0) },
            toggleFamily: { model.stores?.toggleFamily($0) },
            revoke: { model.revoke($0) }
        )
        .id(id)
    }

    @ViewBuilder private var changes: some View {
        if let chat = model.chat, model.changesShown {
            ChangesPane(chat: chat) { model.place.changes = false }
        }
    }

    /// What the card and the chat can do about the open agent.
    private func agentActions(_ agent: AgentKey) -> AgentActions {
        let model = model
        var actions = AgentActions(
            openChanges: { model.place.changes = true }, openAgent: { model.openChat($0) },
            openTerminal: { model.openTerminal(agent) }, editor: AppModel.editor?.name ?? "Editor",
            editorIcon: AppModel.editor.map { NSWorkspace.shared.icon(forFile: $0.url.path) },
            terminalIcon: AppModel.appIcon("com.apple.Terminal"), finderIcon: AppModel.appIcon("com.apple.finder"))
        if let folder = model.local(agent) {
            actions.reveal = { model.reveal(folder) }
            actions.openEditor = { model.openInEditor(folder) }
        }
        return actions
    }

    /// A host on the hosts surface, this Mac among them.
    private func host(_ id: [UInt8]) -> HostView? {
        guard let hosts = model.stores?.hosts else { return nil }
        return ([hosts.local].compactMap { $0 } + hosts.hosts).first { $0.hostId == id }
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
            act: { act, agent in Task { await model.act(act, on: agent) } },
            dismissProblem: { model.dismissProblem() },
            openTerminal: { model.openTerminal($0) })
    }
}

private struct SetupHost: View {
    @Bindable var setup: Setup
    let content: SetupContent

    var body: some View {
        SetupScreen(content: content, choices: $setup.choices, actions: setup.actions)
    }
}
