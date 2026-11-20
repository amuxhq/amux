import AmuxValues
import SwiftUI

/// A tab in the tool column's strip.
struct ToolTabItem {
    let symbol: String
    let title: String
    /// Counts the tab carries beside its name: a review's +n −n.
    let counts: ChangeTotals?
}

/// The tools that can open in the tool column, for the strip's +: the ones
/// amux has, nothing it does not yet.
struct ToolsMenu {
    let reviewOpen: Bool
    let openReview: @MainActor () -> Void
    /// The agent's own interface in Terminal, outside the window.
    let openTerminal: (@MainActor () -> Void)?
}

/// The tool column's tabs on the title row above it, then + for another
/// tool, then whatever goes at the strip's end. Tabs share the strip
/// evenly up to their widest.
struct ToolStrip<End: View>: View {
    let tabs: [ToolTabItem]
    let selected: Int
    let close: @MainActor () -> Void
    let tools: ToolsMenu
    @ViewBuilder let end: End

    var body: some View {
        HStack(spacing: 4) {
            ForEach(Array(tabs.enumerated()), id: \.offset) { index, tab in
                ToolTab(item: tab, selected: index == selected, close: close)
                    .frame(maxWidth: 220)
            }
            Menu {
                Button {
                    tools.openReview()
                } label: {
                    Label("Review", systemImage: tools.reviewOpen ? "checkmark" : "plus.forwardslash.minus")
                }
                .keyboardShortcut("r", modifiers: [])
                .disabled(tools.reviewOpen)
                if let openTerminal = tools.openTerminal {
                    Divider()
                    Button("Open in Terminal ↗") { openTerminal() }
                        .keyboardShortcut("t", modifiers: [])
                }
            } label: {
                Icon("plus", size: 16, optical: false)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Open a tool")
            Spacer(minLength: 4)
            end
        }
        .padding(.leading, 8)
    }
}

/// One of the tool column's tabs: the one showing filled and ringed, the
/// others only words until the pointer is on them; its close button shows
/// on the one showing and under the pointer.
struct ToolTab: View {
    let item: ToolTabItem
    let selected: Bool
    var close: @MainActor () -> Void = {}
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 6) {
            Icon(item.symbol, size: 13)
                .foregroundStyle(selected ? Palette.textSecondary : Palette.textTertiary)
            Text(item.title)
                .font(.ui(.body))
                .foregroundStyle(selected ? Palette.text : Palette.textSecondary)
                .lineLimit(1)
            if let counts = item.counts, counts.files > 0 {
                ChangeCounts(changes: counts).font(.ui(.meta))
            }
            Spacer(minLength: 0)
            Button(action: close) {
                Icon("x", size: 12, optical: false)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: 18, height: 18)
                    .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .opacity(selected || hovering ? 1 : 0)
            .help("Close tab")
        }
        .padding(.leading, 10)
        .padding(.trailing, 5)
        .frame(height: 28)
        .background(
            RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                .fill(selected || hovering ? Palette.hover : .clear))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                .strokeBorder(selected ? Palette.edge : .clear, lineWidth: 0.5))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }
}

/// A small button in amux's blue on the title row, for the one thing the
/// window asks of the person there: restarting into an update. An action,
/// so a rounded square like the window's other buttons.
struct AccentButtonStyle: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.ui(.meta, weight: .medium))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .frame(height: 22)
            .background(
                RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                    .fill(Palette.accent.opacity(hovering ? 0.88 : 1)))
            .opacity(configuration.isPressed ? 0.75 : 1)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
    }
}
