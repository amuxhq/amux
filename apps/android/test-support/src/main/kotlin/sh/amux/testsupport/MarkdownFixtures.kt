package sh.amux.testsupport

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import sh.amux.values.AnswerView
import sh.amux.values.InlineRun
import sh.amux.values.Markdown
import sh.amux.values.OptionView
import sh.amux.values.ProseBlock
import sh.amux.values.ProsePart
import sh.amux.values.QuestionView

/**
 * Prose as the Rust parser reads it, for screens drawn from values alone.
 *
 * A chat row's words come parsed from the bridge, so the storybook and the
 * tests that build rows without it take the parse from
 * `markdown-fixtures.json`, which `cargo run -p ios-tools -- markdown-fixtures`
 * writes from the texts listed in `crates/ios-tools/src/markdown_fixtures.rs`.
 * The iPhone and Mac stories read the same file. A text is found by its
 * source, so a story keeps its words where it uses them.
 */
object MarkdownFixtures {
    @Serializable
    private data class Label(val source: String, val runs: List<InlineRun>)

    @Serializable
    private data class File(
        val finished: List<Markdown>,
        val streaming: List<Markdown>,
        val labels: List<Label>,
    )

    private val file: File by lazy {
        val stream = MarkdownFixtures::class.java.getResourceAsStream("/markdown-fixtures.json")
            ?: error("markdown-fixtures.json is missing from the test-support resources")
        val text = stream.use { it.readBytes().decodeToString() }
        Json { ignoreUnknownKeys = true }.decodeFromString(File.serializer(), text)
    }

    private val finished by lazy { file.finished.associateBy { it.source } }
    private val streaming by lazy { file.streaming.associateBy { it.source } }
    private val labels by lazy { file.labels.associate { it.source to it.runs } }

    /** [source] parsed; [streaming] for text still being written. A text not listed fails, naming the list. */
    fun parsed(source: String, streaming: Boolean = false): Markdown =
        (if (streaming) this.streaming else finished)[source]
            ?: error(
                "${quoted(source)} is not in the markdown fixtures: add it to " +
                    "crates/ios-tools/src/markdown_fixtures.rs and run " +
                    "`cargo run -p ios-tools -- markdown-fixtures`",
            )

    /** A label read for its inline markdown. One with no markup in it is its words; one with markup must be listed. */
    fun runs(label: String): List<InlineRun> {
        labels[label]?.let { return it }
        if (label.isEmpty()) return emptyList()
        check(label.none { it in "`*~[]<>\\" }) {
            "${quoted(label)} is not in the markdown fixtures' labels: add it to " +
                "crates/ios-tools/src/markdown_fixtures.rs and run " +
                "`cargo run -p ios-tools -- markdown-fixtures`"
        }
        return listOf(words(label))
    }

    private fun quoted(text: String): String = "\"" + text.replace("\n", "\\n") + "\""
}

/** A row's words, parsed, from the fixtures. */
fun prosePart(source: String, streaming: Boolean = false): ProsePart =
    ProsePart.Text(MarkdownFixtures.parsed(source, streaming))

/**
 * Words with no markup in them, as the one paragraph they parse to: a test's
 * sentence that is not drawn for its styling. Anything with markup comes from
 * [MarkdownFixtures].
 */
fun plainMarkdown(words: String): Markdown =
    Markdown(
        source = words,
        blocks = if (words.isEmpty()) emptyList() else listOf(ProseBlock.Paragraph(listOf(words(words)))),
    )

/** [text] in no style. */
fun words(text: String): InlineRun =
    InlineRun(text = text, strong = false, emphasis = false, strikethrough = false, code = false)

/** A question as a story writes it, its words read from the fixtures. */
fun QuestionView(
    header: String,
    question: String,
    multiSelect: Boolean,
    options: List<OptionView>,
    allowOther: Boolean,
    secret: Boolean,
): QuestionView = QuestionView(
    header = header,
    question = question,
    headerRuns = MarkdownFixtures.runs(header),
    questionRuns = MarkdownFixtures.runs(question),
    multiSelect = multiSelect,
    options = options,
    allowOther = allowOther,
    secret = secret,
)

/** An option as a story writes it, its words read from the fixtures. */
fun OptionView(label: String, description: String, preview: String, recommended: Boolean): OptionView =
    OptionView(
        label = label,
        description = description,
        preview = preview,
        recommended = recommended,
        labelRuns = MarkdownFixtures.runs(label),
        descriptionRuns = MarkdownFixtures.runs(description),
    )

/** An answer as a story writes it, its picks read from the fixtures. */
fun AnswerView(
    picked: List<String>,
    hidden: Boolean,
    skipped: Boolean,
    note: String? = null,
    other: String? = null,
): AnswerView = AnswerView(
    picked = picked,
    pickedRuns = picked.map(MarkdownFixtures::runs),
    hidden = hidden,
    skipped = skipped,
    note = note,
    other = other,
)
