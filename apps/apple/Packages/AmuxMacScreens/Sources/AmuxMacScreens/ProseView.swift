import AmuxValues
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
                Block(block: block, streaming: streaming)
            }
        }
        .font(.ui(.prose))
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
    /// Whether the text is still arriving, so a code block it ends inside
    /// is the one still being written.
    let streaming: Bool

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(Inline.styled(text))
                .font(.ui(level <= 1 ? .proseHeading : level == 2 ? .proseSubheading : .prose, weight: .semibold))
                .padding(.top, 4)
        case .paragraph(let text):
            Text(Inline.styled(text)).fixedSize(horizontal: false, vertical: true)
        case .list(_, let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(item.marker)
                            .foregroundStyle(.secondary)
                            .frame(minWidth: 14, alignment: .trailing)
                        Text(Inline.styled(item.text)).fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 18)
                }
            }
        case .code(let language, let text, let closed):
            CodeBlock(language: language, text: text, streaming: streaming && !closed)
        case .quote(let lines):
            HStack(spacing: 10) {
                Rectangle().fill(Palette.line).frame(width: 3)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in Text(Inline.styled(line)) }
                }
                .foregroundStyle(.secondary)
            }
        case .table(let header, let rows):
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                        Text(Inline.styled(cell)).fontWeight(.semibold)
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Text(Inline.styled(cell)) }
                    }
                }
            }
            .font(.ui(.callout))
        case .rule:
            Divider()
        }
    }
}

/// Inline code a size smaller than the prose around it, on a light chip, so
/// a name reads as code without shouting over the sentence.
@MainActor
enum Inline {
    static func styled(_ text: AttributedString) -> AttributedString {
        var text = text
        for run in text.runs where run.inlinePresentationIntent?.contains(.code) == true {
            text[run.range].font = .mono(.callout)
            text[run.range].backgroundColor = Palette.well
        }
        return text
    }
}

/// Code that keeps its whitespace, in a well, with its language and a copy
/// button showing under the pointer. Its tokens take their colours as the
/// shared view colours them; it draws what it has at once, plain until the
/// first colours come back, and its task asks again while they are
/// unsettled.
private struct CodeBlock: View {
    @Environment(\.codeHighlighting) private var highlighting
    let language: String?
    let text: String
    /// The block still being written: the open fence of text still
    /// arriving.
    let streaming: Bool
    @State private var hovering = false
    @State private var copied = false
    @State private var answer: HighlightedCode?

    private var request: CodeHighlighting.Request {
        .block(code: text, info: language ?? "", streaming: streaming)
    }

    private var code: AttributedString {
        let lines = CodeText.lines(text)
        let runs = (highlighting.kept(request) ?? answer)?.runs(lines) ?? HighlightedCode.plain(lines)
        return CodeText.attributed(lines: runs, Palette.syntax)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                HStack {
                    Text(language).font(.ui(.small)).foregroundStyle(.tertiary)
                    Spacer()
                    copy.opacity(hovering ? 1 : 0)
                }
                .padding(.leading, 14)
                .padding(.trailing, 6)
                .frame(height: 30)
                Rectangle().fill(Palette.line).frame(height: 1)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.mono(.callout))
                    .lineSpacing(3)
                    .fixedSize()
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
            }
            .overlay(alignment: .topTrailing) {
                if language?.isEmpty ?? true, hovering {
                    copy.padding(6)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.well))
        .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.line))
        .onHover { inside in
            hovering = inside
            if !inside { copied = false }
        }
        .task(id: request) { await highlighting.follow(request) { answer = $0 } }
    }

    private var copy: some View {
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
}
