import AppKit
import SwiftUI

/// The window before amux answers.
public enum LaunchState: Equatable, Sendable {
    case installing
    case attaching
    case failed(String)
}

/// amux installing, attaching, or not to be reached: the app's icon over
/// a line saying which, and when it cannot be reached, why and Try Again.
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
        VStack(spacing: 0) {
            Spacer(minLength: 24)
            VStack(spacing: 14) {
                // The bundle's own icon file: the application's icon is the
                // system's placeholder until the app has finished launching.
                Image(nsImage: Bundle.main.image(forResource: "AppIcon") ?? NSApplication.shared.applicationIconImage)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: 64, height: 64)
                    .accessibilityHidden(true)
                switch state {
                case .installing: waiting("Setting up amux…")
                case .attaching: waiting("Connecting…")
                case .failed(let reason): failed(reason)
                }
            }
            .frame(maxWidth: 440)
            .padding(.horizontal, Metrics.gutter)
            // A little above the middle, where the eye expects it.
            .padding(.bottom, 48)
            Spacer(minLength: 24)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .font(.ui(.body))
        .textTiers()
        .background(Palette.content.ignoresSafeArea())
        .windowChrome()
    }

    private func waiting(_ words: String) -> some View {
        HStack(spacing: 8) {
            Working(size: 12)
            Text(words).foregroundStyle(Palette.textSecondary)
        }
    }

    private func failed(_ reason: String) -> some View {
        VStack(spacing: 8) {
            Text("amux isn’t running")
                .font(.ui(.proseHeading, weight: .semibold))
            Text(Self.marked(reason))
                .foregroundStyle(Palette.textSecondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            HStack(spacing: 8) {
                if updateReady {
                    Button("Restart to finish updating") { restartToUpdate() }
                        .buttonStyle(PillButtonStyle())
                }
                Button("Try again") { retry() }
                    .buttonStyle(PillButtonStyle(prominent: true))
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.top, 12)
        }
    }

    /// The reason with any path in it (a socket, a folder) set in the
    /// monospaced face, so it reads as something to look for on disk.
    static func marked(_ reason: String) -> AttributedString {
        var marked = AttributedString()
        for (index, word) in reason.split(separator: " ", omittingEmptySubsequences: false).enumerated() {
            if index > 0 { marked += AttributedString(" ") }
            // A sentence's full stop or comma after a path is not part of it.
            let path = word.trimmingCharacters(in: CharacterSet(charactersIn: ".,;:)"))
            guard path.hasPrefix("/") || path.hasPrefix("~/"), let range = word.range(of: path) else {
                marked += AttributedString(String(word))
                continue
            }
            var mono = AttributedString(path)
            mono.font = .mono(.callout)
            marked += AttributedString(String(word[..<range.lowerBound]))
            marked += mono
            marked += AttributedString(String(word[range.upperBound...]))
        }
        return marked
    }
}
