//! Changing keys in the installation's file as a person would: the key
//! alone, the rest of the file (comments, layout, keys this version does
//! not know) kept as it was. `amux config` and the daemon's settings call
//! both write through it, so either leaves a file the other reads.

use toml_edit::{DocumentMut, Item};

use crate::{ConfigError, InstallationConfig};

/// One key's change: its new value, or none to remove it so its default
/// applies.
pub type Change<'a> = (&'a str, Option<toml_edit::Value>);

/// `text` with every change made, and the config it reads as. Refused, with
/// the reason, when the result would not read or validate; `text` that does
/// not parse as TOML at all is refused too, since editing it would lose it.
pub fn edit(text: &str, changes: &[Change]) -> Result<(String, InstallationConfig), ConfigError> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| ConfigError::Invalid(error.to_string()))?;
    for (key, value) in changes {
        match value {
            Some(value) => *slot(&mut document, key)? = toml_edit::value(value.clone()),
            None => remove(&mut document, key),
        }
    }
    let edited = document.to_string();
    let config = InstallationConfig::from_toml(&edited)?;
    config.validate()?;
    Ok((edited, config))
}

/// The value `raw` is as TOML, if it is one: `true`, `300`, `["~/src"]`.
pub fn typed(raw: &str) -> Option<toml_edit::Value> {
    let document = format!("value = {raw}").parse::<DocumentMut>().ok()?;
    document.get("value")?.as_value().cloned()
}

/// Whether the file sets `key` at all.
pub fn is_set(text: &str, key: &str) -> bool {
    let Ok(document) = text.parse::<DocumentMut>() else {
        return false;
    };
    key.split('.')
        .try_fold(document.as_item(), |item, part| item.get(part))
        .is_some()
}

/// The key's place in the document, its tables made where they are missing
/// as the sections a person writes (`[new_agent.claude]`), not inline.
/// Refused when a key on the way is set to a value: the file says something
/// else there, which only the person can sort out.
fn slot<'a>(document: &'a mut DocumentMut, key: &str) -> Result<&'a mut Item, ConfigError> {
    let mut item = document.as_item_mut();
    let mut path: Vec<&str> = Vec::new();
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        if parts.peek().is_some() {
            // What is there now, read before `item` is changed.
            let there = item
                .get(part)
                .map(|inside| (inside.is_table_like(), inside.to_string()));
            match there {
                None => {
                    let mut table = toml_edit::Table::new();
                    table.set_implicit(true);
                    item[part] = Item::Table(table);
                }
                Some((false, value)) => {
                    return Err(ConfigError::InTheWay {
                        key: path.join("."),
                        value: value.trim().to_owned(),
                        setting: key.to_owned(),
                    });
                }
                Some((true, _)) => {}
            }
        }
        // `item` is the document or a table-like item checked above, so
        // indexing it never panics.
        item = &mut item[part];
    }
    Ok(item)
}

fn remove(document: &mut DocumentMut, key: &str) {
    let (parents, last) = match key.rsplit_once('.') {
        Some((parents, last)) => (parents.split('.').collect::<Vec<_>>(), last),
        None => (Vec::new(), key),
    };
    if let Some(table) = parents
        .iter()
        .try_fold(document.as_item_mut(), |item, part| item.get_mut(part))
        .and_then(Item::as_table_like_mut)
    {
        table.remove(last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key whose parent the file sets to a value has nowhere to go: the
    /// edit is refused, naming the key in the way, and nothing panics.
    #[test]
    fn a_value_where_a_table_belongs_refuses_the_edit_and_names_it() {
        let refused = |text: &str, key: &str| {
            edit(text, &[(key, Some("1h".into()))])
                .err()
                .unwrap_or_else(|| panic!("{key} was set under a value"))
                .to_string()
        };
        let why = refused(
            "[experiments]\nsuspension = 7\n",
            "experiments.suspension.after",
        );
        assert!(why.contains("experiments.suspension"), "{why}");
        assert!(why.contains('7'), "{why}");
        let why = refused("ui = 3\n", "ui.theme");
        assert!(why.starts_with("ui "), "{why}");
        let why = refused("[ui]\ntheme = \"dark\"\n", "ui.theme.shade");
        assert!(why.contains("ui.theme"), "{why}");
    }

    #[test]
    fn an_edit_keeps_the_rest_of_the_file_and_refuses_what_would_not_read() {
        let text = "# mine\nchannel = \"stable\"\n\n[ui]\ntheme = \"dark\" # kept\nmystery = 1\n";
        let (edited, config) = edit(
            text,
            &[
                ("keep_awake", Some(false.into())),
                ("new_agent.claude.kind", Some("terminal".into())),
                ("ui.theme", None),
            ],
        )
        .unwrap();
        assert!(
            edited.starts_with("# mine\nchannel = \"stable\"\n"),
            "{edited}"
        );
        assert!(!edited.contains("theme"), "{edited}");
        assert!(edited.contains("mystery = 1"), "{edited}");
        assert!(
            edited.contains("[new_agent.claude]\nkind = \"terminal\""),
            "{edited}"
        );
        assert!(!config.keep_awake);
        assert_eq!(
            config.new_agent.claude_kind,
            Some(crate::ClaudeKindSetting::Terminal)
        );
        assert!(is_set(&edited, "new_agent.claude.kind"));
        assert!(!is_set(&edited, "ui.theme"));

        let refused = edit(text, &[("keep_awake", Some("sometimes".into()))]);
        assert!(refused.is_err());
        assert!(edit("not = [toml", &[]).is_err());
    }
}
