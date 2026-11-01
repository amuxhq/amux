//! Every row of the chat vocabulary's catalogue that a kind can show has a
//! place in that kind's bodies and a golden that shows it. The catalogue is
//! encoded in vocabulary.toml beside this file.

use std::collections::BTreeMap;
use std::path::Path;

use prost::Message as _;
use prost_types::{DescriptorProto, FileDescriptorSet};
use serde::Deserialize;

const KINDS: [&str; 3] = ["claude_pty", "claude_sdk", "codex"];
const ROWS: usize = 31;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vocabulary {
    row: Vec<Row>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    item: String,
    claude_pty: Availability,
    claude_sdk: Availability,
    codex: Availability,
}

impl Row {
    fn kind(&self, kind: &str) -> &Availability {
        match kind {
            "claude_pty" => &self.claude_pty,
            "claude_sdk" => &self.claude_sdk,
            _ => &self.codex,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Availability {
    mark: Mark,
    #[serde(default)]
    carriers: Vec<String>,
    #[serde(default)]
    golden: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Mark {
    Full,
    Partial,
    None,
    Na,
}

/// The wire schema's messages by full name.
fn messages() -> BTreeMap<String, DescriptorProto> {
    let set = FileDescriptorSet::decode(wire::DESCRIPTOR_SET).expect("descriptor set decodes");
    set.file
        .into_iter()
        .filter(|file| file.package() == "amux.v1")
        .flat_map(|file| file.message_type)
        .map(|message| (format!(".amux.v1.{}", message.name()), message))
        .collect()
}

/// The message type of `field` in `message`, if the field exists; the
/// empty string for a field that is not a message.
fn field_type<'a>(
    messages: &'a BTreeMap<String, DescriptorProto>,
    message: &str,
    field: &str,
) -> Option<&'a str> {
    messages
        .get(message)?
        .field
        .iter()
        .find(|candidate| candidate.name() == field)
        .map(|found| found.type_name())
}

/// Whether a carrier names a real place in the kind's bodies.
fn carrier_exists(
    messages: &BTreeMap<String, DescriptorProto>,
    kind: &str,
    carrier: &str,
) -> Result<(), String> {
    // Both Claude kinds carry the same item and snapshot messages.
    let camel = if kind == "codex" { "Codex" } else { "Claude" };
    let ask = if kind == "codex" {
        ".amux.v1.CodexAsk"
    } else {
        ".amux.v1.Ask"
    };
    let (place, path) = carrier
        .split_once(':')
        .ok_or_else(|| format!("{carrier:?} is not place:field"))?;
    let root = match place {
        "item" => format!(".amux.v1.{camel}Item"),
        "snapshot" => format!(".amux.v1.{camel}Snapshot"),
        "ask" => ask.to_owned(),
        other => return Err(format!("{carrier:?}: unknown place {other:?}")),
    };
    let mut message = root.clone();
    for field in path.split('.') {
        message = field_type(messages, &message, field)
            .ok_or_else(|| format!("{carrier:?}: {message} has no field {field:?}"))?
            .to_owned();
    }
    Ok(())
}

/// Every line of every golden a kind has.
fn golden_lines(kind: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(kind);
    let mut lines = Vec::new();
    let mut entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "golden"))
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        lines.extend(text.lines().map(|line| (name.clone(), line.to_owned())));
    }
    lines
}

fn shows(line: &str, golden: &[String]) -> bool {
    golden
        .iter()
        .all(|pattern| match pattern.strip_prefix('!') {
            Some(absent) => !line.contains(absent),
            None => line.contains(pattern.as_str()),
        })
}

#[test]
fn vocabulary_coverage() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vocabulary.toml");
    let vocabulary: Vocabulary = toml::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let ids = vocabulary
        .row
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let expected = (1..=ROWS).map(|n| format!("C{n}")).collect::<Vec<_>>();
    assert_eq!(
        ids, expected,
        "vocabulary.toml must hold every catalogue row, in order"
    );

    let messages = messages();
    let goldens = KINDS
        .iter()
        .map(|kind| (*kind, golden_lines(kind)))
        .collect::<BTreeMap<_, _>>();
    let mut problems = Vec::new();
    for row in &vocabulary.row {
        for kind in KINDS {
            let availability = row.kind(kind);
            let what = format!("{} {} ({kind}, {:?})", row.id, row.item, availability.mark);
            if matches!(availability.mark, Mark::None | Mark::Na) {
                if !availability.carriers.is_empty() || !availability.golden.is_empty() {
                    problems.push(format!(
                        "{what}: carriers or goldens on a row the kind cannot show"
                    ));
                }
                continue;
            }
            if availability.carriers.is_empty() {
                problems.push(format!("{what}: no item kind or snapshot field carries it"));
            }
            for carrier in &availability.carriers {
                if let Err(problem) = carrier_exists(&messages, kind, carrier) {
                    problems.push(format!("{what}: {problem}"));
                }
            }
            if availability
                .golden
                .iter()
                .all(|pattern| pattern.starts_with('!'))
            {
                problems.push(format!("{what}: no golden pattern"));
                continue;
            }
            match goldens[kind]
                .iter()
                .find(|(_, line)| shows(line, &availability.golden))
            {
                Some((golden, _)) => println!("{what}: {golden}.golden"),
                None => problems.push(format!(
                    "{what}: no golden line shows {:?}",
                    availability.golden
                )),
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} catalogue rows are not covered:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
