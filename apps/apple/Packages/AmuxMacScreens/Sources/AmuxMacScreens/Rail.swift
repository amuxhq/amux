import AmuxValues
import SwiftUI

/// The frame's left column: the surfaces as tiles, the one open drawn as
/// a small window of the same stuff as the windows beside it, with
/// Settings and the profile at its foot. It has no ground of its own: it is
/// the frame, and the window drags from its empty part.
struct Rail: View {
    let content: MainContent
    @Binding var surface: Surface
    let actions: MainActions

    var body: some View {
        VStack(spacing: 8) {
            ForEach(Surface.allCases) { each in
                RailButton(surface: each, selected: surface == each, badge: badge(each)) { surface = each }
            }
            Spacer()
            SettingsLink {
                Icon("settings", size: 18)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: Metrics.railButton, height: Metrics.railButton)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Settings (⌘,)")
            ProfileMenu(content: content, actions: actions)
                .frame(width: Metrics.railButton, height: Metrics.railButton)
                .padding(.bottom, Metrics.gap)
        }
        // The first button centres on the sidebar's search row.
        .padding(.top, (Metrics.bar - Metrics.railButton) / 2)
        .frame(width: Metrics.railButton)
        .frame(maxHeight: .infinity)
        .frame(width: Metrics.rail)
    }

    /// The rail counts the agents that need the person, wherever they are.
    private func badge(_ surface: Surface) -> Int {
        guard surface == .agents else { return 0 }
        return content.fleet.live.flatMap(\.rows).filter { $0.depth == 0 && $0.card.familyAttention == .needsYou }.count
    }
}

private struct RailButton: View {
    let surface: Surface
    let selected: Bool
    let badge: Int
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Icon(surface.symbol, size: 18).offset(y: surface == .agents ? 0.75 : 0)
                .foregroundStyle(selected ? AnyShapeStyle(.primary) : AnyShapeStyle(Palette.textTertiary))
                .frame(width: Metrics.railButton, height: Metrics.railButton)
                .background(
                    RoundedRectangle(cornerRadius: Radius.rail, style: .continuous)
                        .fill(selected || hovering ? Palette.hover : .clear))
                .contentShape(Rectangle())
                .overlay(alignment: .topTrailing) {
                    if badge > 0 {
                        Text("\(badge)")
                            .font(.system(size: 9, weight: .bold).monospacedDigit())
                            .foregroundStyle(.white)
                            .padding(.horizontal, 3.5)
                            .frame(minWidth: 14, minHeight: 14)
                            .background(Capsule().fill(Palette.accent))
                            .overlay(Capsule().stroke(Palette.frame, lineWidth: 1.5))
                            .offset(x: -3, y: 3)
                    }
                }
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(surface.title)
        .accessibilityLabel(badge > 0 ? "\(surface.title), \(badge) waiting for you" : surface.title)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The open profile's picture, which switches profiles and signs in or out.
private struct ProfileMenu: View {
    let content: MainContent
    let actions: MainActions

    var body: some View {
        Menu {
            if let current = content.current {
                Section(current.account.email.isEmpty ? Words.profileName(current) : current.account.email) {
                    if current.account.binding == .signedIn {
                        Button("Sign out") { actions.signOut() }
                    } else {
                        Button("Sign in…") { actions.signIn() }
                            .disabled(content.signingIn)
                    }
                }
            }
            if content.profiles.count > 1 {
                Section("Profiles") {
                    ForEach(content.profiles, id: \.id) { profile in
                        Toggle(
                            Words.profileName(profile),
                            isOn: Binding(
                                get: { profile.id == content.profile },
                                set: { if $0 { actions.openProfile(profile.id) } }))
                    }
                }
            }
        } label: {
            Picture(profile: content.current, size: 24)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(content.current.map(Words.profileName) ?? "Profile")
    }
}

/// The account's picture, or its initial while there is none.
struct Picture: View {
    let profile: ProfileView?
    var size: CGFloat = 28

    var body: some View {
        AsyncImage(url: profile.flatMap { URL(string: $0.account.picture) }) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            Text(initial)
                .font(.system(size: size * 0.42, weight: .semibold, design: .rounded))
                .foregroundStyle(.primary)
                // Rounded capitals centred by their line sit high.
                .offset(y: size * 0.04)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Palette.raised)
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().strokeBorder(Palette.line))
    }

    private var initial: String {
        profile.map { String(Words.profileName($0).prefix(1)).uppercased() } ?? "?"
    }
}

/// How the window words its own small things.
enum Words {
    static func profileName(_ profile: ProfileView) -> String {
        if !profile.account.name.isEmpty { return profile.account.name }
        return profile.label
    }
}
