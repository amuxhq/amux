import AmuxValues
import SwiftUI

/// The column of surfaces down the window's left edge, with Settings and
/// the profile at its foot.
struct Rail: View {
    let content: MainContent
    @Binding var surface: Surface
    let actions: MainActions

    var body: some View {
        VStack(spacing: 4) {
            ForEach(Surface.allCases) { each in
                RailButton(surface: each, selected: surface == each, badge: badge(each)) { surface = each }
            }
            Spacer()
            SettingsLink {
                Icon("settings", size: 17)
                    .foregroundStyle(.secondary)
                    .frame(width: 34, height: 32)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Settings")
            ProfileMenu(content: content, actions: actions)
                .padding(.bottom, 12)
        }
        .padding(.top, 6)
        .frame(maxHeight: .infinity)
    }

    /// The rail says how many agents need the person, wherever they are.
    private func badge(_ surface: Surface) -> Int {
        guard surface == .agents else { return 0 }
        return content.fleet.live.flatMap(\.rows).filter { $0.card.familyAttention == .needsYou }.count
    }
}

private struct RailButton: View {
    let surface: Surface
    let selected: Bool
    let badge: Int
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Icon(surface.symbol, size: 18)
                .foregroundStyle(selected ? .primary : .secondary)
                .frame(width: 34, height: 32)
                .contentShape(Rectangle())
                .overlay(alignment: .topTrailing) {
                    if badge > 0 {
                        Circle().fill(Color.accentColor).frame(width: 7, height: 7).offset(x: -5, y: 5)
                    }
                }
        }
        .buttonStyle(HoverButtonStyle(active: selected))
        .help(surface.title)
        .accessibilityLabel(surface.title)
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
                        Button("Sign Out") { actions.signOut() }
                    } else {
                        Button("Sign In…") { actions.signIn() }
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
            Picture(profile: content.current)
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
    var size: CGFloat = 26

    var body: some View {
        AsyncImage(url: profile.flatMap { URL(string: $0.account.picture) }) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            Text(initial)
                .font(.system(size: size * 0.42, weight: .medium))
                .foregroundStyle(.white)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.accentColor.gradient)
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
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
