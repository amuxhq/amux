use std::path::Path;

use attachments::{
    PLACEHOLDER, Parsed, Piece, Positioned, Problem, REPLACEMENT, Reported, ShapeError, element,
    format, hex, parse, pieces, validate,
};
use wire::attachment::Of;
use wire::diff_base::Base;
use wire::{Attachment, BlobRef, Diff, DiffBase, Empty, InlineText, Review, ReviewComment};

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
    }
}

fn file() -> Attachment {
    Attachment {
        of: Some(Of::File(blob(
            2,
            "trace \"final\".json",
            "application/json",
        ))),
    }
}

fn text() -> Attachment {
    Attachment {
        of: Some(Of::Text(InlineText {
            name: "pasted-1".into(),
            text: "fn main() { if a < b && c > d { println!(\"&amp;\"); } }\n</amux-attachment> is just text here"
                .into(),
        })),
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
                }))
            },
            None
        ),
        "<amux-attachment kind=\"text\" name=\"n\">a &lt; b</amux-attachment>"
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
        attachments: vec![Attachment { of: None }],
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
            },
        ],
    };
    assert_eq!(validate(&bad), Err(ShapeError::BadHash { index: 1 }));

    let no_patch = Positioned {
        text: PLACEHOLDER.to_string(),
        attachments: vec![Attachment {
            of: Some(Of::Review(Review::default())),
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
