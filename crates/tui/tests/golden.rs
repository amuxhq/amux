//! Component goldens: each piece of the chat vocabulary drawn from authored
//! view values into the test backend, as text and as a map of semantic
//! style classes (see `Theme::classify`), in the light and dark themes.
//!
//! Rewrite with `UPDATE_GOLDENS=1 just test-tui` and review the diff like
//! code; CI refuses to rewrite.

#![cfg(feature = "fixtures")]

use std::collections::BTreeSet;

use common::{assert_golden, capture, golden_dir, themes};
use tui::Theme;
use tui::vocabulary::components;

use crate::common;

#[test]
fn every_vocabulary_component_matches_its_golden_in_both_themes() {
    for (variant, theme) in themes() {
        for component in components(theme) {
            assert_golden(
                &format!("{}.{variant}", component.name),
                &capture(&component.buffer, theme),
            );
        }
    }
}

#[test]
fn every_golden_file_belongs_to_a_component() {
    let mut expected = BTreeSet::new();
    for (variant, theme) in themes() {
        for component in components(theme) {
            expected.insert(format!("{}.{variant}.txt", component.name));
        }
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(golden_dir())
        .expect("golden directory")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".txt") && !name.starts_with("frame_"))
        .collect();
    let stale: Vec<_> = on_disk.difference(&expected).collect();
    assert!(stale.is_empty(), "goldens no component draws: {stale:?}");
}

#[test]
fn a_component_name_is_used_once() {
    let names: Vec<_> = components(Theme::default())
        .into_iter()
        .map(|component| component.name)
        .collect();
    let unique: BTreeSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
}

#[test]
fn every_row_kind_has_a_component() {
    // A new row kind must earn a drawing here; this list is the catalogue.
    let names: BTreeSet<_> = components(Theme::default())
        .into_iter()
        .map(|component| component.name)
        .collect();
    for kind in [
        "prompt",
        "prose",
        "thinking",
        "tool_call",
        "file_change",
        "command",
        "explore",
        "subagent",
        "background",
        "image",
        "slash_output",
        "ask",
        "turn_end",
        "stopped",
        "compaction",
        "error",
        "model_switch",
        "boundary",
        "agent_message",
        "auto_review",
        "unrecognized",
    ] {
        assert!(names.contains(format!("row_{kind}").as_str()), "row_{kind}");
    }
}
