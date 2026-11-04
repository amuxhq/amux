import AmuxCore
import AmuxValues
import SwiftUI

/// The app's window: amux installing or attaching, then setting up while
/// it is under way, then the profiles and their agents.
struct AppWindow: View {
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
            if let setup = model.setup, setup.step != nil {
                SetupScreen(setup: setup, model: model)
            } else {
                Main(model: model)
            }
        }
    }
}

struct Main: View {
    @Bindable var model: AppModel

    var body: some View {
        NavigationSplitView {
            List(model.profiles, id: \.id, selection: $model.selected) { profile in
                ProfileRow(profile: profile)
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 240)
            .toolbar {
                if model.current?.account.binding != .signedIn {
                    Button("Sign In") { Task { await model.signInFromMenu() } }
                        .disabled(model.signingIn)
                }
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

struct Agents: View {
    @Bindable var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let setup = model.setup {
                if setup.toFinish > 0 {
                    FinishSettingUp(setup: setup)
                }
                if setup.noticeOnMain {
                    UsageNoticeBar(setup: setup)
                }
            }
            if let problem = model.problem {
                Text(problem).foregroundStyle(.red).padding()
            }
            if let note = model.note {
                Text(note).foregroundStyle(.secondary).padding([.horizontal, .top])
            }
            if let profile = model.current {
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
            List(model.agents, id: \.agent, selection: $model.selectedAgent) { card in
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name.isEmpty ? "agent" : card.name)
                    Text(card.host).font(.caption).foregroundStyle(.secondary)
                }
            }
            .overlay {
                if model.agents.isEmpty { Text("No agents").foregroundStyle(.secondary) }
            }
        }
    }
}

/// "Finish Setting Up (n)", until those steps are done or it is dismissed.
private struct FinishSettingUp: View {
    let setup: Setup

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "checklist").foregroundStyle(.tint)
            Button("Finish Setting Up (\(setup.toFinish))") { setup.finishSettingUp() }
                .buttonStyle(.link)
            Spacer()
            Button {
                setup.dismissFinishing()
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

/// The analytics notice on the main window, when setting up did not open
/// and no client had shown it yet.
private struct UsageNoticeBar: View {
    let setup: Setup

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "chart.bar").foregroundStyle(.secondary)
            UsageNoticeLine()
                .multilineTextAlignment(.leading)
            Button("OK") { setup.noticeRead() }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.quaternary.opacity(0.5))
    }
}
