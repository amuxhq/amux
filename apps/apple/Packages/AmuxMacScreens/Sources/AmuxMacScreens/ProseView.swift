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
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(document.blocks.enumerated()), id: \.offset) { _, block in
                Block(block: block)
            }
        }
        .font(.system(size: 14))
        .lineSpacing(4.5)
        .foregroundStyle(quiet ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
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
            Text(text)
                .font(.system(size: level <= 1 ? 18 : level == 2 ? 15.5 : 14, weight: .semibold))
                .padding(.top, 4)
        case .paragraph(let text):
            Text(text).fixedSize(horizontal: false, vertical: true)
        case .list(_, let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(item.marker)
                            .foregroundStyle(.secondary)
                            .frame(minWidth: 14, alignment: .trailing)
                        Text(item.text).fixedSize(horizontal: false, vertical: true)
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
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in Text(line) }
                }
                .foregroundStyle(.secondary)
            }
        case .table(let header, let rows):
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                        Text(cell).fontWeight(.semibold)
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Text(cell) }
                    }
                }
            }
            .font(.system(size: 12.5))
        case .rule:
            Divider()
        }
    }
}

/// Code that keeps its whitespace, in a well, with its language and a copy
/// button showing under the pointer.
private struct CodeBlock: View {
    let language: String?
    let text: String
    @State private var hovering = false
    @State private var copied = false

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .font(.system(size: 12.5, design: .monospaced))
                .lineSpacing(2)
                .fixedSize()
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.well))
        .overlay(alignment: .topTrailing) {
            if hovering {
                HStack(spacing: 4) {
                    if let language, !language.isEmpty {
                        Text(language).font(.system(size: 11)).foregroundStyle(.tertiary)
                    }
                    Button {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(text, forType: .string)
                        copied = true
                    } label: {
                        Icon(copied ? "check" : "copy", size: 13)
                            .foregroundStyle(.secondary)
                            .frame(width: 24, height: 22)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help(copied ? "Copied" : "Copy")
                }
                .padding(6)
            }
        }
        .onHover { inside in
            hovering = inside
            if !inside { copied = false }
        }
    }
}
