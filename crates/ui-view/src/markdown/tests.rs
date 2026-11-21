//! The cases every client's own parser handled before this one replaced
//! them, and the choices where they disagreed.

use super::*;

fn blocks(source: &str) -> Vec<ProseBlock> {
    Markdown::parse(source, false).blocks
}

fn text(runs: &[InlineRun]) -> String {
    runs_text(runs)
}

fn paragraph(block: &ProseBlock) -> &[InlineRun] {
    match block {
        ProseBlock::Paragraph { text } => text,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

/// Each item's first paragraph's text.
fn item_texts(block: &ProseBlock) -> Vec<String> {
    match block {
        ProseBlock::List { items, .. } => items
            .iter()
            .map(|item| text(paragraph(&item.blocks[0])))
            .collect(),
        other => panic!("expected a list, got {other:?}"),
    }
}

fn open_blocks(source: &str) -> Vec<bool> {
    fn walk(blocks: &[ProseBlock], out: &mut Vec<bool>) {
        for block in blocks {
            match block {
                ProseBlock::Code { closed, .. } => out.push(!closed),
                ProseBlock::Quote { blocks } => walk(blocks, out),
                ProseBlock::List { items, .. } => {
                    for item in items {
                        walk(&item.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&blocks(source), &mut out);
    out
}

#[test]
fn headings_keep_their_level() {
    let parsed = blocks("## Why it broke");
    assert_eq!(
        parsed,
        vec![ProseBlock::Heading {
            level: 2,
            text: vec![InlineRun::plain("Why it broke")],
        }]
    );
}

#[test]
fn fenced_code_keeps_its_lines_and_its_language() {
    let parsed = blocks("```rust\nlet x = 1;\n    let y = 2;\n```");
    assert_eq!(
        parsed,
        vec![ProseBlock::Code {
            language: "rust".into(),
            code: "let x = 1;\n    let y = 2;".into(),
            closed: true,
        }]
    );
}

#[test]
fn a_fence_with_no_language_says_so_rather_than_guessing() {
    let parsed = blocks("```\nplain\n```");
    assert_eq!(
        parsed,
        vec![ProseBlock::Code {
            language: String::new(),
            code: "plain".into(),
            closed: true,
        }]
    );
}

#[test]
fn a_fence_s_language_is_the_first_word_of_its_info() {
    let ProseBlock::Code { language, .. } =
        &blocks("```rust title=\"main.rs\"\nfn main() {}\n```")[0]
    else {
        panic!("expected code");
    };
    assert_eq!(language, "rust");
}

/// An agent on Windows writes `\r\n`, and an old Mac file a lone `\r`.
/// Both end a line as `\n` does: nothing keeps the `\r`.
#[test]
fn windows_and_old_mac_line_endings_end_a_line() {
    let parsed =
        blocks("## Why\r\n\r\n```rust\r\nlet x = 1;\r\nlet y = 2;\r\n```\r\nDone.\rAgain.");
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    assert_eq!(
        parsed[0],
        ProseBlock::Heading {
            level: 2,
            text: vec![InlineRun::plain("Why")],
        }
    );
    assert_eq!(
        parsed[1],
        ProseBlock::Code {
            language: "rust".into(),
            code: "let x = 1;\nlet y = 2;".into(),
            closed: true,
        }
    );
    assert_eq!(text(paragraph(&parsed[2])), "Done.\nAgain.");
}

#[test]
fn only_a_last_fence_not_closed_yet_is_still_being_written() {
    for (source, open) in [
        ("Here:\n\n```rust\nfn a() {}\nfn b", vec![true]),
        ("```rust\nfn a() {}\n", vec![true]),
        ("```rust\n", vec![true]),
        ("```rust\nfn a() {}\n```", vec![false]),
        ("```rust\nfn a() {}\n```\n\n", vec![false]),
        ("```rust\nfn a() {}\n``", vec![true]),
        ("~~~\nx\n~~~", vec![false]),
        // A shorter fence inside a longer one does not close it.
        ("````md\n```\n", vec![true]),
        ("```a\nx\n```\n\nThen:\n\n```b\ny", vec![false, true]),
        ("- item\n\n  ```sh\n  ls\n  ```", vec![false]),
        ("> ```sh\n> ls\n> ```", vec![false]),
        // A quote's fence the quote has ended is finished.
        ("> ```sh\n> ls\n\nafter", vec![false]),
        // A fence indented four spaces is code, not a closing fence; three
        // spaces still close.
        ("```rust\nfn a() {}\n    ```\n", vec![true]),
        ("```rust\nfn a() {}\n   ```\n", vec![false]),
        // Outside a quote, a line starting `>` is code.
        ("```md\n> ```\n", vec![true]),
        ("```md\n> quoted\n", vec![true]),
        ("```rust\n```", vec![false]),
        ("```rust", vec![true]),
        // A list item's fence a dedented paragraph ends is finished.
        ("- ```sh\n  ls\n\nafter", vec![false]),
        // Indentation alone after the code is not a closing fence, though
        // the parser counts it as the block's.
        ("- ~~~rust\n  fn f() {}\n  ", vec![true]),
        ("- ~~~rust\n  fn f() {}\n  ~~~", vec![false]),
        ("```rust\nfn f() {}\n   ", vec![true]),
    ] {
        assert_eq!(open_blocks(source), open, "{source:?}");
    }
}

/// The empty line after the last newline of a block still being written
/// stays: it says the line before it is complete, which a streaming block
/// colours.
#[test]
fn an_open_block_keeps_its_last_newline() {
    assert_eq!(
        blocks("```rust\nfn a() {}\n"),
        vec![ProseBlock::Code {
            language: "rust".into(),
            code: "fn a() {}\n".into(),
            closed: false,
        }]
    );
}

#[test]
fn lists_keep_their_items_and_their_depth() {
    let parsed = blocks("- one\n- two\n  - nested");
    assert_eq!(parsed.len(), 1);
    let ProseBlock::List {
        ordered,
        depth,
        items,
        ..
    } = &parsed[0]
    else {
        panic!("expected a list");
    };
    assert!(!ordered);
    assert_eq!(*depth, 0);
    assert_eq!(item_texts(&parsed[0]), ["one", "two"]);
    // The nested list is the second item's, a level down.
    let nested = &items[1].blocks[1];
    assert!(
        matches!(nested, ProseBlock::List { depth: 1, .. }),
        "{nested:?}"
    );
    assert_eq!(item_texts(nested), ["nested"]);
}

#[test]
fn a_numbered_list_is_numbered_from_its_first_number() {
    let parsed = blocks("1. first\n2. second");
    assert!(matches!(
        parsed[0],
        ProseBlock::List {
            ordered: true,
            start: 1,
            ..
        }
    ));
    assert_eq!(item_texts(&parsed[0]), ["first", "second"]);

    // CommonMark numbers on from the first: every `1.` still counts up,
    // and a list may start anywhere.
    let parsed = blocks("9. nine\n1. ten");
    assert!(matches!(
        parsed[0],
        ProseBlock::List {
            ordered: true,
            start: 9,
            ..
        }
    ));
    assert_eq!(item_texts(&parsed[0]), ["nine", "ten"]);
}

/// A blank line between items makes a loose list, not two lists, so the
/// numbering carries on.
#[test]
fn items_parted_by_blank_lines_are_one_list() {
    let parsed = blocks("1. first\n\n2. second\n\n3. third");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    assert_eq!(item_texts(&parsed[0]), ["first", "second", "third"]);
}

#[test]
fn an_item_holds_its_code_and_its_paragraphs() {
    let parsed =
        blocks("1. Run it:\n\n   ```sh\n   cargo test\n   ```\n\n   Then read the output.");
    let ProseBlock::List { items, .. } = &parsed[0] else {
        panic!("expected a list");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(text(paragraph(&items[0].blocks[0])), "Run it:");
    assert_eq!(
        items[0].blocks[1],
        ProseBlock::Code {
            language: "sh".into(),
            code: "cargo test".into(),
            closed: true,
        }
    );
    assert_eq!(
        text(paragraph(&items[0].blocks[2])),
        "Then read the output."
    );
}

#[test]
fn a_task_list_says_which_tasks_are_done() {
    let parsed = blocks("- [x] built\n- [ ] tested\n- plain");
    let ProseBlock::List { items, .. } = &parsed[0] else {
        panic!("expected a list");
    };
    assert_eq!(
        items.iter().map(|item| item.task).collect::<Vec<_>>(),
        [Some(true), Some(false), None]
    );
    assert_eq!(item_texts(&parsed[0]), ["built", "tested", "plain"]);
}

/// A table's second row is its alignment rule, which is punctuation: it
/// gives the columns their alignment and is not a row.
#[test]
fn a_table_s_alignment_rule_is_not_a_row() {
    let parsed = blocks("| Layer | Reads |\n| --- | ---: |\n| claude | yes |\n| codex | no |");
    let ProseBlock::Table {
        alignments,
        header,
        rows,
    } = &parsed[0]
    else {
        panic!("expected a table, got {parsed:?}");
    };
    assert_eq!(alignments, &[TableAlign::None, TableAlign::Right]);
    assert_eq!(
        header.iter().map(|cell| text(cell)).collect::<Vec<_>>(),
        ["Layer", "Reads"]
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].iter().map(|cell| text(cell)).collect::<Vec<_>>(),
        ["claude", "yes"]
    );
    assert_eq!(
        rows[1].iter().map(|cell| text(cell)).collect::<Vec<_>>(),
        ["codex", "no"]
    );
}

#[test]
fn a_table_s_cells_keep_their_inline_style() {
    let parsed = blocks("| Name | Kind |\n|:-|:-:|\n| `ui-view` | **crate** |");
    let ProseBlock::Table {
        alignments, rows, ..
    } = &parsed[0]
    else {
        panic!("expected a table");
    };
    assert_eq!(alignments, &[TableAlign::Left, TableAlign::Center]);
    assert!(rows[0][0][0].code);
    assert!(rows[0][1][0].strong);
}

/// Without the delimiter row under it, a line of pipes is a paragraph:
/// GitHub's rule, where the clients' own parsers took any pipe lines as a
/// table.
#[test]
fn pipes_without_a_delimiter_row_are_a_paragraph() {
    assert_eq!(text(paragraph(&blocks("| one cell")[0])), "| one cell");
    assert_eq!(text(paragraph(&blocks("| a | b |")[0])), "| a | b |");
}

/// While a reply streams, its last line of pipes waits for the row under
/// it, so a table does not first draw as a paragraph of pipes.
#[test]
fn a_streaming_table_header_waits_for_its_delimiter_row() {
    let header = "Here:\n\n| a | b |";
    assert_eq!(
        Markdown::parse(header, true).blocks,
        vec![ProseBlock::Paragraph {
            text: vec![InlineRun::plain("Here:")]
        }]
    );
    // Finished, the line is what it is.
    assert_eq!(Markdown::parse(header, false).blocks.len(), 2);
    // Once the delimiter row is there, the table draws, and so do the
    // rows that arrive under it.
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    assert!(matches!(
        Markdown::parse(table, true).blocks[0],
        ProseBlock::Table { .. }
    ));
    let ProseBlock::Table { rows, .. } = &Markdown::parse(table, true).blocks[0] else {
        unreachable!();
    };
    assert_eq!(rows.len(), 1);
    // The source a client copies is the whole of it.
    assert_eq!(Markdown::parse(header, true).source, header);
}

#[test]
fn quotes_keep_their_lines_and_drop_their_marker() {
    let parsed = blocks("> first\n> second");
    let ProseBlock::Quote { blocks } = &parsed[0] else {
        panic!("expected a quote");
    };
    assert_eq!(text(paragraph(&blocks[0])), "first\nsecond");
}

#[test]
fn a_quote_holds_blocks() {
    let parsed = blocks("> ## Note\n>\n> - one\n> - two");
    let ProseBlock::Quote { blocks } = &parsed[0] else {
        panic!("expected a quote");
    };
    assert!(matches!(blocks[0], ProseBlock::Heading { level: 2, .. }));
    assert_eq!(item_texts(&blocks[1]), ["one", "two"]);
}

#[test]
fn lines_with_no_blank_line_between_them_are_one_paragraph() {
    let parsed = blocks("one line\nand its continuation\n\na second paragraph");
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        text(paragraph(&parsed[0])),
        "one line\nand its continuation"
    );
}

/// An agent's single newline is a line break, as Claude's and Codex's own
/// interfaces draw it. A blank line still parts two paragraphs.
#[test]
fn an_agent_s_single_line_breaks_stay_line_breaks() {
    let parsed = blocks("alpha\nbeta\ngamma");
    assert_eq!(parsed.len(), 1);
    assert_eq!(text(paragraph(&parsed[0])), "alpha\nbeta\ngamma");
    assert_eq!(blocks("alpha\n\nbeta").len(), 2);
    // A hard break is one too.
    assert_eq!(text(paragraph(&blocks("alpha  \nbeta")[0])), "alpha\nbeta");
}

#[test]
fn emphasis_code_spans_and_strikethrough_resolve_rather_than_showing_their_punctuation() {
    let parsed = blocks("run `cargo test` **now** and *soon* ~~not later~~");
    let runs = paragraph(&parsed[0]);
    assert_eq!(text(runs), "run cargo test now and soon not later");
    assert!(runs.iter().any(|run| run.code && run.text == "cargo test"));
    assert!(runs.iter().any(|run| run.strong && run.text == "now"));
    assert!(runs.iter().any(|run| run.emphasis && run.text == "soon"));
    assert!(
        runs.iter()
            .any(|run| run.strikethrough && run.text == "not later")
    );
}

#[test]
fn strong_emphasis_is_both() {
    let parsed = blocks("***both*** and __strong__ and _em_");
    let runs = paragraph(&parsed[0]);
    assert!(
        runs.iter()
            .any(|run| run.strong && run.emphasis && run.text == "both")
    );
    assert!(runs.iter().any(|run| run.strong && run.text == "strong"));
    assert!(runs.iter().any(|run| run.emphasis && run.text == "em"));
}

#[test]
fn snake_case_identifiers_keep_their_underscores() {
    let parsed = blocks("call create_issue and some_var_name");
    let runs = paragraph(&parsed[0]);
    assert_eq!(text(runs), "call create_issue and some_var_name");
    assert!(runs.iter().all(|run| !run.emphasis));
}

#[test]
fn a_link_keeps_its_text_and_its_destination() {
    let parsed = blocks("see [the spec](https://example.com)");
    assert_eq!(
        paragraph(&parsed[0]),
        [
            InlineRun::plain("see "),
            InlineRun {
                text: "the spec".into(),
                link: Some("https://example.com".into()),
                ..InlineRun::default()
            },
        ]
    );
}

/// Every link keeps its target, whatever its scheme; which a client opens
/// is the client's choice.
#[test]
fn every_link_keeps_its_target() {
    let source = "[page](https://example.com) [mail](mailto:ada@example.com) \
                  [sign-in](amux://callback?code=abc) <amux://callback?code=abc>";
    let parsed = blocks(source);
    let links: Vec<_> = paragraph(&parsed[0])
        .iter()
        .filter_map(|run| run.link.clone())
        .collect();
    assert_eq!(
        links,
        [
            "https://example.com",
            "mailto:ada@example.com",
            "amux://callback?code=abc",
            "amux://callback?code=abc",
        ]
    );
    assert!(text(paragraph(&parsed[0])).contains("sign-in"));
}

/// GitHub's reading: a bare web address is a link to itself, less the
/// sentence's punctuation after it.
#[test]
fn a_bare_web_address_is_a_link() {
    let parsed = blocks("See https://example.com/a. Then `http://not.a/link`.");
    let runs = paragraph(&parsed[0]);
    assert_eq!(
        runs[1],
        InlineRun {
            text: "https://example.com/a".into(),
            link: Some("https://example.com/a".into()),
            ..InlineRun::default()
        }
    );
    assert_eq!(runs.iter().filter(|run| run.link.is_some()).count(), 1);
}

#[test]
fn code_in_a_link_links() {
    let parsed = blocks("[`docs`](https://example.com)");
    let runs = paragraph(&parsed[0]);
    assert!(runs[0].code);
    assert_eq!(runs[0].link.as_deref(), Some("https://example.com"));
}

/// A stray asterisk must not blank the sentence around it.
#[test]
fn malformed_inline_markup_falls_back_to_the_literal_text() {
    assert_eq!(text(paragraph(&blocks("a * b")[0])), "a * b");
    assert_eq!(text(paragraph(&blocks("**unclosed")[0])), "**unclosed");
    assert_eq!(text(paragraph(&blocks("`unclosed")[0])), "`unclosed");
}

#[test]
fn a_horizontal_rule_is_a_rule_and_not_a_list() {
    assert_eq!(blocks("---"), vec![ProseBlock::Rule]);
    assert_eq!(blocks("***"), vec![ProseBlock::Rule]);
    assert_eq!(
        blocks("above\n\n---\n\nbelow")[1],
        ProseBlock::Rule,
        "a rule between paragraphs"
    );
}

/// CommonMark's setext heading: a line of `---` straight under a line of
/// text underlines it.
#[test]
fn a_line_underlined_is_a_heading() {
    assert_eq!(
        blocks("Title\n---"),
        vec![ProseBlock::Heading {
            level: 2,
            text: vec![InlineRun::plain("Title")],
        }]
    );
}

/// A plan streams in a few characters at a time, so its next heading is at
/// first only its marks: `##` with no title yet.
#[test]
fn a_plan_streaming_in_is_parsed_while_its_next_heading_is_half_written() {
    let parsed = blocks("## Summary\n\nCreate `README.md` with `UPDATED`.\n\n##");
    assert_eq!(
        parsed[0],
        ProseBlock::Heading {
            level: 2,
            text: vec![InlineRun::plain("Summary")],
        }
    );
    assert_eq!(
        text(paragraph(&parsed[1])),
        "Create README.md with UPDATED."
    );
    assert_eq!(
        parsed[2],
        ProseBlock::Heading {
            level: 2,
            text: vec![],
        }
    );
}

/// `#42` or `#!/bin/sh` at the start of a line is not a heading: a
/// heading's marks are followed by a space.
#[test]
fn a_hash_with_no_space_after_it_starts_a_paragraph_not_a_heading() {
    assert_eq!(text(paragraph(&blocks("#42 is fixed")[0])), "#42 is fixed");
    assert_eq!(text(paragraph(&blocks("#!/bin/sh")[0])), "#!/bin/sh");
}

/// Markup amux does not draw keeps its words.
#[test]
fn html_is_kept_as_its_words() {
    assert_eq!(
        text(paragraph(&blocks("a <kbd>Ctrl</kbd> b")[0])),
        "a <kbd>Ctrl</kbd> b"
    );
    assert_eq!(text(paragraph(&blocks("<details>")[0])), "<details>");
}

#[test]
fn an_image_is_its_words_linking_to_it() {
    let parsed = blocks("![the chart](https://example.com/chart.png)");
    assert_eq!(
        paragraph(&parsed[0]),
        [InlineRun {
            text: "the chart".into(),
            link: Some("https://example.com/chart.png".into()),
            ..InlineRun::default()
        }]
    );
}

#[test]
fn escapes_are_their_characters() {
    assert_eq!(
        text(paragraph(&blocks(r"\*not emphasis\* and 1\. this")[0])),
        "*not emphasis* and 1. this"
    );
}

#[test]
fn an_indented_block_is_code() {
    assert_eq!(
        blocks("para\n\n    indented\n    code"),
        vec![
            ProseBlock::Paragraph {
                text: vec![InlineRun::plain("para")]
            },
            ProseBlock::Code {
                language: String::new(),
                code: "indented\ncode".into(),
                closed: true,
            },
        ]
    );
}

#[test]
fn nothing_but_space_is_no_blocks() {
    assert!(blocks("").is_empty());
    assert!(blocks("  \n\n \t").is_empty());
}

#[test]
fn a_label_reads_its_inline_markdown_alone() {
    assert_eq!(
        inline_runs("Run `cargo test`"),
        [
            InlineRun::plain("Run "),
            InlineRun {
                text: "cargo test".into(),
                code: true,
                ..InlineRun::default()
            },
        ]
    );
    for label in [
        "1. Rust",
        "# 42",
        "- a dash",
        "> quoted",
        "| a | b |",
        "---",
        "12) twelve",
        "* star",
        "```",
    ] {
        assert_eq!(runs_text(&inline_runs(label)), label, "{label:?}");
    }
    assert_eq!(
        inline_runs("**`x`** here")[0],
        InlineRun {
            text: "x".into(),
            strong: true,
            code: true,
            ..InlineRun::default()
        }
    );
    assert!(inline_runs("").is_empty());
    assert_eq!(runs_text(&inline_runs("one\ntwo")), "one\ntwo");
}

#[test]
fn plain_text_reads_each_block_without_its_markup() {
    let parsed = Markdown::parse("## Title\n\n- **one**\n- two\n\n```\ncode\n```", false);
    assert_eq!(parsed.plain_text(), "Title\none\ntwo\ncode");
}

/// A parse is kept by its source, so a row built again for a frame finds
/// the blocks it had.
#[test]
fn a_parse_is_kept_by_its_source() {
    let source = "kept *once* and found again";
    let first = kept(source, false);
    assert!(Arc::ptr_eq(&first, &kept(source, false)));
    assert!(!Arc::ptr_eq(&first, &kept(source, true)));
}

#[test]
fn the_blocks_cross_as_json() {
    let parsed = Markdown::parse("# Hi\n\n- [x] `a`", false);
    let json = serde_json::to_string(&parsed).unwrap();
    assert_eq!(
        json,
        r##"{"source":"# Hi\n\n- [x] `a`","blocks":[{"Heading":{"level":1,"text":[{"text":"Hi","strong":false,"emphasis":false,"strikethrough":false,"code":false,"link":null}]}},{"List":{"ordered":false,"start":1,"depth":0,"items":[{"task":true,"blocks":[{"Paragraph":{"text":[{"text":"a","strong":false,"emphasis":false,"strikethrough":false,"code":true,"link":null}]}}]}]}}]}"##
    );
    assert_eq!(serde_json::from_str::<Markdown>(&json).unwrap(), parsed);
}
