import SwiftUI

/// The window before amux answers.
public enum LaunchState: Equatable, Sendable {
    case installing
    case attaching
    case failed(String)
}

/// amux installing, attaching, or not to be reached.
public struct LaunchScreen: View {
    let state: LaunchState
    let updateReady: Bool
    let retry: @MainActor () -> Void
    let restartToUpdate: @MainActor () -> Void

    public init(
        state: LaunchState, updateReady: Bool = false,
        retry: @escaping @MainActor () -> Void = {},
        restartToUpdate: @escaping @MainActor () -> Void = {}
    ) {
        self.state = state
        self.updateReady = updateReady
        self.retry = retry
        self.restartToUpdate = restartToUpdate
    }

    public var body: some View {
        switch state {
        case .installing:
            ProgressView("Installing amux")
        case .attaching:
            ProgressView("Connecting to amux")
        case .failed(let reason):
            VStack(spacing: 12) {
                Text("amux is not running").font(.headline)
                Text(reason).foregroundStyle(.secondary).textSelection(.enabled)
                Button("Try again") { retry() }
                if updateReady {
                    Button("Restart to Complete Update") { restartToUpdate() }
                }
            }
            .padding()
        }
    }
}
