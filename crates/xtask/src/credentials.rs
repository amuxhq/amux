//! The credentials a release uses, from the command the operator names with
//! `git config amux.credentials`, as `scripts/credentials.py` reads them.
//!
//! Asked for one credential by name, the command prints its value and exits
//! 0, or exits non-zero when it holds none. A credential is read when it is
//! used and never printed; one a tool needs as a file exists only in a
//! directory this user alone can read, for as long as the [`File`] lives.

use std::path::{Path, PathBuf};
use std::process::Command;

fn command() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let found = Command::new("git")
        .args(["config", "--get", "amux.credentials"])
        .output()?;
    let words: Vec<String> = String::from_utf8_lossy(&found.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if !found.status.success() || words.is_empty() {
        return Err(
            "no credential command; set one with `git config amux.credentials <command>` \
                    (docs/RELEASE.md)"
                .into(),
        );
    }
    Ok(words)
}

/// Whether a credential command is named, asked without running it.
pub(crate) fn configured() -> bool {
    command().is_ok()
}

fn read_bytes(name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let words = command()?;
    let found = Command::new(&words[0])
        .args(&words[1..])
        .arg(name)
        .output()
        .map_err(|error| format!("the credential command cannot run: {error}"))?;
    if !found.status.success() {
        // The command's own message says why; it is its job never to put a
        // value there.
        let reason = String::from_utf8_lossy(&found.stderr);
        let reason = reason.lines().next().unwrap_or("").trim().to_owned();
        let reason = if reason.is_empty() {
            String::new()
        } else {
            format!(" ({reason})")
        };
        return Err(format!("the credential command has no {name}{reason}").into());
    }
    Ok(found.stdout)
}

/// A text credential, without the newline a command ends it with.
pub(crate) fn read(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(read_bytes(name)?)?
        .trim_end_matches('\n')
        .to_owned())
}

/// A credential on disk while this lives.
pub(crate) struct File {
    directory: PathBuf,
    path: PathBuf,
}

impl File {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// The credential `name` as a file named `filename`.
pub(crate) fn file(name: &str, filename: &str) -> Result<File, Box<dyn std::error::Error>> {
    let value = read_bytes(name)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("amux-credential-{}-{nanos}", std::process::id()));
    let builder = &mut std::fs::DirBuilder::new();
    // Readable by this user alone; `create` fails rather than reuse a
    // directory someone else made at that name.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut *builder, 0o700);
    builder.create(&directory)?;
    let file = File {
        path: directory.join(filename),
        directory,
    };
    std::fs::write(&file.path, value)?;
    Ok(file)
}
