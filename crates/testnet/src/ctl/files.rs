//! Files a session makes for itself to test with: a large attachment, a
//! long paste. A tester writes only inside its own folder, and nothing in
//! its shell holds a command's arguments to that, so the files are made
//! here, where the path is checked.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use clap::ValueEnum;
use serde_json::json;

use super::session::{Folder, Outcome};

/// The largest file a session makes: enough for any attachment test, small
/// enough that a run of testers cannot fill the disk by accident.
const LARGEST: u64 = 2 << 30;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Fill {
    /// Zero bytes.
    Zero,
    /// Bytes that do not compress.
    Random,
    /// Numbered lines of words, for pastes and text attachments.
    Text,
}

pub(crate) fn make(
    folder: &Folder,
    name: &Path,
    size: &str,
    fill: Fill,
) -> Result<Outcome, String> {
    let bytes = parse_size(size)?;
    if bytes > LARGEST {
        return Err(format!("{size} is larger than the 2g a session may make"));
    }
    let path = inside(&folder.dir, name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(&path)
            .map_err(|error| format!("making {}: {error}", path.display()))?,
    );
    let mut chunk = vec![0_u8; 1 << 20];
    let mut state: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    let mut line = 0_u64;
    let mut text = String::new();
    let mut left = bytes;
    while left > 0 {
        let take = left.min(chunk.len() as u64) as usize;
        let piece: &[u8] = match fill {
            Fill::Zero => &chunk[..take],
            Fill::Random => {
                for byte in &mut chunk[..take] {
                    // xorshift: quick, and incompressible enough.
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    *byte = state as u8;
                }
                &chunk[..take]
            }
            Fill::Text => {
                while text.len() < take {
                    line += 1;
                    text.push_str(&format!(
                        "line {line}: the quick brown fox jumps over the lazy dog\n"
                    ));
                }
                let rest = text.split_off(take);
                chunk[..take].copy_from_slice(text.as_bytes());
                text = rest;
                &chunk[..take]
            }
        };
        file.write_all(piece).map_err(|error| error.to_string())?;
        left -= take as u64;
    }
    file.flush().map_err(|error| error.to_string())?;
    Ok(Outcome::new(
        format!("made {} ({bytes} bytes)", path.display()),
        json!({"path": path, "bytes": bytes}),
    ))
}

/// NAME inside the folder, or why not: it must be relative, never step
/// up, and land in the folder once links are followed.
fn inside(dir: &Path, name: &Path) -> Result<PathBuf, String> {
    let refused = || {
        format!(
            "{} is not inside your folder: name a file below it, without '..'",
            name.display()
        )
    };
    if name.as_os_str().is_empty()
        || !name
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(refused());
    }
    let path = dir.join(name);
    // A folder already below it could be a link that leads out.
    let root = dir.canonicalize().map_err(|error| error.to_string())?;
    let mut existing = path.parent().unwrap_or(dir).to_path_buf();
    while !existing.exists() {
        existing = existing.parent().unwrap_or(dir).to_path_buf();
    }
    let real = existing.canonicalize().map_err(|error| error.to_string())?;
    if !real.starts_with(&root) || path.is_symlink() {
        return Err(refused());
    }
    Ok(path)
}

/// `50m`, `1.5g`, `300k` or a count of bytes.
fn parse_size(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let (number, unit) = match lower.chars().last() {
        Some('k') => (&lower[..lower.len() - 1], 1_u64 << 10),
        Some('m') => (&lower[..lower.len() - 1], 1 << 20),
        Some('g') => (&lower[..lower.len() - 1], 1 << 30),
        _ => (lower.as_str(), 1),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a size: try 50m, 1.5g, 300k or a count of bytes"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{text:?} is not a size"));
    }
    Ok((value * unit as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_a_person_writes_them() {
        assert_eq!(parse_size("50m"), Ok(50 << 20));
        assert_eq!(parse_size("1.5G"), Ok(3 << 29));
        assert_eq!(parse_size("300k"), Ok(300 << 10));
        assert_eq!(parse_size("12"), Ok(12));
        assert!(parse_size("lots").is_err());
    }

    #[test]
    fn a_name_that_leads_out_of_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let outside = tempfile::tempdir().unwrap();
        for name in ["../x", "/tmp/x", "a/../../x", ""] {
            assert!(inside(dir.path(), Path::new(name)).is_err(), "{name}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
            assert!(inside(dir.path(), Path::new("out/x")).is_err());
        }
        assert!(inside(dir.path(), Path::new("files/big.bin")).is_ok());
        assert!(inside(dir.path(), Path::new("ünïcode name.txt")).is_ok());
    }
}
