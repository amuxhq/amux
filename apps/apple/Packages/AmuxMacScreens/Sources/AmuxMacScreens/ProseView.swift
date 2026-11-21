import AmuxValues
import AmuxWords
import SwiftUI

/// The agent's prose, its markdown drawn as blocks: headings, lists, code
/// that keeps its whitespace, quotes and tables. The blocks come parsed with
/// the row.
struct ProseView: View {
    let markdown: Markdown
    let streaming: Bool
    /// A working note: said in passing, quieter than an answer.
    let quiet: Bool

    var body: some View {
        Blocks(blocks: markdown.blocks, streaming: streaming)
            .font(.ui(.prose))
            .lineSpacing(6)
            .foregroundStyle(quiet ? AnyShapeStyle(Palette.textSecondary) : AnyShapeStyle(Palette.prose))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Blocks one under another: a document's, a quote's or a list item's.
private struct Blocks: View {
    let blocks: [ProseBlock]
    let streaming: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                Block(block: block, streaming: streaming)
            }
        }
    }
}

private struct Block: View {
    let block: ProseBlock
    /// Whether the text is still arriving, so a code block it ends inside
    /// is the one still being written.
    let streaming: Bool

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Inline.text(text)
                .font(.ui(level <= 1 ? .proseHeading : level == 2 ? .proseSubheading : .prose, weight: .semibold))
                .foregroundStyle(Palette.text)
                .padding(.top, 6)
        case .paragraph(let text):
            Inline.text(text).fixedSize(horizontal: false, vertical: true)
        case .list(let ordered, let start, _, let items):
            // One grid, so the marker column is as wide as the widest marker
            // and every item's words start at one edge: `9.` stands over
            // `10.`, numbers right-aligned. A nested list sits in its item's
            // words, so it is indented by the marker column.
            let markers = ProseText.markers(ordered: ordered, start: start, items: items)
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 6, verticalSpacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    GridRow {
                        Text(markers[index])
                            .foregroundStyle(Palette.textSecondary)
                            .gridColumnAlignment(.trailing)
                        Blocks(blocks: item.blocks, streaming: streaming)
                            .fixedSize(horizontal: false, vertical: true)
                            .modifier(DoneTask(done: item.task == true))
                    }
                }
            }
        case .code(let language, let code, let closed):
            CodeBlock(language: language.isEmpty ? nil : language, text: code, streaming: streaming && !closed)
        case .quote(let blocks):
            HStack(spacing: 10) {
                Rectangle().fill(Palette.line).frame(width: 3)
                Blocks(blocks: blocks, streaming: streaming)
                    .foregroundStyle(Palette.textSecondary)
            }
        case .table(let alignments, let header, let rows):
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { column, cell in
                        Inline.text(cell).fontWeight(.semibold)
                            .gridColumnAlignment(Self.alignment(alignments, column))
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Inline.text(cell) }
                    }
                }
            }
            .font(.ui(.meta))
        case .rule:
            Divider()
        }
    }

    private static func alignment(_ alignments: [TableAlign], _ column: Int) -> HorizontalAlignment {
        switch column < alignments.count ? alignments[column] : .none {
        case .center: .center
        case .right: .trailing
        case .left, .none: .leading
        }
    }
}

/// A finished task's words recede; any other item keeps the prose's ink.
private struct DoneTask: ViewModifier {
    let done: Bool

    func body(content: Content) -> some View {
        if done {
            content.foregroundStyle(Palette.textTertiary)
        } else {
            content
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
    static func text(_ runs: [InlineRun]) -> Text { Text(styled(ProseText.attributed(runs))) }

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
/// copy on a quiet header, the code under it. Its tokens take their
/// colours as the shared view colours them; it draws what it has at once,
/// plain until the first colours come back, and its task asks again while
/// they are unsettled.
private struct CodeBlock: View {
    @Environment(\.codeHighlighting) private var highlighting
    let language: String?
    let text: String
    /// The block still being written: the open fence of text still
    /// arriving.
    let streaming: Bool
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
            HStack {
                Text(language ?? "").font(.ui(.meta)).foregroundStyle(Palette.textTertiary)
                Spacer()
                copy
            }
            .padding(.leading, 12)
            .padding(.trailing, 4)
            .frame(height: 26)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
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
        .task(id: request) { await highlighting.follow(request) { answer = $0 } }
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
