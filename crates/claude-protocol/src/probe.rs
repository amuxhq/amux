//! Which field names this crate types, at each place in a line.
//!
//! A probe marks every object of a line with its place and decodes it as
//! production does. Every object this crate types keeps what it does not
//! know in its [`Extensions`](crate::stream::Extensions), which takes the
//! mark too: so the places whose marks are taken are the objects this
//! crate types, and the names their extensions hold beside the mark are
//! the ones it does not. A place whose mark nobody takes is data (a tool's
//! input, a schema) or an object this crate does not know, and types none
//! of its names.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// Each place in a line holding an object this crate types (`""` the
/// line itself, `a.b` a field's, `a[2]` an element's), with the names it
/// holds there that this crate does not type.
pub type Fields = BTreeMap<String, BTreeSet<String>>;

/// The key a probe marks an object with, holding its place.
const MARK: &str = "\u{1}at";

thread_local! {
    static HEARD: RefCell<Option<Fields>> = const { RefCell::new(None) };
}

/// Notes the object an extension map was taken from, during a probe.
pub(crate) fn heard(extensions: &mut Map<String, Value>) {
    let Some(Value::String(at)) = extensions.remove(MARK) else {
        return;
    };
    note(at, extensions.keys().cloned().collect());
}

/// Notes an object at `at` this crate reads by the names it does not hold
/// in `untyped`, during a probe.
pub(crate) fn heard_at(at: &str, untyped: impl IntoIterator<Item = String>) {
    note(
        at.to_owned(),
        untyped.into_iter().filter(|name| name != MARK).collect(),
    );
}

/// Notes the names an object at `at` holds that a type reading it does
/// not type. An object read more than once (by each variant of an untagged
/// enum a decoder tries, or each type a response may be) keeps only the
/// names none of them types.
fn note(at: String, untyped: BTreeSet<String>) {
    HEARD.with(|heard| {
        if let Some(heard) = heard.borrow_mut().as_mut() {
            match heard.get_mut(&at) {
                Some(noted) => noted.retain(|name| untyped.contains(name)),
                None => {
                    heard.insert(at, untyped);
                }
            }
        }
    });
}

/// Probes `line` through `decode`, the decoder of its kind of line,
/// leaving unmarked the subtrees in `data` (by path, `[]` an element) and
/// the maps in `maps`, whose keys are the data's and whose values are
/// typed. None for a line that is not a JSON object.
pub(crate) fn probe(
    line: &[u8],
    data: &[&str],
    maps: &[&str],
    decode: impl FnOnce(&[u8]),
) -> Option<Fields> {
    let mut value = serde_json::from_slice::<Value>(line).ok()?;
    if !value.is_object() {
        return None;
    }
    mark(&mut value, "", "", data, maps);
    let marked = serde_json::to_vec(&value).ok()?;
    struct Restore(Option<Fields>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HEARD.with(|heard| *heard.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(HEARD.with(|heard| heard.borrow_mut().replace(Fields::new())));
    decode(&marked);
    let fields = HEARD
        .with(|heard| heard.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    Some(fields)
}

fn mark(value: &mut Value, at: &str, path: &str, data: &[&str], maps: &[&str]) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                let path = join(path, key);
                if !data.contains(&path.as_str()) {
                    mark(field, &join(at, key), &path, data, maps);
                }
            }
            if !is_map(maps, path) {
                fields.insert(MARK.to_owned(), Value::String(at.to_owned()));
            }
        }
        Value::Array(items) => {
            let path = format!("{path}[]");
            for (index, item) in items.iter_mut().enumerate() {
                mark(item, &format!("{at}[{index}]"), &path, data, maps);
            }
        }
        _ => {}
    }
}

/// Whether `path` is one of `maps`: a path, or `*.<name>` any field
/// called `name`.
pub fn is_map(maps: &[&str], path: &str) -> bool {
    maps.iter().any(|map| match map.strip_prefix("*.") {
        Some(name) => path == name || path.ends_with(&format!(".{name}")),
        None => *map == path,
    })
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}
