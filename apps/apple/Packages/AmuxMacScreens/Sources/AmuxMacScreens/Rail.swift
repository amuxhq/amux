import AmuxValues
import SwiftUI

/// The column of surfaces down the window's left edge: amux's mark, then
/// the surfaces as tiles, the one open filled, with Settings and the
/// profile at its foot. It sits a step darker than the sidebar, as chrome
/// does, so it reads as the window's frame rather than as padding.
struct Rail: View {
    let content: MainContent
    @Binding var surface: Surface
    let actions: MainActions

    var body: some View {
        VStack(spacing: 6) {
            Mark()
                .padding(.top, 10)
                .padding(.bottom, 8)
            ForEach(Surface.allCases) { each in
                RailButton(surface: each, selected: surface == each, badge: badge(each)) { surface = each }
            }
            Spacer()
            SettingsLink {
                Icon("settings", size: 18)
                    .foregroundStyle(.secondary)
                    .frame(width: 36, height: 36)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help("Settings (⌘,)")
            ProfileMenu(content: content, actions: actions)
                .padding(.bottom, 12)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Palette.rail)
        .overlay(alignment: .trailing) { Rectangle().fill(Palette.line).frame(width: 1) }
    }

    /// The rail counts the agents that need the person, wherever they are.
    private func badge(_ surface: Surface) -> Int {
        guard surface == .agents else { return 0 }
        return content.fleet.live.flatMap(\.rows).filter { $0.card.familyAttention == .needsYou }.count
    }
}

/// amux's icon, small, at the top of the rail.
private struct Mark: View {
    var body: some View {
        Image(nsImage: Self.image)
            .resizable()
            .interpolation(.high)
            .frame(width: 28, height: 28)
            .clipShape(RoundedRectangle(cornerRadius: 7.5, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 7.5, style: .continuous).strokeBorder(Color.white.opacity(0.08)))
            .accessibilityHidden(true)
    }

    @MainActor static let image: NSImage = {
        let url = Bundle.module.url(forResource: "mark", withExtension: "png", subdirectory: "Brand")
        return url.flatMap { NSImage(contentsOf: $0) } ?? NSImage()
    }()
}

private struct RailButton: View {
    let surface: Surface
    let selected: Bool
    let badge: Int
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Icon(surface.symbol, size: 19)
                .foregroundStyle(selected ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                .frame(width: 38, height: 38)
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(selected ? Palette.raised : (hovering ? Palette.hover : .clear))
                        .shadow(color: .black.opacity(selected ? 0.18 : 0), radius: 2, y: 1))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(selected ? Palette.line : .clear))
                .contentShape(Rectangle())
                .overlay(alignment: .topTrailing) {
                    if badge > 0 {
                        Text("\(badge)")
                            .font(.system(size: 10, weight: .bold))
                            .monospacedDigit()
                            .foregroundStyle(.white)
                            .padding(.horizontal, 4.5)
                            .frame(minWidth: 16, minHeight: 16)
                            .background(Capsule().fill(Palette.you))
                            .overlay(Capsule().strokeBorder(Palette.rail, lineWidth: 2))
                            .offset(x: 6, y: -5)
                    }
                }
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(surface.title)
        .accessibilityLabel(badge > 0 ? "\(surface.title), \(badge) need you" : surface.title)
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
    var size: CGFloat = 30

    var body: some View {
        AsyncImage(url: profile.flatMap { URL(string: $0.account.picture) }) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            Text(initial)
                .font(.system(size: size * 0.42, weight: .semibold, design: .rounded))
                .foregroundStyle(.primary)
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
