import AmuxValues
import SwiftUI

/// What the main window shows: the profiles and the open one's agents.
public struct MainContent {
    public var profiles: [ProfileView]
    public var agents: [FleetCard]
    /// How many setting-up steps the window offers to finish.
    public var toFinish: UInt32
    public var problem: String?
    public var note: String?
    public var signingIn: Bool
    public var updateReady: Bool

    public init(
        profiles: [ProfileView], agents: [FleetCard], toFinish: UInt32 = 0, problem: String? = nil,
        note: String? = nil, signingIn: Bool = false, updateReady: Bool = false
    ) {
        self.profiles = profiles
        self.agents = agents
        self.toFinish = toFinish
        self.problem = problem
        self.note = note
        self.signingIn = signingIn
        self.updateReady = updateReady
    }
}

/// What the main window can ask for.
public struct MainActions {
    public var signIn: @MainActor () -> Void
    public var signOut: @MainActor () -> Void
    public var finishSettingUp: @MainActor () -> Void
    public var dismissFinishing: @MainActor () -> Void
    public var restartToUpdate: @MainActor () -> Void

    public init(
        signIn: @escaping @MainActor () -> Void = {},
        signOut: @escaping @MainActor () -> Void = {},
        finishSettingUp: @escaping @MainActor () -> Void = {},
        dismissFinishing: @escaping @MainActor () -> Void = {},
        restartToUpdate: @escaping @MainActor () -> Void = {}
    ) {
        self.signIn = signIn
        self.signOut = signOut
        self.finishSettingUp = finishSettingUp
        self.dismissFinishing = dismissFinishing
        self.restartToUpdate = restartToUpdate
    }
}

/// The profiles beside the open profile's agents.
public struct MainScreen: View {
    let content: MainContent
    @Binding var selected: String?
    @Binding var selectedAgent: AgentKey?
    let actions: MainActions

    public init(
        content: MainContent, selected: Binding<String?>, selectedAgent: Binding<AgentKey?>,
        actions: MainActions = MainActions()
    ) {
        self.content = content
        _selected = selected
        _selectedAgent = selectedAgent
        self.actions = actions
    }

    private var current: ProfileView? { content.profiles.first { $0.id == selected } }

    public var body: some View {
        NavigationSplitView {
            List(content.profiles, id: \.id, selection: $selected) { profile in
                ProfileRow(profile: profile)
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 240)
            .toolbar {
                if current?.account.binding != .signedIn {
                    Button("Sign In") { actions.signIn() }
                        .disabled(content.signingIn)
                }
            }
        } detail: {
            agents
                .toolbar {
                    if content.updateReady {
                        Button("Restart to Complete Update") { actions.restartToUpdate() }
                            .buttonStyle(.borderless)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
        }
    }

    private var agents: some View {
        VStack(alignment: .leading, spacing: 0) {
            if content.toFinish > 0 {
                FinishSettingUp(count: content.toFinish, actions: actions)
            }
            if let problem = content.problem {
                Text(problem).foregroundStyle(.red).padding()
            }
            if let note = content.note {
                Text(note).foregroundStyle(.secondary).padding([.horizontal, .top])
            }
            if let profile = current {
                HStack {
                    Text(profile.account.binding == .signedIn ? "Signed in" : "Not signed in")
                        .foregroundStyle(.secondary)
                    Spacer()
                    if profile.account.binding == .signedIn {
                        Button("Sign Out") { actions.signOut() }
                    }
                }
                .padding()
            }
            List(content.agents, id: \.agent, selection: $selectedAgent) { card in
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name.isEmpty ? "agent" : card.name)
                    Text(card.host).font(.caption).foregroundStyle(.secondary)
                }
            }
            .overlay {
                if content.agents.isEmpty { Text("No agents").foregroundStyle(.secondary) }
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

/// "Finish Setting Up (n)", until those steps are done or it is dismissed.
private struct FinishSettingUp: View {
    let count: UInt32
    let actions: MainActions

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "checklist").foregroundStyle(.tint)
            Button("Finish Setting Up (\(count))") { actions.finishSettingUp() }
                .buttonStyle(.link)
            Spacer()
            Button {
                actions.dismissFinishing()
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.quaternary.opacity(0.5))
    }
}
