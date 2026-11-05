//! `amux config`: one setting at a time in the installation's config file.
//!
//! A key is a dotted path into the file (`ui.theme`, `new_agent.claude.model`),
//! one of [`settings::installation_keys`]. A value is read as TOML where it
//! is one (`true`, `300`, `["~/source"]`) and as a string otherwise, so
//! `amux config set channel preview` needs no quotes. A write changes that
//! key alone, comments and layout kept, and only when the file still reads
//! after it. The daemon and the supervisor read the file as they need each
//! setting; what a running one picks up when is the setting's own.

use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use settings::edit::{edit as edited_text, typed};
use settings::{InstallationConfig, installation_keys, is_deployment_key};

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
        "host_name" => toml::Value::String(config.host_name().into()),
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
    let text = read(path)?;
    // As TOML first, then as the string it is when that is refused: a host
    // named `true` is still a name.
    let mut candidates = Vec::new();
    if let Some(typed) = typed(raw) {
        candidates.push(typed);
    }
    candidates.push(toml_edit::Value::from(raw));
    let mut first_refusal = None;
    for value in candidates {
        match edited_text(&text, &[(key, Some(value.clone()))]) {
            Ok((edited, _)) => {
                write(path, &edited)?;
                println!("{key} = {}", value.to_string().trim());
                applied(path, key)?;
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
    let text = read(path)?;
    if !settings::edit::is_set(&text, key) {
        println!(
            "{key} is not set in {}; its default applies.",
            path.display()
        );
        return Ok(());
    }
    let (edited, _) = edited_text(&text, &[(key, None)])?;
    write(path, &edited)?;
    println!("{key} is no longer set; its default applies.");
    applied(path, key)?;
    Ok(())
}

/// What a key does beyond the file the moment it is written: the login
/// item follows `start_at_login` whether or not amux is running.
fn applied(path: &Path, key: &str) -> Result<()> {
    if key == "start_at_login" {
        let config = InstallationConfig::from_file(path)?;
        crate::setup::start_at_login(&config, Some(path))?;
    }
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

/// Every key is accepted, but an unknown one is answered with the everyday
/// ones only: how amux is deployed is for whoever deploys it, who knows the
/// keys from the parameters page.
fn known(key: &str) -> Result<()> {
    let keys = installation_keys();
    if keys.iter().any(|known| known == key) {
        return Ok(());
    }
    let everyday: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|key| !is_deployment_key(key))
        .collect();
    bail!(
        "{key} is not a setting; the settings are {}",
        everyday.join(", ")
    )
}

fn read(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    node::write_durably(path, text.as_bytes())
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
            "A running daemon follows it at once: it stops sending, or starts, and nothing \
             recorded while it was off is sent.",
        ),
        "auto_reports" => Some(
            "A running daemon follows it from its next report. Reports go to the account each \
             profile is signed in to, and hold the records of what amux's parts concluded and \
             amux's own masked logs, never what anybody wrote.",
        ),
        "supervisor" | "updates" | "root" => {
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
        assert!(
            !error.contains("releases_url") && !error.contains("agent.grace_secs"),
            "the deployment keys stay out of the everyday list: {error}"
        );
        // They are still settings.
        set(&path, "agent.grace_secs", "60").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "channel = \"stable\"\n\n[agent]\ngrace_secs = 60\n"
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
