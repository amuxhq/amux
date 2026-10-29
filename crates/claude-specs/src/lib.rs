//! Recorded Claude protocol corpus checks kept out of product dependency graphs.

pub mod driver;
pub mod specs;
pub mod terminal;

use std::path::{Path, PathBuf};

pub fn validate_recordings(root: &Path) -> Result<usize, String> {
    validate_tree(root)
}

fn validate_tree(root: &Path) -> Result<usize, String> {
    let mut rows = 0;
    for entry in std::fs::read_dir(root).map_err(|error| format!("{}: {error}", root.display()))? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            rows += validate_tree(&path)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            rows += validate_file(&path)?;
        }
    }
    Ok(rows)
}

fn validate_file(path: &PathBuf) -> Result<usize, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut rows = 0;
    for (index, line) in text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        serde_json::from_str::<serde_json::Value>(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), index + 1))?;
        rows += 1;
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    #[test]
    fn recorded_claude_rows_are_valid_json() {
        let count = super::validate_recordings(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .as_path(),
        )
        .unwrap();
        assert!(count > 100, "expected the maintained Claude corpus");
    }
}
