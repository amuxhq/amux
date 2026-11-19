import AmuxWords
import SwiftUI

/// The agent's prose, its markdown drawn as blocks: headings, lists, code
/// that keeps its whitespace, quotes and tables.
struct ProseView: View {
    let source: String
    let streaming: Bool
    /// A working note: said in passing, quieter than an answer.
    let quiet: Bool

    var body: some View {
        let document = Parsed.document(source)
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(document.blocks.enumerated()), id: \.offset) { _, block in
                Block(block: block)
            }
        }
        .font(.ui(.prose))
        .lineSpacing(6)
        .foregroundStyle(quiet ? AnyShapeStyle(Palette.textSecondary) : AnyShapeStyle(Palette.prose))
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Parsed documents, kept by their source so a row drawn again is not
/// parsed again.
@MainActor
private enum Parsed {
    private static var kept: [String: MarkdownDocument] = [:]
    private static var order: [String] = []

    static func document(_ source: String) -> MarkdownDocument {
        if let document = kept[source] { return document }
        let document = MarkdownDocument.parse(source)
        kept[source] = document
        order.append(source)
        if order.count > 400 { kept.removeValue(forKey: order.removeFirst()) }
        return document
    }
}

private struct Block: View {
    let block: MarkdownBlock

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Inline.text((text))
                .font(.ui(level <= 1 ? .proseHeading : level == 2 ? .proseSubheading : .prose, weight: .semibold))
                .foregroundStyle(Palette.text)
                .padding(.top, 6)
        case .paragraph(let text):
            Inline.text((text)).fixedSize(horizontal: false, vertical: true)
        case .list(_, let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(item.marker)
                            .foregroundStyle(Palette.textSecondary)
                            .frame(minWidth: 14, alignment: .trailing)
                        Inline.text((item.text)).fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 18)
                }
            }
        case .code(let language, let text):
            CodeBlock(language: language, text: text)
        case .quote(let lines):
            HStack(spacing: 10) {
                Rectangle().fill(Palette.line).frame(width: 3)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in Inline.text((line)) }
                }
                .foregroundStyle(Palette.textSecondary)
            }
        case .table(let header, let rows):
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                        Inline.text((cell)).fontWeight(.semibold)
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Inline.text((cell)) }
                    }
                }
            }
            .font(.ui(.meta))
        case .rule:
            Divider()
        }
    }
}

/// Inline code in the monospaced face a half step smaller than the prose
/// around it, on a faint chip. A rounded pill would need a custom text
/// renderer, and SwiftUI skips custom renderers on selectable text;
/// copying from an answer matters more than the chip's corners. Strong
/// emphasis is semibold, which reads as emphasis on a dark ground where
/// bold reads as shouting.
@MainActor
enum Inline {
    static func text(_ source: AttributedString) -> Text { Text(styled(source)) }

    static func styled(_ text: AttributedString) -> AttributedString {
        var text = text
        for run in text.runs {
            guard let intent = run.inlinePresentationIntent else { continue }
            if intent.contains(.code) {
                text[run.range].font = .mono(.callout)
                text[run.range].backgroundColor = Palette.codePill
            } else if intent.contains(.stronglyEmphasized) {
                text[run.range].font = .ui(.prose, weight: .semibold)
            }
        }
        return text
    }
}

/// Code that keeps its whitespace, set into the window: its language and
/// copy on a quiet header, the code under it.
private struct CodeBlock: View {
    let language: String?
    let text: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language ?? "").font(.ui(.meta)).foregroundStyle(Palette.textTertiary)
                Spacer()
                copy
            }
            .padding(.leading, 12)
            .padding(.trailing, 4)
            .frame(height: 26)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(text)
                    .font(.mono(.callout))
                    .lineSpacing(5)
                    .fixedSize()
                    .padding(.horizontal, 12)
                    .padding(.top, 2)
                    .padding(.bottom, 10)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
        .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
    }

    private var copy: some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            copied = true
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                copied = false
            }
        } label: {
            HStack(spacing: 4) {
                Icon(copied ? "check" : "copy", size: 12)
                if copied { Text("Copied").font(.ui(.caption)) }
            }
            .foregroundStyle(Palette.textTertiary)
            .padding(.horizontal, 6)
            .frame(height: 22)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
        .help("Copy")
    }
}
