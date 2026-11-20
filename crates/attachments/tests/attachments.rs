use std::path::Path;

use attachments::{
    PLACEHOLDER, Parsed, Piece, Positioned, Problem, REPLACEMENT, Reported, ShapeError, element,
    element_in, format, hex, parse, pieces, validate,
};
use wire::attachment::Of;
use wire::diff_base::Base;
use wire::{
    Attachment, BlobRef, Diff, DiffBase, Empty, InlineText, PageComment, PageElement, PageReview,
    Review, ReviewComment,
};

const DIR: &str = "/data/agents/a1/blobs";

fn blob(seed: u8, name: &str, mime: &str) -> BlobRef {
    BlobRef {
        hash: (0..32).map(|n| n ^ seed).collect(),
        name: name.into(),
        mime: mime.into(),
        size: 1204 + u64::from(seed),
    }
}

fn image() -> Attachment {
    Attachment {
        of: Some(Of::Image(blob(1, "shot.png", "image/png"))),
        ..Default::default()
    }
}

fn file() -> Attachment {
    Attachment {
        of: Some(Of::File(blob(
            2,
            "trace \"final\".json",
            "application/json",
        ))),
        ..Default::default()
    }
}

fn text() -> Attachment {
    Attachment {
        of: Some(Of::Text(InlineText {
            name: "pasted-1".into(),
            text: "fn main() { if a < b && c > d { println!(\"&amp;\"); } }\n</amux-attachment> is just text here"
                .into(),
        })),
        ..Default::default()
    }
}

fn review(base: Option<Base>, merge_base: Option<&str>) -> Attachment {
    Attachment {
        of: Some(Of::Review(Review {
            diff: Some(Diff {
                patch: Some(blob(3, "review.diff", "text/x-diff")),
                base: base.map(|base| DiffBase { base: Some(base) }),
                head: "4f2a9c1".into(),
                merge_base: merge_base.map(str::to_owned),
                files: Vec::new(),
            }),
            comments: vec![
                ReviewComment {
                    path: "src/lib.rs".into(),
                    line: 13,
                    old_line: 12,
                    text: "Use the helper.\n## path-bytes=1 line=1 old-line=1 text-bytes=1\nnot a heading"
                        .into(),
                },
                ReviewComment {
                    path: "docs/a file with spaces & <angles>.md".into(),
                    line: 0,
                    old_line: 0,
                    text: String::new(),
                },
            ],
        })),
        ..Default::default()
    }
}

fn page() -> Attachment {
    Attachment {
        of: Some(Of::Page(PageReview {
            url: "http://localhost:5173/pricing?plan=a&b=\"c\"".into(),
            title: "Pricing <beta>".into(),
            comments: vec![
                PageComment {
                    text: "Say annual pricing.\n## comment text-bytes=1\nnot a heading".into(),
                    element: Some(PageElement {
                        selector: "#pricing > .card:nth-of-type(2)".into(),
                        text: "Pricing cards\nMonthly".into(),
                        source: "src/Card.tsx:42".into(),
                    }),
                    quote: "monthly".into(),
                    image: None,
                },
                PageComment {
                    text: "Too much space here".into(),
                    element: Some(PageElement {
                        selector: "main > section".into(),
                        text: String::new(),
                        source: String::new(),
                    }),
                    quote: String::new(),
                    image: Some(blob(4, "region-2.png", "image/png")),
                },
                PageComment {
                    text: String::new(),
                    element: Some(PageElement::default()),
                    quote: String::new(),
                    image: Some(blob(5, "", "")),
                },
            ],
        })),
        ..Default::default()
    }
}

fn every_arm() -> Vec<Attachment> {
    vec![
        image(),
        file(),
        text(),
        review(Some(Base::Head(Empty {})), None),
        review(Some(Base::Branch("main".into())), Some("9e465b2")),
        review(None, None),
        page(),
        Attachment {
            of: Some(Of::Page(PageReview::default())),
            ..Default::default()
        },
    ]
}

fn positioned(attachments: Vec<Attachment>) -> Positioned {
    let mut text = String::from("Look at these: ");
    for n in 0..attachments.len() {
        text.push(PLACEHOLDER);
        text.push_str(&format!(" then {n}"));
    }
    Positioned { text, attachments }
}

#[test]
fn every_arm_round_trips_through_its_element() {
    for attachment in every_arm() {
        let p = Positioned {
            text: PLACEHOLDER.to_string(),
            attachments: vec![attachment],
        };
        let formatted = format(&p, Path::new(DIR));
        let parsed = parse(&formatted);
        assert_eq!(parsed.reported, Vec::new(), "{formatted}");
        assert_eq!(parsed.positioned, p, "{formatted}");
    }
}

#[test]
fn positions_round_trip_with_prose_between_elements() {
    let p = positioned(every_arm());
    validate(&p).unwrap();
    let formatted = format(&p, Path::new(DIR));
    assert!(!formatted.contains(PLACEHOLDER));
    assert_eq!(
        parse(&formatted),
        Parsed {
            positioned: p,
            reported: Vec::new()
        }
    );
}

#[test]
fn the_canonical_elements() {
    assert_eq!(
        element(&image(), Some(Path::new("/b/x"))),
        format!(
            "<amux-attachment kind=\"image\" hash=\"sha256:{}\" name=\"shot.png\" mime=\"image/png\" size=\"1205\" path=\"/b/x\"/>",
            hex(&blob(1, "", "").hash)
        )
    );
    assert_eq!(
        element(
            &Attachment {
                of: Some(Of::Text(InlineText {
                    name: "n".into(),
                    text: "a < b".into()
                })),
                ..Default::default()
            },
            None
        ),
        "<amux-attachment kind=\"text\" name=\"n\">a < b</amux-attachment>"
    );
}

/// A body escapes only what the parser would read as markup: code reads as
/// written, and an element's tags or a character reference inside it stay
/// text.
#[test]
fn a_body_escapes_only_what_reads_as_markup() {
    let body = |text: &str| {
        let attachment = Attachment {
            of: Some(Of::Text(InlineText {
                name: "n<&>".into(),
                text: text.into(),
            })),
            ..Default::default()
        };
        let element = element(&attachment, None);
        assert_eq!(parse(&element).positioned.attachments, vec![attachment]);
        element
            .strip_prefix("<amux-attachment kind=\"text\" name=\"n&lt;&amp;&gt;\">")
            .and_then(|rest| rest.strip_suffix("</amux-attachment>"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        body("if a < b && c > d { Vec::<u8>::new() } & done;"),
        "if a < b && c > d { Vec::<u8>::new() } & done;"
    );
    assert_eq!(
        body("<amux-attachment kind=\"x\"/> and </amux-attachment>"),
        "&lt;amux-attachment kind=\"x\"/> and &lt;/amux-attachment>"
    );
    assert_eq!(
        body("&amp; &lt; &#10; &nbsp;"),
        "&amp;amp; &amp;lt; &amp;#10; &amp;nbsp;"
    );
    // An element written by an earlier build, every character escaped,
    // reads the same.
    assert_eq!(
        parse("<amux-attachment kind=\"text\" name=\"n\">a &lt; b &amp;&amp; c &gt; d</amux-attachment>")
            .positioned
            .attachments,
        vec![Attachment {
            of: Some(Of::Text(InlineText {
                name: "n".into(),
                text: "a < b && c > d".into(),
            })),
            ..Default::default()
        }]
    );
}

#[test]
fn pieces_locate_each_blob_in_the_agent_directory() {
    let p = positioned(vec![image(), text()]);
    let pieces = pieces(&p, Path::new(DIR));
    let located = pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Element { path, .. } => Some(path.clone()),
            Piece::Text(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        located,
        vec![Some(Path::new(DIR).join(hex(&blob(1, "", "").hash))), None]
    );
    assert!(matches!(pieces[0], Piece::Text("Look at these: ")));
}

/// An arm a later build added decodes here as no arm: it would be dropped
/// from the prompt without a word. The placeholder count is no part of it.
#[test]
fn an_attachment_with_no_arm_is_one_this_build_cannot_read() {
    assert!(attachments::all_readable(&[]));
    assert!(attachments::all_readable(&every_arm()));
    let mut held = every_arm();
    held.push(wire::Attachment {
        of: None,
        ..Default::default()
    });
    assert!(!attachments::all_readable(&held));
}

#[test]
fn validate_counts_placeholders_against_attachments() {
    assert_eq!(validate(&positioned(every_arm())), Ok(()));
    assert_eq!(validate(&Positioned::default()), Ok(()));
    let mut extra = positioned(vec![image()]);
    extra.text.push(PLACEHOLDER);
    assert_eq!(
        validate(&extra),
        Err(ShapeError::CountMismatch {
            placeholders: 2,
            attachments: 1
        })
    );
    let missing = Positioned {
        text: "no placeholder".into(),
        attachments: vec![image()],
    };
    assert_eq!(
        validate(&missing),
        Err(ShapeError::CountMismatch {
            placeholders: 0,
            attachments: 1
        })
    );
}

#[test]
fn validate_rejects_empty_attachments_and_bad_hashes() {
    let empty = Positioned {
        text: PLACEHOLDER.to_string(),
        attachments: vec![Attachment {
            of: None,
            ..Default::default()
        }],
    };
    assert_eq!(validate(&empty), Err(ShapeError::Empty { index: 0 }));

    let mut short = blob(1, "x", "image/png");
    short.hash.truncate(31);
    let bad = Positioned {
        text: format!("{PLACEHOLDER}{PLACEHOLDER}"),
        attachments: vec![
            text(),
            Attachment {
                of: Some(Of::File(short)),
                ..Default::default()
            },
        ],
    };
    assert_eq!(validate(&bad), Err(ShapeError::BadHash { index: 1 }));

    let no_patch = Positioned {
        text: PLACEHOLDER.to_string(),
        attachments: vec![Attachment {
            of: Some(Of::Review(Review::default())),
            ..Default::default()
        }],
    };
    assert_eq!(validate(&no_patch), Err(ShapeError::BadHash { index: 0 }));
}

fn problems(text: &str) -> Vec<Problem> {
    parse(text)
        .reported
        .into_iter()
        .map(|reported| reported.problem)
        .collect()
}

#[test]
fn malformed_candidates_stay_text_and_are_reported() {
    let hash = format!("sha256:{}", hex(&blob(1, "", "").hash));
    let cases: Vec<(String, Problem)> = vec![
        (
            "unterminated <amux-attachment kind=\"image\"".into(),
            Problem::Unterminated,
        ),
        (
            "open body <amux-attachment kind=\"text\" name=\"x\">never closed".into(),
            Problem::Unterminated,
        ),
        (
            "<amux-attachment kind=\"image\" name=\"x.png\"/>".into(),
            Problem::MissingHash,
        ),
        (
            "<amux-attachment kind=\"file\" hash=\"sha256:1234\"/>".into(),
            Problem::BadHash,
        ),
        (
            format!(
                "<amux-attachment kind=\"file\" hash=\"{}\"/>",
                hash.to_uppercase()
            ),
            Problem::BadHash,
        ),
        (
            format!("<amux-attachment kind=\"video\" hash=\"{hash}\"/>"),
            Problem::UnknownKind,
        ),
        (
            format!("<amux-attachment hash=\"{hash}\"/>"),
            Problem::UnknownKind,
        ),
        (
            format!("<amux-attachment kind=\"image\" hash=\"{hash}\" id=\"old\"/>"),
            Problem::UnknownAttribute("id".into()),
        ),
        (
            format!("<amux-attachment kind=\"image\" hash=\"{hash}\" size=\"big\"/>"),
            Problem::BadAttribute("size".into()),
        ),
        (
            format!("<amux-attachment kind=\"image\" hash=\"{hash}\">body</amux-attachment>"),
            Problem::BadForm,
        ),
        (
            "<amux-attachment kind=\"text\" name=\"x\"/>".into(),
            Problem::BadForm,
        ),
        (
            "<amux-attachment kind=\"text\" name=\"x\">&bogus;</amux-attachment>".into(),
            Problem::BadBody,
        ),
        (
            format!(
                "<amux-attachment kind=\"review\" hash=\"{hash}\" comments=\"2\">## path-bytes=1 line=1 old-line=0 text-bytes=0\na\n</amux-attachment>"
            ),
            Problem::BadBody,
        ),
        (
            format!(
                "<amux-attachment kind=\"review\" hash=\"{hash}\" comments=\"0\" base=\"trunk\"></amux-attachment>"
            ),
            Problem::BadAttribute("base".into()),
        ),
        (
            "<amux-attachment kind=\"page\" url=\"u\" comments=\"1\"></amux-attachment>".into(),
            Problem::BadBody,
        ),
        (
            "<amux-attachment kind=\"page\" url=\"u\" comments=\"1\">## comment text-bytes=9\ntext: short\n</amux-attachment>".into(),
            Problem::BadBody,
        ),
        (
            "<amux-attachment kind=\"page\" url=\"u\" comments=\"1\">## comment colour-bytes=1\ncolour: r\n</amux-attachment>".into(),
            Problem::BadBody,
        ),
        (
            "<amux-attachment kind=\"page\" url=\"u\"/>".into(),
            Problem::BadForm,
        ),
    ];
    for (text, problem) in cases {
        let parsed = parse(&text);
        assert_eq!(parsed.positioned.text, text, "stays text: {text}");
        assert!(parsed.positioned.attachments.is_empty(), "{text}");
        assert_eq!(
            parsed.reported,
            vec![Reported {
                at: text.find("<amux").unwrap(),
                problem
            }],
            "{text}"
        );
    }
}

#[test]
fn a_nested_element_leaves_the_outer_as_text_and_parses_the_inner() {
    let inner = element(&image(), None);
    let text =
        format!("<amux-attachment kind=\"text\" name=\"x\">before {inner} after</amux-attachment>");
    let parsed = parse(&text);
    assert_eq!(parsed.positioned.attachments, vec![image()]);
    assert_eq!(
        parsed.positioned.text,
        format!(
            "<amux-attachment kind=\"text\" name=\"x\">before {PLACEHOLDER} after</amux-attachment>"
        )
    );
    assert_eq!(problems(&text), vec![Problem::Nested]);
}

#[test]
fn a_malformed_candidate_does_not_hide_a_later_valid_element() {
    let valid = element(&file(), None);
    let text = format!("<amux-attachment kind=\"image\"/> and {valid}");
    let parsed = parse(&text);
    assert_eq!(parsed.positioned.attachments, vec![file()]);
    assert_eq!(
        parsed.positioned.text,
        format!("<amux-attachment kind=\"image\"/> and {PLACEHOLDER}")
    );
    assert_eq!(problems(&text), vec![Problem::MissingHash]);
    validate(&parsed.positioned).unwrap();
}

#[test]
fn stray_placeholders_in_text_are_replaced_and_reported() {
    let valid = element(&image(), None);
    let text = format!("a{PLACEHOLDER}b {valid} c{PLACEHOLDER}");
    let parsed = parse(&text);
    assert_eq!(
        parsed.positioned.text,
        format!("a{REPLACEMENT}b {PLACEHOLDER} c{REPLACEMENT}")
    );
    assert_eq!(parsed.positioned.attachments, vec![image()]);
    assert_eq!(
        problems(&text),
        vec![Problem::StrayPlaceholder, Problem::StrayPlaceholder]
    );
    validate(&parsed.positioned).unwrap();
}

#[test]
fn similar_tags_and_plain_text_are_not_candidates() {
    for text in [
        "plain text",
        "<amux-attachments kind=\"image\"/>",
        "a <b>tag</b> and </amux-attachment> alone",
    ] {
        let parsed = parse(text);
        assert_eq!(parsed.positioned.text, text);
        assert!(parsed.positioned.attachments.is_empty());
    }
    assert_eq!(
        problems("<amux-attachmentx kind=\"image\"/>"),
        vec![Problem::UnknownKind]
    );
}

#[test]
fn the_path_attribute_is_accepted_and_ignored() {
    let with_path = element(&image(), Some(Path::new("/elsewhere/on/another/host")));
    let parsed = parse(&with_path);
    assert_eq!(parsed.positioned.attachments, vec![image()]);
    assert!(parsed.reported.is_empty());
    let page_with_paths = element_in(&page(), Path::new("/elsewhere"));
    assert!(page_with_paths.contains(&format!(
        "image-path: /elsewhere{}",
        std::path::MAIN_SEPARATOR
    )));
    assert_eq!(parse(&page_with_paths).positioned.attachments, vec![page()]);
}

/// The model reads a page review as labelled lines under a heading that
/// counts their bytes.
#[test]
fn the_page_element() {
    let review = Attachment {
        of: Some(Of::Page(PageReview {
            url: "http://localhost:5173/".into(),
            title: "Pricing".into(),
            comments: vec![
                PageComment {
                    text: "Say annual pricing.".into(),
                    element: Some(PageElement {
                        selector: "#pricing".into(),
                        text: "Pricing cards".into(),
                        source: String::new(),
                    }),
                    quote: "monthly".into(),
                    image: None,
                },
                PageComment {
                    text: "Tighter".into(),
                    element: Some(PageElement {
                        selector: "main".into(),
                        ..Default::default()
                    }),
                    quote: String::new(),
                    image: Some(blob(4, "region-2.png", "image/png")),
                },
            ],
        })),
        ..Default::default()
    };
    let hash = hex(&blob(4, "", "").hash);
    let path = Path::new("/b").join(&hash);
    let path = path.display();
    assert_eq!(
        element_in(&review, Path::new("/b")),
        format!(
            "<amux-attachment kind=\"page\" url=\"http://localhost:5173/\" title=\"Pricing\" comments=\"2\">\
             ## comment selector-bytes=8 element-bytes=13 quote-bytes=7 text-bytes=19\n\
             selector: #pricing\n\
             element: Pricing cards\n\
             quote: monthly\n\
             text: Say annual pricing.\n\
             ## comment selector-bytes=4 image-hash=sha256:{hash} image-size=1208 image-bytes=12 image-mime-bytes=9 image-path-bytes=67 text-bytes=7\n\
             selector: main\n\
             image: region-2.png\n\
             image-mime: image/png\n\
             image-path: {path}\n\
             text: Tighter\n\
             </amux-attachment>"
        )
    );
}

#[test]
fn a_page_review_names_every_screenshot_as_a_blob() {
    let names = attachments::blobs(&page())
        .into_iter()
        .map(|blob| blob.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, ["region-2.png", ""]);
    let mut short = page();
    if let Some(Of::Page(review)) = &mut short.of {
        review.comments[1].image.as_mut().unwrap().hash.truncate(31);
    }
    let bad = Positioned {
        text: PLACEHOLDER.to_string(),
        attachments: vec![short],
    };
    assert_eq!(validate(&bad), Err(ShapeError::BadHash { index: 0 }));
}

/// An image is native by its bytes alone, whatever its name or label.
#[test]
fn an_image_is_native_by_its_bytes() {
    use attachments::{native_image, native_image_at};
    assert_eq!(native_image(b"\x89PNG\r\n\x1a\n..."), Some("image/png"));
    assert_eq!(native_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
    assert_eq!(native_image(b"GIF87a."), Some("image/gif"));
    assert_eq!(native_image(b"GIF89a."), Some("image/gif"));
    assert_eq!(native_image(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(native_image(b"RIFF\x10\0\0\0WAVEfmt "), None);
    assert_eq!(
        native_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
        None
    );
    assert_eq!(native_image(b"\0\0\0\x18ftypheic"), None);
    assert_eq!(native_image(b"II*\0"), None);
    assert_eq!(native_image(b""), None);
    let path = std::env::temp_dir().join(format!("amux-native-{}", std::process::id()));
    std::fs::write(&path, b"GIF89a and the rest of a long file").unwrap();
    assert_eq!(native_image_at(&path), Some("image/gif"));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(native_image_at(&path), None);
}

mod sent_review {
    use std::path::Path;

    use attachments::sent_review::{NAME, attachment, entry, text};
    use attachments::{PLACEHOLDER, Positioned, format, parse};
    use wire::attachment::Of;
    use wire::experimental::review_placement::At;
    use wire::experimental::{
        ReviewAnchor, ReviewDefinition, ReviewDefinitionKind, ReviewLine, ReviewLineKind,
        ReviewLineRange, ReviewPlacement, ReviewRound, ReviewSummary, ReviewerComment, SentReview,
    };
    use wire::{Empty, InlineText};

    const SNAPSHOT: &str = "4f2a9c1e0b7d3a5f6e8c9b0a1d2e3f4a5b6c7d8e";
    const BASE: &str = "9e465b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a";
    const ROOT: &str = "/work/shop";
    const OBJECTS: &str = "/data/profiles/p/agents/a1/review/objects";

    fn line(kind: ReviewLineKind, old_line: u32, new_line: u32, text: &str) -> ReviewLine {
        ReviewLine {
            kind: kind as i32,
            old_line,
            new_line,
            text: text.into(),
            runs: Vec::new(),
        }
    }

    fn definition(kind: ReviewDefinitionKind, name: &str, label: &str) -> ReviewDefinition {
        ReviewDefinition {
            kind: kind as i32,
            name: name.into(),
            label: label.into(),
            start_line: 1,
            end_line: 40,
            parent: None,
        }
    }

    fn lines(start: u32, end: u32) -> ReviewLineRange {
        ReviewLineRange { start, end }
    }

    fn placed(at: At) -> Option<ReviewPlacement> {
        Some(ReviewPlacement {
            at: Some(at),
            changed_near: false,
        })
    }

    /// A comment on two lines of code holding `<`, `>` and `&`, one on a
    /// removed line, and one on a whole file.
    fn comments() -> Vec<ReviewerComment> {
        use ReviewLineKind::{Added, Context, Removed};
        vec![
            ReviewerComment {
                id: b"c1".to_vec(),
                anchor: Some(ReviewAnchor {
                    commit: SNAPSHOT.into(),
                    path: "src/store.rs".into(),
                    lines: Some(lines(13, 14)),
                }),
                text: "Keep the strict comparison: a store at its limit is full.".into(),
                snippet: vec![
                    line(Context, 10, 11, "    let path = dir.join(\"store\");"),
                    line(Removed, 11, 0, "    if size < limit && open {"),
                    line(Added, 0, 12, "    if size <= limit && open {"),
                    line(
                        Added,
                        0,
                        13,
                        "        return Ok(Store::<File> { path, size });",
                    ),
                    line(Context, 12, 14, "    }"),
                ],
                scope: vec![
                    definition(ReviewDefinitionKind::Impl, "Store", "impl Store"),
                    definition(ReviewDefinitionKind::Method, "open", "fn open"),
                ],
                round: None,
                placement: placed(At::Lines(lines(13, 14))),
            },
            ReviewerComment {
                id: b"c2".to_vec(),
                anchor: Some(ReviewAnchor {
                    commit: BASE.into(),
                    path: "src/limits.rs".into(),
                    lines: Some(lines(7, 7)),
                }),
                text: "Why did this go?".into(),
                snippet: vec![
                    line(Context, 6, 6, "pub const LIMIT: u64 = 1 << 20;"),
                    line(Removed, 7, 0, "pub const SOFT: u64 = LIMIT >> 1;"),
                ],
                scope: Vec::new(),
                round: None,
                placement: None,
            },
            ReviewerComment {
                id: b"c3".to_vec(),
                anchor: Some(ReviewAnchor {
                    commit: SNAPSHOT.into(),
                    path: "docs/notes & limits.md".into(),
                    lines: Some(lines(0, 0)),
                }),
                text: "Say what the limit is.\n".into(),
                snippet: Vec::new(),
                scope: Vec::new(),
                round: None,
                placement: placed(At::File(Empty {})),
            },
        ]
    }

    fn summary() -> ReviewSummary {
        ReviewSummary {
            id: b"review-1".to_vec(),
            base_branch: "main".into(),
            base_commit: BASE.into(),
            snapshot: SNAPSHOT.into(),
            rounds: vec![ReviewRound {
                snapshot: BASE.into(),
                base_commit: BASE.into(),
                sent_at_ms: 1,
            }],
            objects: OBJECTS.into(),
            root: ROOT.into(),
            ..Default::default()
        }
    }

    /// Per comment: where it is, the definition it is in, the code in diff
    /// form and then the words; last, what the numbers refer to and the
    /// command that diffs against the snapshot.
    #[test]
    fn the_text_the_agent_reads() {
        assert_eq!(
            text(ROOT, OBJECTS, SNAPSHOT, &comments()),
            format!(
                "3 comments from a review of your changes.\n\
                 \n\
                 ## src/store.rs, lines 13–14, in impl Store › fn open\n\
                 \n\
                 ```diff\n\
                 \x20    let path = dir.join(\"store\");\n\
                 -    if size < limit && open {{\n\
                 +    if size <= limit && open {{\n\
                 +        return Ok(Store::<File> {{ path, size }});\n\
                 \x20    }}\n\
                 ```\n\
                 \n\
                 Keep the strict comparison: a store at its limit is full.\n\
                 \n\
                 ## src/limits.rs, line 7 of commit {BASE}\n\
                 \n\
                 ```diff\n\
                 \x20pub const LIMIT: u64 = 1 << 20;\n\
                 -pub const SOFT: u64 = LIMIT >> 1;\n\
                 ```\n\
                 \n\
                 Why did this go?\n\
                 \n\
                 ## docs/notes & limits.md, the whole file\n\
                 \n\
                 Say what the limit is.\n\
                 \n\
                 ---\n\
                 Line numbers are those of snapshot {SNAPSHOT} unless a comment names another \
                 commit, a commit of the working tree taken for this review and kept outside the \
                 repository, so the files may have changed since. Paths are relative to the \
                 repository's top, {ROOT}. To see what has changed since the snapshot, run:\n\
                 \n\
                 ```sh\n\
                 GIT_ALTERNATE_OBJECT_DIRECTORIES={OBJECTS} git -C {ROOT} diff {SNAPSHOT}\n\
                 ```\n"
            )
        );
    }

    #[test]
    fn a_path_the_shell_would_split_is_quoted_in_the_command() {
        assert_eq!(
            attachments::sent_review::command("/Users/me/my shop", "/d/it's/objects", SNAPSHOT),
            format!(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES='/d/it'\\''s/objects' git -C '/Users/me/my shop' diff {SNAPSHOT}"
            )
        );
    }

    #[test]
    fn a_snippet_holding_backticks_gets_a_longer_fence() {
        let mut comment = comments().remove(0);
        comment.snippet = vec![line(ReviewLineKind::Added, 0, 3, "let s = \"```\";")];
        let read = text(ROOT, OBJECTS, SNAPSHOT, &[comment]);
        assert!(
            read.contains("````diff\n+let s = \"```\";\n````\n"),
            "{read}"
        );
    }

    /// The attachment is text named `review`, with the entry naming exactly
    /// the comments sent, the round its sender expects and the snapshot.
    #[test]
    fn the_attachment_carries_its_text_and_its_entry() {
        let sent = attachment(&summary(), &comments()[..2]);
        assert_eq!(
            sent.of,
            Some(Of::Text(InlineText {
                name: NAME.into(),
                text: text(ROOT, OBJECTS, SNAPSHOT, &comments()[..2]),
            }))
        );
        assert_eq!(
            entry(&sent),
            Some(SentReview {
                review_id: b"review-1".to_vec(),
                round: 2,
                snapshot: SNAPSHOT.into(),
                comment_ids: vec![b"c1".to_vec(), b"c2".to_vec()],
            })
        );
        let plain = wire::Attachment {
            of: sent.of.clone(),
            ..Default::default()
        };
        assert_eq!(entry(&plain), None);
    }

    /// The element the model reads holds the code as written, and parses
    /// back to the same text.
    #[test]
    fn code_in_a_snippet_survives_the_element() {
        let sent = attachment(&summary(), &comments());
        let p = Positioned {
            text: format!("Please look at these.\n{PLACEHOLDER}"),
            attachments: vec![sent.clone()],
        };
        let formatted = format(&p, Path::new("/blobs"));
        assert!(
            formatted.contains("-    if size < limit && open {\n"),
            "{formatted}"
        );
        assert!(
            formatted.contains("+        return Ok(Store::<File> { path, size });\n"),
            "{formatted}"
        );
        let parsed = parse(&formatted);
        assert_eq!(parsed.reported, Vec::new());
        // The element carries the text; the entry travels on the attachment.
        let mut expected = p;
        expected.attachments[0].experimental.clear();
        assert_eq!(parsed.positioned, expected);
    }
}
