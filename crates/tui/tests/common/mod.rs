//! What the golden tests share: where goldens live and how a drawn buffer
//! is captured and compared.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use tui::Theme;

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// The buffer's text, then its style classes, one character per cell.
pub fn capture(buffer: &Buffer, theme: Theme) -> String {
    let area = buffer.area;
    let mut text = String::new();
    let mut styles = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            line.push_str(cell.symbol());
            styles.push(theme.classify(cell.style()));
        }
        text.push_str(line.trim_end());
        text.push('\n');
        styles.push('\n');
    }
    format!("{text}--- styles\n{styles}")
}

/// Compares `rendered` with the golden `name`, or rewrites it under
/// UPDATE_GOLDENS outside CI.
pub fn assert_golden(name: &str, rendered: &str) {
    let path = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "UPDATE_GOLDENS is refused in CI: rewrite goldens locally and review them"
        );
        std::fs::create_dir_all(golden_dir()).expect("golden directory");
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with UPDATE_GOLDENS=1 and review"));
    assert!(
        rendered == expected,
        "{name} differs from its golden; if intended, rerun with UPDATE_GOLDENS=1 and review.\n\
         rendered:\n{rendered}"
    );
}
