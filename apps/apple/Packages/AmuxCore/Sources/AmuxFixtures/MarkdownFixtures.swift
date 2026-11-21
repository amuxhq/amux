import AmuxValues
import Foundation

/// Prose as the Rust parser reads it, for screens drawn from values alone.
///
/// A chat row's words come parsed from the bridge, so a story, the
/// catalogue or a test that builds rows without it takes the parse from
/// `Fixtures/markdown-fixtures.json`, which `cargo run -p ios-tools --
/// markdown-fixtures` writes from the texts listed in
/// `crates/ios-tools/src/markdown_fixtures.rs`. A text is found by its
/// source, so a story keeps its words where it uses them.
public enum MarkdownFixtures {
    private struct File: Decodable {
        let finished: [Markdown]
        let streaming: [Markdown]
        let labels: [Label]
    }

    private struct Label: Decodable {
        let source: String
        let runs: [InlineRun]
    }

    private static let file: File = {
        // A package bundles the file in its folder; the debug app, which
        // compiles this source itself to keep it out of a release, copies
        // it to the app's own resources.
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle.main
        #endif
        guard let url = bundle.url(forResource: "markdown-fixtures", withExtension: "json", subdirectory: "Fixtures")
            ?? bundle.url(forResource: "markdown-fixtures", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let file = try? JSONDecoder().decode(File.self, from: data)
        else { fatalError("Fixtures/markdown-fixtures.json is missing or unreadable") }
        return file
    }()

    private static let finished = Dictionary(file.finished.map { ($0.source, $0) }, uniquingKeysWith: { a, _ in a })
    private static let streaming = Dictionary(file.streaming.map { ($0.source, $0) }, uniquingKeysWith: { a, _ in a })
    private static let labels = Dictionary(file.labels.map { ($0.source, $0.runs) }, uniquingKeysWith: { a, _ in a })

    /// `source` parsed; `streaming` for text still being written. A text
    /// not listed stops the program, naming the list it belongs in.
    public static func parsed(_ source: String, streaming: Bool = false) -> Markdown {
        guard let markdown = (streaming ? Self.streaming : finished)[source] else {
            fatalError(
                "\(source.debugDescription) is not in the markdown fixtures: add it to "
                    + "crates/ios-tools/src/markdown_fixtures.rs and run "
                    + "`cargo run -p ios-tools -- markdown-fixtures`")
        }
        return markdown
    }

    /// A label read for its inline markdown. One with no markup in it is
    /// its words; one with markup must be listed, as a text must.
    public static func runs(_ label: String) -> [InlineRun] {
        if let runs = labels[label] { return runs }
        if label.isEmpty { return [] }
        guard !label.contains(where: { "`*~[]<>\\".contains($0) }) else {
            fatalError(
                "\(label.debugDescription) is not in the markdown fixtures' labels: add it to "
                    + "crates/ios-tools/src/markdown_fixtures.rs and run "
                    + "`cargo run -p ios-tools -- markdown-fixtures`")
        }
        return [InlineRun(words: label)]
    }
}

extension QuestionView {
    /// A question as a story writes it, its words read from the fixtures.
    public init(
        header: String, question: String, multiSelect: Bool, options: [OptionView], allowOther: Bool,
        secret: Bool
    ) {
        self.init(
            header: header, question: question, headerRuns: MarkdownFixtures.runs(header),
            questionRuns: MarkdownFixtures.runs(question), multiSelect: multiSelect, options: options,
            allowOther: allowOther, secret: secret)
    }
}

extension OptionView {
    /// An option as a story writes it, its words read from the fixtures.
    public init(label: String, description: String, preview: String, recommended: Bool) {
        self.init(
            label: label, description: description, preview: preview, recommended: recommended,
            labelRuns: MarkdownFixtures.runs(label), descriptionRuns: MarkdownFixtures.runs(description))
    }
}

extension AnswerView {
    /// An answer as a story writes it, its picks read from the fixtures.
    public init(picked: [String], hidden: Bool, skipped: Bool, note: String?, other: String?) {
        self.init(
            picked: picked, pickedRuns: picked.map(MarkdownFixtures.runs), hidden: hidden, skipped: skipped,
            note: note, other: other)
    }
}

extension Markdown {
    /// Words with no markup in them, as the one paragraph they parse to:
    /// a test's sentence that is not drawn for its styling. Anything with
    /// markup comes from ``MarkdownFixtures``.
    public init(words: String) {
        self.init(
            source: words,
            blocks: words.isEmpty ? [] : [.paragraph(text: [InlineRun(words: words)])])
    }
}

extension InlineRun {
    /// `words` in no style.
    public init(words: String) {
        self.init(text: words, strong: false, emphasis: false, strikethrough: false, code: false, link: nil)
    }
}

extension ProsePart {
    /// A row's words, parsed, from the fixtures.
    public static func fixture(_ source: String, streaming: Bool = false) -> ProsePart {
        .text(MarkdownFixtures.parsed(source, streaming: streaming))
    }
}
