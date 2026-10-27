//! `amux config`: one setting at a time in the installation's config file.
//!
//! A key is a dotted path into the file (`ui.theme`, `new_agent.claude.model`),
//! one of [`settings::INSTALLATION_KEYS`]. A value is read as TOML where it
//! is one (`true`, `300`, `["~/source"]`) and as a string otherwise, so
//! `amux config set channel preview` needs no quotes. A write changes that
//! key alone, comments and layout kept, and only when the file still reads
//! after it. The daemon and the supervisor read the file as they need each
//! setting; what a running one picks up when is the setting's own.

use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use settings::{INSTALLATION_KEYS, InstallationConfig};
use toml_edit::{DocumentMut, Item};

/// `amux config get`: the value in effect, the default when the file does
/// not set it.
pub fn get(path: &Path, key: &str) -> Result<()> {
    known(key)?;
    let config = if path.exists() {
        InstallationConfig::from_file(path)?
    } else {
        InstallationConfig::default()
    };
    // The two settings that are absent until set resolve as the daemon
    // resolves them.
    let value = match key {
        "host_name" => toml::Value::String(config.host_name()),
        "updates" => toml::Value::try_from(config.updates()?)?,
        _ => {
            let table = toml::Value::try_from(&config)?;
            key.split('.')
                .try_fold(&table, |value, part| value.get(part))
                .cloned()
                .ok_or_else(|| anyhow!("{key} has no value"))?
        }
    };
    match value {
        toml::Value::String(text) => println!("{text}"),
        other => println!("{other}"),
    }
    Ok(())
}

/// `amux config set`: writes one key, refusing a value the file would not
/// read with.
pub fn set(path: &Path, key: &str, raw: &str) -> Result<()> {
    known(key)?;
    let document = read(path)?;
    // As TOML first, then as the string it is when that is refused: a host
    // named `true` is still a name.
    let mut candidates = Vec::new();
    if let Some(typed) = typed(raw) {
        candidates.push(typed);
    }
    candidates.push(toml_edit::Value::from(raw));
    let mut first_refusal = None;
    for value in candidates {
        let mut edited = document.clone();
        *slot(&mut edited, key) = toml_edit::value(value.clone());
        match InstallationConfig::from_toml(&edited.to_string())
            .and_then(|config| config.validate().map(|()| config))
        {
            Ok(_) => {
                write(path, &edited)?;
                println!("{key} = {}", value.to_string().trim());
                if let Some(note) = note(key) {
                    println!("{note}");
                }
                return Ok(());
            }
            Err(error) => {
                first_refusal.get_or_insert(error);
            }
        }
    }
    // A parse error quotes the file it read, which here is the edited copy;
    // its last line is the reason.
    match first_refusal {
        Some(error) => {
            let error = error.to_string();
            let reason = error
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or(&error);
            bail!("{key} cannot be {raw}: {reason}")
        }
        None => unreachable!("a raw string is always a candidate"),
    }
}

/// `amux config unset`: removes one key, so its default applies.
pub fn unset(path: &Path, key: &str) -> Result<()> {
    known(key)?;
    let mut document = read(path)?;
    let (parents, last) = match key.rsplit_once('.') {
        Some((parents, last)) => (parents.split('.').collect::<Vec<_>>(), last),
        None => (Vec::new(), key),
    };
    let table = parents
        .iter()
        .try_fold(document.as_item_mut(), |item, part| item.get_mut(part))
        .and_then(Item::as_table_like_mut);
    if table.and_then(|table| table.remove(last)).is_none() {
        println!(
            "{key} is not set in {}; its default applies.",
            path.display()
        );
        return Ok(());
    }
    write(path, &document)?;
    println!("{key} is no longer set; its default applies.");
    Ok(())
}

/// `amux config edit`: the file in the person's editor, read back after so
/// a mistake is said at once rather than when amux next starts.
pub fn edit(path: &Path) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, "").with_context(|| format!("creating {}", path.display()))?;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.to_owned());
    // An editor is often given with its arguments (`code --wait`), so a
    // shell reads it, as git does.
    let status = if cfg!(windows) {
        Command::new(&editor).arg(path).status()
    } else {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(&editor)
            .arg(path)
            .status()
    }
    .with_context(|| format!("starting {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}; the file is as it left it");
    }
    let config = InstallationConfig::from_file(path).with_context(|| {
        format!(
            "{} does not read; `amux config edit` opens it again",
            path.display()
        )
    })?;
    for key in &config.ignored {
        eprintln!("amux: {key} is not a setting this amux knows, so it is ignored");
    }
    Ok(())
}

fn known(key: &str) -> Result<()> {
    if INSTALLATION_KEYS.contains(&key) {
        return Ok(());
    }
    bail!(
        "{key} is not a setting; the settings are {}",
        INSTALLATION_KEYS.join(", ")
    )
}

/// The value `raw` is as TOML, if it is one.
fn typed(raw: &str) -> Option<toml_edit::Value> {
    let document = format!("value = {raw}").parse::<DocumentMut>().ok()?;
    document.get("value")?.as_value().cloned()
}

/// The key's place in the document, its tables made where they are missing
/// as the sections a person writes (`[new_agent.claude]`), not inline.
fn slot<'a>(document: &'a mut DocumentMut, key: &str) -> &'a mut Item {
    let mut item = document.as_item_mut();
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() && item.get(part).is_none() {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            item[part] = Item::Table(table);
        }
        item = &mut item[part];
    }
    item
}

fn read(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("reading {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn write(path: &Path, document: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    node::write_durably(path, document.to_string().as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// When a running amux picks the change up, for the settings where that is
/// not simply the next time it reads them.
fn note(key: &str) -> Option<&'static str> {
    match key {
        "channel" => {
            Some("The supervisor follows it from its next check; `amux update` checks now.")
        }
        "telemetry" => Some(
            "A running daemon stops sending at once; one that started with telemetry off \
             starts sending when it next starts.",
        ),
        "supervisor" | "updates" | "root" | "front_door_socket" | "host_name" => {
            Some("amux uses it from its next start: `amux server stop`, then any amux command.")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_set_keeps_the_rest_of_the_file_and_types_the_value() {
        let (_dir, path) = file("# mine\nchannel = \"stable\"\n\n[ui]\ntheme = \"dark\" # kept\n");
        set(&path, "channel", "preview").unwrap();
        set(&path, "telemetry", "false").unwrap();
        set(&path, "ui.chat_in", "native").unwrap();
        set(&path, "new_agent.claude.model", "sonnet").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\nchannel = \"preview\"\n"),
            "{text}"
        );
        assert!(text.contains("theme = \"dark\" # kept"), "{text}");
        assert!(text.contains("chat_in = \"native\""), "{text}");
        assert!(text.contains("telemetry = false"), "{text}");
        let config = InstallationConfig::from_file(&path).unwrap();
        assert_eq!(config.new_agent.claude.model, "sonnet");
        assert!(!config.telemetry);
    }

    #[test]
    fn a_value_that_reads_as_toml_but_not_for_the_key_is_taken_as_a_string() {
        let (_dir, path) = file("");
        set(&path, "host_name", "true").unwrap();
        assert_eq!(
            InstallationConfig::from_file(&path)
                .unwrap()
                .host_name
                .as_deref(),
            Some("true")
        );
    }

    #[test]
    fn a_bad_value_or_an_unknown_key_leaves_the_file_alone() {
        let (_dir, path) = file("channel = \"stable\"\n");
        let error = set(&path, "channel", "nightly").unwrap_err().to_string();
        assert!(error.contains("channel cannot be nightly"), "{error}");
        let error = set(&path, "theme", "dark").unwrap_err().to_string();
        assert!(error.contains("theme is not a setting"), "{error}");
        assert!(error.contains("ui.theme"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "channel = \"stable\"\n"
        );
    }

    #[test]
    fn unset_removes_the_key_alone() {
        let (_dir, path) =
            file("channel = \"preview\"\n[ui]\ntheme = \"dark\"\ncolor = \"ansi\"\n");
        unset(&path, "ui.theme").unwrap();
        unset(&path, "ui.theme").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "channel = \"preview\"\n[ui]\ncolor = \"ansi\"\n"
        );
    }
}
