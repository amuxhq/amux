import AmuxValues
import AmuxWords
import SwiftUI

/// The agent's model, effort and mode, picked in one panel rather than a
/// long menu: the providers down the left when the agent is not started
/// yet, the provider's models in the middle with what each is for and a
/// search once there are many, and the chosen model's efforts and the
/// agent's modes on the right. A pick applies at once and the panel stays,
/// so a model and its effort are chosen in one visit.
struct ModelPicker: View {
    /// A provider the agent could be, for a new agent; none once it runs.
    struct ProviderItem: Identifiable {
        let provider: Provider
        let name: String
        let glyph: String
        /// Why it can't be picked now: "Not signed in on build-box".
        var note: String?
        var id: Provider { provider }
    }

    var providers: [ProviderItem] = []
    var provider: Provider?
    let settings: SettingsView?
    var chooseProvider: (Provider) -> Void = { _ in }
    let chooseModel: (String) -> Void
    let chooseEffort: (String) -> Void
    let chooseMode: (String) -> Void

    @State private var query = ""

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            if providers.count > 1 {
                providerColumn
                Rectangle().fill(Palette.line).frame(width: 1)
            }
            modelColumn
            if hasSide {
                Rectangle().fill(Palette.line).frame(width: 1)
                sideColumn
            }
        }
        .font(.ui(.body))
        .fixedSize(horizontal: false, vertical: true)
    }

    private var models: [ModelChoice] { settings?.models ?? [] }

    private var shown: [ModelChoice] {
        let words = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !words.isEmpty else { return models }
        return models.filter {
            $0.displayName.lowercased().contains(words) || $0.description.lowercased().contains(words)
        }
    }

    private var efforts: [EffortChoice] { settings?.efforts ?? [] }
    private var modes: [ModeChoice] { (settings?.modes ?? []).filter(\.settable) }
    private var hasSide: Bool { !efforts.isEmpty || !modes.isEmpty }

    // MARK: - Columns

    private var providerColumn: some View {
        VStack(alignment: .leading, spacing: 2) {
            ColumnHead("Agent")
            ForEach(providers) { item in
                PickerRow(selected: item.provider == provider, enabled: item.note == nil) {
                    chooseProvider(item.provider)
                } content: {
                    HStack(spacing: 8) {
                        Icon(item.glyph, size: 14, brand: true)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(item.name)
                            if let note = item.note {
                                Text(note).font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
                            }
                        }
                    }
                }
            }
        }
        .padding(6)
        .frame(width: 150, alignment: .topLeading)
    }

    private var modelColumn: some View {
        VStack(alignment: .leading, spacing: 2) {
            if models.count > 6 {
                HStack(spacing: 6) {
                    Icon("search", size: 13).foregroundStyle(Palette.textTertiary)
                    TextField("Search models", text: $query).textFieldStyle(.plain)
                }
                .padding(.horizontal, 8)
                .frame(height: 28)
                .background(RoundedRectangle(cornerRadius: Radius.control, style: .continuous).fill(Palette.well))
                .padding(.bottom, 4)
            } else {
                ColumnHead("Model")
            }
            if settings == nil {
                Text("Loading models…")
                    .foregroundStyle(Palette.textSecondary)
                    .padding(8)
            }
            ForEach(shown, id: \.value) { choice in
                PickerRow(selected: choice.current, enabled: settings?.changeable.model == .pick) {
                    chooseModel(choice.value)
                } content: {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(choice.displayName).foregroundStyle(Palette.text)
                        if !choice.description.isEmpty {
                            Text(choice.description)
                                .font(.ui(.meta))
                                .foregroundStyle(Palette.textSecondary)
                                .lineLimit(2)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
            if !query.isEmpty, shown.isEmpty {
                Text("No matching models").foregroundStyle(Palette.textSecondary).padding(8)
            }
        }
        .padding(6)
        .frame(width: 260, alignment: .topLeading)
    }

    private var sideColumn: some View {
        VStack(alignment: .leading, spacing: 2) {
            if !efforts.isEmpty {
                ColumnHead("Effort")
                ForEach(efforts, id: \.value) { choice in
                    PickerRow(selected: choice.current, enabled: settings?.changeable.effort == .pick) {
                        chooseEffort(choice.value)
                    } content: {
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(choice.value.capitalized)
                            if choice.default {
                                Text("Default").font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
                            }
                        }
                    }
                }
            }
            if !modes.isEmpty {
                ColumnHead("Mode").padding(.top, efforts.isEmpty ? 0 : 6)
                ForEach(modes, id: \.value) { choice in
                    PickerRow(selected: choice.current, enabled: settings?.changeable.mode == .pick) {
                        chooseMode(choice.value)
                    } content: {
                        Text(ChatWords.mode(choice))
                    }
                }
            }
        }
        .padding(6)
        .frame(width: 150, alignment: .topLeading)
    }
}

/// A column's quiet heading.
private struct ColumnHead: View {
    let title: String
    init(_ title: String) { self.title = title }

    var body: some View {
        Text(title)
            .font(.ui(.caption, weight: .medium))
            .foregroundStyle(Palette.textTertiary)
            .padding(.horizontal, 8)
            .frame(height: 24, alignment: .leading)
    }
}

/// One pick: lit under the pointer, a check at the end of the current one.
private struct PickerRow<Content: View>: View {
    let selected: Bool
    let enabled: Bool
    let pick: () -> Void
    @ViewBuilder let content: Content
    @State private var hovering = false

    init(selected: Bool, enabled: Bool, pick: @escaping () -> Void, @ViewBuilder content: () -> Content) {
        self.selected = selected
        self.enabled = enabled
        self.pick = pick
        self.content = content()
    }

    var body: some View {
        Button(action: pick) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                content.frame(maxWidth: .infinity, alignment: .leading)
                Icon("check", size: 12)
                    .foregroundStyle(Palette.accent)
                    .opacity(selected ? 1 : 0)
                    .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4.5 }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                    .fill(hovering && enabled ? Palette.hover : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
        .onHover { hovering = $0 }
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}
