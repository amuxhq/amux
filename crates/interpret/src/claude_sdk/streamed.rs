//! A tool call's input while Claude is still writing it: the JSON object
//! arrives in pieces, and the string fields at its top level can be read
//! from whatever prefix has arrived so far.

use std::iter::Peekable;
use std::str::Chars;

/// A top-level string field of a partly written object: its key, its
/// value as far as it has arrived, and whether the value's closing quote
/// has arrived too.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Field {
    pub(super) key: String,
    pub(super) value: String,
    pub(super) complete: bool,
}

/// The string fields at the top level of `json`, an object's text cut off
/// anywhere, in the order they were written. Reading stops at the first
/// field still being written, so only the last field returned can be
/// incomplete. A field whose value is a number, a boolean or null is
/// skipped; one whose value is an object or array ends the reading, as
/// nothing after it is needed to draw a call.
pub(super) fn string_fields(json: &str) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut chars = json.chars().peekable();
    skip_whitespace(&mut chars);
    if chars.next() != Some('{') {
        return fields;
    }
    loop {
        skip_whitespace(&mut chars);
        match chars.next() {
            Some('"') => {}
            Some(',') => continue,
            _ => return fields,
        }
        let (key, true) = string(&mut chars) else {
            return fields;
        };
        skip_whitespace(&mut chars);
        if chars.next() != Some(':') {
            return fields;
        }
        skip_whitespace(&mut chars);
        match chars.peek() {
            Some('"') => {
                chars.next();
                let (value, complete) = string(&mut chars);
                fields.push(Field {
                    key,
                    value,
                    complete,
                });
                if !complete {
                    return fields;
                }
            }
            Some(_) => {
                while let Some(&c) = chars.peek() {
                    match c {
                        ',' | '}' => break,
                        '{' | '[' | '"' => return fields,
                        _ => {
                            chars.next();
                        }
                    }
                }
            }
            None => return fields,
        }
    }
}

fn skip_whitespace(chars: &mut Peekable<Chars>) {
    while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
}

/// A string's contents after its opening quote, decoded as far as they
/// have arrived, and whether its closing quote arrived. An escape cut off
/// partway is left out until the rest of it arrives.
fn string(chars: &mut Peekable<Chars>) -> (String, bool) {
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return (out, true),
            '\\' => {
                let Some(escaped) = chars.next() else {
                    return (out, false);
                };
                match escaped {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let Some(unit) = hex4(chars) else {
                            return (out, false);
                        };
                        if (0xD800..0xDC00).contains(&unit) {
                            // A high surrogate needs the low one after it.
                            if chars.next() != Some('\\') || chars.next() != Some('u') {
                                return (out, false);
                            }
                            let Some(low) = hex4(chars) else {
                                return (out, false);
                            };
                            out.extend(
                                char::decode_utf16([unit, low])
                                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
                            );
                        } else {
                            out.push(
                                char::from_u32(unit.into()).unwrap_or(char::REPLACEMENT_CHARACTER),
                            );
                        }
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    (out, false)
}

fn hex4(chars: &mut Peekable<Chars>) -> Option<u16> {
    let mut unit = 0u16;
    for _ in 0..4 {
        unit = unit * 16 + chars.next()?.to_digit(16)? as u16;
    }
    Some(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> Vec<(String, String, bool)> {
        string_fields(json)
            .into_iter()
            .map(|field| (field.key, field.value, field.complete))
            .collect()
    }

    fn field(key: &str, value: &str, complete: bool) -> (String, String, bool) {
        (key.to_owned(), value.to_owned(), complete)
    }

    #[test]
    fn reads_each_field_as_far_as_it_arrived() {
        assert!(read("").is_empty());
        assert!(read("{\"descr").is_empty());
        assert_eq!(
            read("{\"description\": \"Push"),
            [field("description", "Push", false)]
        );
        assert_eq!(
            read("{\"description\": \"Push both\", \"command\": \"git pu"),
            [
                field("description", "Push both", true),
                field("command", "git pu", false),
            ]
        );
    }

    #[test]
    fn skips_scalars_and_stops_at_nested_values() {
        assert_eq!(
            read("{\"timeout\": 600000, \"run_in_background\": true, \"command\": \"ls\"}"),
            [field("command", "ls", true)]
        );
        assert_eq!(
            read("{\"command\": \"ls\", \"env\": {\"A\": \"b\"}, \"description\": \"x\"}"),
            [field("command", "ls", true)]
        );
    }

    #[test]
    fn decodes_escapes_and_holds_back_a_cut_one() {
        assert_eq!(
            read(r#"{"command": "echo \"a\"\nb\\c \u00e9 \ud83d\ude00"}"#),
            [field("command", "echo \"a\"\nb\\c é 😀", true)]
        );
        assert_eq!(read(r#"{"command": "a\"#), [field("command", "a", false)]);
        assert_eq!(
            read(r#"{"command": "a\u00"#),
            [field("command", "a", false)]
        );
        assert_eq!(
            read(r#"{"command": "a\ud83d"#),
            [field("command", "a", false)]
        );
    }
}
