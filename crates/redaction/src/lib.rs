//! The structural redactor for what is not a per-kind body: free text and
//! JSON in reports, logs, specs and provider captures. It knows key names
//! and text shapes (secrets, machine paths, emails, the local user and
//! host) and nothing about any record's schema; bodies are redacted per
//! kind in `interpret`, the only code that can decode them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

pub const SECRET_PLACEHOLDER: &str = "<REDACTED>";
pub const EMAIL_PLACEHOLDER: &str = "<REDACTED_EMAIL>";
pub const IDENTIFIER_PLACEHOLDER: &str = "<REDACTED_IDENTIFIER>";
pub const PATH_PLACEHOLDER: &str = "<MACHINE_PATH>";

/// Tokens whose prefix names them as credentials wherever they appear.
const TOKEN_PREFIXES: &[&str] = &[
    "sk-proj-",
    "sk-svcacct-",
    "sk-admin-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
];

/// Parts of a variable or field name that mark its value as secret.
const SECRET_NAME_PARTS: &[&str] = &[
    "KEY",
    "APIKEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "AUTH",
    "PAT",
];

/// A local name shorter than this is not redacted as a literal: "al" or
/// "mac" would be cut out of every word that contains it.
const SHORTEST_LITERAL: usize = 4;

/// Capture-specific values that supplement the sanitizer's structural rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Redaction {
    pub home: PathBuf,
    pub extra_paths: Vec<PathBuf>,
    pub secret_env: Vec<String>,
    /// Host name whose literal appearances should not leave the capture.
    pub hostname: Option<String>,
    /// Local user name whose literal appearances should not leave the capture.
    pub user: Option<String>,
    /// Other exact identifiers whose literal appearances should not leave the capture.
    pub extra_personal_identifiers: Vec<String>,
    /// Exact JSON field names whose values this capture treats as personal identifiers.
    pub personal_identifier_keys: Vec<String>,
}

impl Redaction {
    /// The rules for a report leaving this machine: its home directory, the
    /// local user's name and the host's name, long and short.
    pub fn for_this_machine() -> Redaction {
        let long_enough = |name: String| (name.len() >= SHORTEST_LITERAL).then_some(name);
        let hostname = gethostname::gethostname().to_string_lossy().into_owned();
        let short = hostname.split('.').next().unwrap_or_default().to_owned();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default();
        Redaction {
            home,
            hostname: long_enough(hostname.clone()),
            user: std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .ok()
                .and_then(long_enough),
            extra_personal_identifiers: (short != hostname)
                .then_some(short)
                .and_then(long_enough)
                .into_iter()
                .collect(),
            ..Redaction::default()
        }
    }
}

/// Counts the sensitive values replaced while producing a diagnostic report.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionSummary {
    #[serde(default)]
    pub secrets: u64,
    #[serde(default)]
    pub machine_paths: u64,
    #[serde(default)]
    pub personal_identifiers: u64,
}

/// Redact one parsed JSON value with the shared capture rules.
pub fn redact_value(value: &mut Value, rules: &Redaction, summary: &mut RedactionSummary) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if should_redact_personal_identifier(key, rules) && !value.is_null() {
                    if value.as_str() != Some(IDENTIFIER_PLACEHOLDER) {
                        *value = Value::String(IDENTIFIER_PLACEHOLDER.to_string());
                        summary.personal_identifiers += 1;
                    }
                } else if is_sensitive_key(key) && !value.is_null() {
                    if value.as_str() != Some(SECRET_PLACEHOLDER) {
                        *value = Value::String(SECRET_PLACEHOLDER.to_string());
                        summary.secrets += 1;
                    }
                } else if is_path_key(key) {
                    sanitize_path_value(value, rules, summary);
                } else {
                    redact_value(value, rules, summary);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_value(value, rules, summary);
            }
        }
        Value::String(value) => *value = redact_text(value, rules, summary),
        _ => {}
    }
}

fn should_redact_personal_identifier(key: &str, rules: &Redaction) -> bool {
    is_personal_identifier_key(key)
        || rules
            .personal_identifier_keys
            .iter()
            .any(|identifier_key| identifier_key == key)
}

/// Whether a JSON field name conventionally identifies a person or their account.
///
/// Capture-specific field names remain on [`Redaction::personal_identifier_keys`].
/// This shared rule is public so evidence tooling can reject the same account,
/// organization, user, and bridge identifiers that corpus sanitization removes.
fn is_personal_identifier_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    normalized == "bridgesessionid"
        || matches!(
            normalized.as_str(),
            "email" | "userid" | "useruuid" | "organization"
        )
        || ((normalized.contains("account") || normalized.contains("organization"))
            && (normalized.ends_with("id")
                || normalized.ends_with("uuid")
                || normalized.ends_with("name")))
}

fn sanitize_path_value(value: &mut Value, rules: &Redaction, summary: &mut RedactionSummary) {
    match value {
        Value::String(path)
            if path != PATH_PLACEHOLDER && is_absolute_machine_path(path.as_str()) =>
        {
            *path = PATH_PLACEHOLDER.to_string();
            summary.machine_paths += 1;
        }
        Value::Array(values) => {
            for value in values {
                sanitize_path_value(value, rules, summary);
            }
        }
        _ => redact_value(value, rules, summary),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "token"
            | "apikey"
            | "authorization"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "password"
            | "passwd"
            | "cookie"
            | "setcookie"
            | "clientsecret"
            | "secretkey"
            | "credential"
    ) || normalized.ends_with("apikey")
}

fn is_path_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "cwd" | "path" | "filepath" | "workingdirectory" | "homedir" | "projectdir" | "rootdir"
    ) || normalized.ends_with("path")
        || normalized.ends_with("directory")
        || normalized.ends_with("directories")
}

/// A field or variable name lowercased with everything but letters and
/// digits dropped, as every key rule compares it.
pub fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_absolute_machine_path(value: &str) -> bool {
    value.starts_with('/')
        || (value.len() >= 3
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'\\' | b'/'))
}

/// Redact secrets, machine paths, and local machine identifiers in free text.
pub fn redact_text(input: &str, rules: &Redaction, summary: &mut RedactionSummary) -> String {
    let mut value = input.to_string();
    for secret in &rules.secret_env {
        if !secret.is_empty() && secret != SECRET_PLACEHOLDER {
            value = redact_literal(&value, secret, SECRET_PLACEHOLDER, &mut summary.secrets);
        }
    }
    for marker in [
        "sk-ant-",
        "Bearer ",
        "ANTHROPIC_API_KEY=",
        "CLAUDE_CODE_OAUTH_TOKEN=",
    ] {
        value = redact_spans(&value, marker, SECRET_PLACEHOLDER, &mut summary.secrets);
    }
    value = prefixed_tokens(&value, &mut summary.secrets);
    value = secret_assignments(&value, &mut summary.secrets);
    value = redact_email_addresses(&value, summary);

    let mut paths = Vec::with_capacity(rules.extra_paths.len() + 1);
    if !rules.home.as_os_str().is_empty() {
        paths.push(rules.home.to_string_lossy().into_owned());
    }
    paths.extend(
        rules
            .extra_paths
            .iter()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.to_string_lossy().into_owned()),
    );
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    paths.dedup();
    for path in paths {
        value = redact_spans(&value, &path, PATH_PLACEHOLDER, &mut summary.machine_paths);
    }
    for marker in [
        "/Users/",
        "/home/",
        "/private/var/folders/",
        "/var/folders/",
        "/tmp/",
        "/Volumes/",
        "/workspace/",
        "/root/",
        "/opt/",
        "/usr/local/",
    ] {
        value = redact_spans(&value, marker, PATH_PLACEHOLDER, &mut summary.machine_paths);
    }
    value = redact_windows_user_paths(&value, summary);
    // The host's full name before its short one, which it contains.
    for identifier in [&rules.hostname, &rules.user].into_iter().flatten() {
        if !identifier.is_empty() && identifier != IDENTIFIER_PLACEHOLDER {
            value = redact_word(
                &value,
                identifier,
                IDENTIFIER_PLACEHOLDER,
                &mut summary.personal_identifiers,
            );
        }
    }
    for identifier in &rules.extra_personal_identifiers {
        if !identifier.is_empty() && identifier != IDENTIFIER_PLACEHOLDER {
            value = redact_width_matched_identifier(&value, identifier, summary);
        }
    }
    value
}

fn redact_width_matched_identifier(
    input: &str,
    identifier: &str,
    summary: &mut RedactionSummary,
) -> String {
    const LABEL: &str = "<REDACTED>";

    let width = UnicodeWidthStr::width(identifier);
    let label_width = UnicodeWidthStr::width(LABEL);
    let replacement = if width >= label_width {
        format!("{LABEL}{}", "_".repeat(width - label_width))
    } else {
        "#".repeat(width)
    };
    if replacement == identifier {
        return input.to_string();
    }
    redact_word(
        input,
        identifier,
        &replacement,
        &mut summary.personal_identifiers,
    )
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Tokens with a credential prefix, at the start of a word, to the end of
/// the token.
fn prefixed_tokens(text: &str, count: &mut u64) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || !is_word_byte(bytes[index - 1]);
        let prefix = at_word_start
            .then(|| {
                TOKEN_PREFIXES
                    .iter()
                    .find(|prefix| bytes[index..].starts_with(prefix.as_bytes()))
            })
            .flatten();
        let Some(prefix) = prefix else {
            index += 1;
            continue;
        };
        let mut end = index + prefix.len();
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
        }
        // A bare prefix is a word, not a token.
        if end - index < prefix.len() + 8 {
            index = end;
            continue;
        }
        out.push_str(&text[kept..index]);
        out.push_str(SECRET_PLACEHOLDER);
        *count += 1;
        kept = end;
        index = end;
    }
    out.push_str(&text[kept..]);
    out
}

/// `NAME=value`, `name: value` and `"name": "value"` where the name says
/// secret: the value is blanked, keeping its quotes.
fn secret_assignments(text: &str, count: &mut u64) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'=' | b':') || bytes.get(index + 1) == Some(&b'=') {
            index += 1;
            continue;
        }
        let mut name_end = index;
        while name_end > kept && bytes[name_end - 1] == b' ' {
            name_end -= 1;
        }
        let quoted_name = name_end > kept && matches!(bytes[name_end - 1], b'"' | b'\'');
        if quoted_name {
            name_end -= 1;
        }
        let mut start = name_end;
        while start > kept && (is_word_byte(bytes[start - 1]) || bytes[start - 1] == b'-') {
            start -= 1;
        }
        if !is_secret_name(&text[start..name_end]) {
            index += 1;
            continue;
        }
        let mut value_start = index + 1;
        while bytes.get(value_start) == Some(&b' ') {
            value_start += 1;
        }
        let (value_start, value_end, resume) = match bytes.get(value_start) {
            Some(quote @ (b'"' | b'\'')) => {
                let inner = value_start + 1;
                let close = text[inner..]
                    .find(*quote as char)
                    .map(|at| inner + at)
                    .unwrap_or(bytes.len());
                (inner, close, (close + 1).min(bytes.len()))
            }
            _ => {
                let end = text[value_start..]
                    .find(|c: char| {
                        c.is_whitespace() || matches!(c, ';' | '&' | '|' | '"' | '\'' | ',' | '}')
                    })
                    .map(|at| value_start + at)
                    .unwrap_or(bytes.len());
                (value_start, end, end)
            }
        };
        let value = &text[value_start..value_end];
        if !value.is_empty() && value != SECRET_PLACEHOLDER {
            out.push_str(&text[kept..value_start]);
            out.push_str(SECRET_PLACEHOLDER);
            *count += 1;
            kept = value_end;
        }
        index = resume.max(index + 1);
    }
    out.push_str(&text[kept..]);
    out
}

fn is_secret_name(name: &str) -> bool {
    let whole = normalized_key(name);
    !whole.is_empty()
        && (name
            .split(['_', '-', '.'])
            .any(|part| SECRET_NAME_PARTS.contains(&part.to_ascii_uppercase().as_str()))
            || [
                "apikey",
                "token",
                "secret",
                "password",
                "passwd",
                "credential",
            ]
            .iter()
            .any(|suffix| whole.ends_with(suffix)))
}

fn redact_literal(input: &str, needle: &str, replacement: &str, count: &mut u64) -> String {
    let matches = input.matches(needle).count() as u64;
    if matches == 0 {
        return input.to_string();
    }
    *count += matches;
    input.replace(needle, replacement)
}

/// Replaces `needle` where it stands as a word of its own: a name inside a
/// longer word ("nova" in "innovation") is someone else's word, and
/// replacing it there would change text a replay then compares.
fn redact_word(input: &str, needle: &str, replacement: &str, count: &mut u64) -> String {
    let bytes = input.as_bytes();
    let bounded = |at: usize| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + needle.len()).copied();
        !before.is_some_and(is_word_byte) && !after.is_some_and(is_word_byte)
    };
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    for (at, _) in input.match_indices(needle) {
        if at < copied || !bounded(at) {
            continue;
        }
        output.push_str(&input[copied..at]);
        output.push_str(replacement);
        copied = at + needle.len();
        *count += 1;
    }
    if copied == 0 {
        return input.to_string();
    }
    output.push_str(&input[copied..]);
    output
}

fn redact_spans(input: &str, marker: &str, replacement: &str, count: &mut u64) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find(marker) {
        output.push_str(&remainder[..start]);
        let token = &remainder[start..];
        let end = token
            .char_indices()
            .skip_while(|(index, _)| *index < marker.len())
            .find(|(_, character)| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
            })
            .map(|(index, _)| index)
            .unwrap_or(token.len());
        output.push_str(replacement);
        remainder = &token[end..];
        *count += 1;
    }
    output.push_str(remainder);
    output
}

fn is_email_local_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
}

fn is_email_domain(domain: &str) -> bool {
    let Some((_, top_level)) = domain.rsplit_once('.') else {
        return false;
    };
    top_level.len() >= 2 && top_level.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn redact_email_addresses(input: &str, summary: &mut RedactionSummary) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut kept = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        let mut start = index;
        while start > kept && is_email_local_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < bytes.len() && is_email_domain_byte(bytes[end]) {
            end += 1;
        }
        while end > index + 1 && bytes[end - 1] == b'.' {
            end -= 1;
        }
        if start == index || !is_email_domain(&input[index + 1..end]) {
            index += 1;
            continue;
        }
        output.push_str(&input[kept..start]);
        output.push_str(EMAIL_PLACEHOLDER);
        summary.personal_identifiers += 1;
        kept = end;
        index = end;
    }
    output.push_str(&input[kept..]);
    output
}

fn redact_windows_user_paths(input: &str, summary: &mut RedactionSummary) -> String {
    let bytes = input.as_bytes();
    for (index, character) in input.char_indices() {
        if index + 9 > bytes.len() {
            break;
        }
        if character.is_ascii_alphabetic()
            && bytes.get(index + 1) == Some(&b':')
            && matches!(bytes.get(index + 2), Some(b'\\') | Some(b'/'))
            && bytes[index + 3..index + 8].eq_ignore_ascii_case(b"users")
            && matches!(bytes.get(index + 8), Some(b'\\') | Some(b'/'))
        {
            let marker = &input[index..index + 9];
            return redact_spans(input, marker, PATH_PLACEHOLDER, &mut summary.machine_paths);
        }
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rules() -> Redaction {
        Redaction {
            home: PathBuf::from("/Users/someone"),
            secret_env: vec!["s3cret-value".into()],
            hostname: Some("desk.local".into()),
            user: Some("someone".into()),
            ..Redaction::default()
        }
    }

    #[test]
    fn json_is_redacted_by_key_and_keeps_its_shape() {
        let mut value = json!({
            "apiKey": "sk-ant-abc",
            "cwd": "/Users/someone/src",
            "nested": {"accountUuid": "1234", "note": "fine"},
            "paths": {"workingDirectory": ["/tmp/a", "relative/b"]},
            "absent": {"token": null},
        });
        let mut summary = RedactionSummary::default();
        redact_value(&mut value, &rules(), &mut summary);
        assert_eq!(
            value,
            json!({
                "apiKey": SECRET_PLACEHOLDER,
                "cwd": PATH_PLACEHOLDER,
                "nested": {"accountUuid": IDENTIFIER_PLACEHOLDER, "note": "fine"},
                "paths": {"workingDirectory": [PATH_PLACEHOLDER, "relative/b"]},
                "absent": {"token": null},
            })
        );
        assert_eq!(
            summary,
            RedactionSummary {
                secrets: 1,
                machine_paths: 2,
                personal_identifiers: 1,
            }
        );
        // Redacting again changes nothing and counts nothing.
        let before = value.clone();
        let mut again = RedactionSummary::default();
        redact_value(&mut value, &rules(), &mut again);
        assert_eq!(value, before);
        assert_eq!(again, RedactionSummary::default());
    }

    #[test]
    fn text_loses_secrets_emails_and_the_machine_it_came_from() {
        let mut summary = RedactionSummary::default();
        let text = redact_text(
            "key sk-ant-api03-XYZ and s3cret-value from someone@example.com on desk.local \
             in /Users/someone/src and C:\\Users\\someone\\x",
            &rules(),
            &mut summary,
        );
        for gone in [
            "sk-ant-api03-XYZ",
            "s3cret-value",
            "someone@example.com",
            "desk.local",
        ] {
            assert!(!text.contains(gone), "{gone} survived in {text}");
        }
        assert!(text.contains(SECRET_PLACEHOLDER));
        assert!(text.contains(EMAIL_PLACEHOLDER));
        assert!(summary.secrets >= 2 && summary.personal_identifiers >= 1);
    }

    #[test]
    fn secret_assignments_blank_secret_names_only() {
        assert_eq!(
            secret_assignments("GITHUB_TOKEN=abc123 PATH=/bin make", &mut 0),
            "GITHUB_TOKEN=<REDACTED> PATH=/bin make"
        );
        assert_eq!(
            secret_assignments("export OPENAI_API_KEY=\"sk x\"; echo", &mut 0),
            "export OPENAI_API_KEY=\"<REDACTED>\"; echo"
        );
        assert_eq!(
            secret_assignments("{\"api_key\": \"abc\", \"rows\": 3} apiKey: xyz", &mut 0),
            "{\"api_key\": \"<REDACTED>\", \"rows\": 3} apiKey: <REDACTED>"
        );
        assert_eq!(
            secret_assignments("a == b; MONKEY=1 https://x max_tokens=5", &mut 0),
            "a == b; MONKEY=1 https://x max_tokens=5"
        );
    }

    #[test]
    fn prefixed_tokens_need_a_word_start_and_a_body() {
        assert_eq!(
            prefixed_tokens("use ghp_0123456789abcdef now", &mut 0),
            "use <REDACTED> now"
        );
        assert_eq!(prefixed_tokens("ghp_ alone", &mut 0), "ghp_ alone");
        assert_eq!(
            prefixed_tokens("xAKIA0123456789ABCDEF", &mut 0),
            "xAKIA0123456789ABCDEF"
        );
    }

    #[test]
    fn text_loses_prefixed_tokens_and_secret_assignments() {
        let mut summary = RedactionSummary::default();
        let text = redact_text(
            "pushed with ghp_PLANTEDgithub0002abcdef; GITHUB_TOKEN=plantedvalue and \
             \"api_key\": \"planted-key\" but ghp_ alone and PORT=8080 stay",
            &Redaction::default(),
            &mut summary,
        );
        for gone in ["ghp_PLANTEDgithub0002abcdef", "plantedvalue", "planted-key"] {
            assert!(!text.contains(gone), "{gone} survived in {text}");
        }
        assert!(
            text.contains("ghp_ alone") && text.contains("PORT=8080"),
            "{text}"
        );
        assert_eq!(summary.secrets, 3);
    }

    #[test]
    fn a_name_is_redacted_as_a_word_and_left_inside_other_words() {
        let rules = Redaction {
            hostname: Some("nova.lan".to_owned()),
            user: Some("ada-l".to_owned()),
            extra_personal_identifiers: vec!["nova".to_owned()],
            ..Redaction::default()
        };
        let mut summary = RedactionSummary::default();
        let text = redact_text(
            "ada-l on nova.lan, then nova: innovation, novas, ada-love",
            &rules,
            &mut summary,
        );
        assert_eq!(
            text,
            "<REDACTED_IDENTIFIER> on <REDACTED_IDENTIFIER>, then ####: innovation, novas, ada-love"
        );
        assert_eq!(summary.personal_identifiers, 3);
    }

    #[test]
    fn the_rules_for_this_machine_name_its_host_user_and_home() {
        let rules = Redaction::for_this_machine();
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let mut summary = RedactionSummary::default();
        let text = redact_text(
            &format!("{user} on {host} in {home}/src"),
            &rules,
            &mut summary,
        );
        for (name, value) in [("host", &host), ("user", &user), ("home", &home)] {
            if value.len() >= SHORTEST_LITERAL {
                assert!(
                    !text.contains(value.as_str()),
                    "the {name} survived in {text}"
                );
            }
        }
    }
}
