//! The protocol's compatibility check: `amux.v1` as a release shipped it,
//! against `amux.v1` as the tree has it now.
//!
//! What a released build wrote must still decode as it meant, because builds
//! of different versions meet: paired hosts, agent processes that outlive a
//! daemon upgrade, and the stores and journals an earlier build left on disk.
//! On the binary wire that comes down to numbers and types, so a break is:
//!
//! - a field number gone without being reserved, or a reserved one reused;
//! - a field whose type or cardinality changed, or that moved into, out of
//!   or between oneofs;
//! - an enum value number gone without being reserved, or a reserved one
//!   reused, or a value moved to another number while its old one stands;
//! - a service or method gone, or a method whose request, response or
//!   streaming no longer reads as before, since a method's name is its gRPC
//!   path.
//!
//! Names are otherwise free. A field or method whose type is renamed, or
//! swapped for another message, is compared structurally: the new type must
//! read every number the old one wrote. The exception is a detail message,
//! whose full name travels as an error detail's type and so stays. Deleting
//! a message nothing names any more is free.
//!
//! The baseline is the newest release tag behind HEAD, read from the
//! descriptor set committed with it, so the check needs no build of the old
//! tree. Releases before [`FROZEN_SINCE`] predate the promise and are not
//! checked against; naming a ref checks against it whatever its version.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message as _;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorSet,
    ServiceDescriptorProto,
};

/// The first release whose `amux.v1` later builds must stay compatible with.
pub const FROZEN_SINCE: semver::Version = semver::Version::new(0, 9, 0);

/// The package the promise covers; `amux.dump` and anything else is exempt.
const PACKAGE: &str = "amux.v1";

/// The descriptor set codegen commits beside the generated Rust.
const DESCRIPTORS: &str = "crates/wire/src/generated/amux.v1.bin";

/// `xtask proto-compat [REF]`.
pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let root = repository_root();
    let baseline = match args.first() {
        Some(named) => named.clone(),
        None => {
            let tag = latest_release(&root)?;
            let version = semver::Version::parse(tag.trim_start_matches('v'))?;
            if version < FROZEN_SINCE {
                println!(
                    "the newest release, {tag}, predates protocol v1 (frozen from {FROZEN_SINCE}); nothing to check against"
                );
                return Ok(());
            }
            tag
        }
    };
    let old = git(&root, &["show", &format!("{baseline}:{DESCRIPTORS}")])?;
    let new = std::fs::read(root.join(DESCRIPTORS))?;
    let breaks = breaks(
        &FileDescriptorSet::decode(old.as_slice())?,
        &FileDescriptorSet::decode(new.as_slice())?,
    );
    if breaks.is_empty() {
        println!("{PACKAGE} is wire-compatible with {baseline}");
        return Ok(());
    }
    for found in &breaks {
        eprintln!("{found}");
    }
    Err(format!(
        "{} break(s) against {baseline}; see crates/wire/proto/amux/v1/README.md",
        breaks.len()
    )
    .into())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The newest release tag reachable from HEAD.
fn latest_release(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let out = git(
        root,
        &["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"],
    )
    .map_err(|error| {
        format!("no release tag reachable from HEAD (a shallow clone has none): {error}")
    })?;
    Ok(String::from_utf8(out)?.trim().to_owned())
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(out.stdout)
}

/// Every way `new` fails to read what `old` wrote, one sentence each.
pub fn breaks(old: &FileDescriptorSet, new: &FileDescriptorSet) -> Vec<String> {
    let mut check = Check {
        old: Schema::of(old),
        new: Schema::of(new),
        compared: BTreeSet::new(),
        found: Vec::new(),
    };
    for (name, was) in check.old.messages.clone() {
        if let Some(&now) = check.new.messages.get(&name) {
            check.message(&name, was, now);
        } else if DETAILS.contains(&name.as_str()) {
            check.found.push(format!(
                "detail message {name} is gone; its name travels as an error detail's type"
            ));
        }
    }
    for (name, was) in check.old.enums.clone() {
        if let Some(&now) = check.new.enums.get(&name) {
            check.enumeration(&name, was, now);
        }
    }
    for (name, was) in check.old.services.clone() {
        match check.new.services.get(&name).copied() {
            Some(now) => check.service(&name, was, now),
            None => check.found.push(format!("service {name} is gone")),
        }
    }
    check.found
}

/// The messages whose full name is on the wire, as the type of an error
/// detail, so they keep it.
const DETAILS: &[&str] = &[
    ".amux.v1.AmbiguousAgentName",
    ".amux.v1.AmbiguousHostName",
    ".amux.v1.ProtocolVersionMismatch",
];

/// The package's messages, enums and services by full name, nested ones
/// included.
#[derive(Default)]
struct Schema<'a> {
    messages: BTreeMap<String, &'a DescriptorProto>,
    enums: BTreeMap<String, &'a EnumDescriptorProto>,
    services: BTreeMap<String, &'a ServiceDescriptorProto>,
}

impl<'a> Schema<'a> {
    fn of(set: &'a FileDescriptorSet) -> Schema<'a> {
        fn walk<'a>(prefix: &str, message: &'a DescriptorProto, schema: &mut Schema<'a>) {
            let name = format!("{prefix}.{}", message.name());
            for nested in &message.nested_type {
                walk(&name, nested, schema);
            }
            for enumeration in &message.enum_type {
                schema
                    .enums
                    .insert(format!("{name}.{}", enumeration.name()), enumeration);
            }
            schema.messages.insert(name, message);
        }
        let mut schema = Schema::default();
        for file in set.file.iter().filter(|file| file.package() == PACKAGE) {
            let prefix = format!(".{PACKAGE}");
            for message in &file.message_type {
                walk(&prefix, message, &mut schema);
            }
            for enumeration in &file.enum_type {
                schema
                    .enums
                    .insert(format!("{prefix}.{}", enumeration.name()), enumeration);
            }
            for service in &file.service {
                schema
                    .services
                    .insert(format!("{prefix}.{}", service.name()), service);
            }
        }
        schema
    }
}

struct Check<'a> {
    old: Schema<'a>,
    new: Schema<'a>,
    /// Pairs of differently named types already compared, so a recursive
    /// message is compared once.
    compared: BTreeSet<(String, String)>,
    found: Vec<String>,
}

impl Check<'_> {
    /// Whether a value of type `was`, as the old schema wrote it, reads as
    /// type `now`. The same name is compared where the message itself is; a
    /// different one is compared structurally, by numbers and types.
    fn types(&mut self, at: &str, was: &str, now: &str) {
        if was == now || !self.compared.insert((was.to_owned(), now.to_owned())) {
            return;
        }
        if DETAILS.contains(&was) {
            self.found.push(format!(
                "{at}: the detail message {was} became {now}; its name travels as an error detail's type"
            ));
            return;
        }
        let (old, new) = (&self.old, &self.new);
        if let (Some(&before), Some(&after)) = (old.messages.get(was), new.messages.get(now)) {
            return self.message(&format!("{at}: {was} read as {now}"), before, after);
        }
        if let (Some(&before), Some(&after)) = (old.enums.get(was), new.enums.get(now)) {
            return self.enumeration(&format!("{at}: {was} read as {now}"), before, after);
        }
        // Outside the package (a well-known or vendored type) only the same
        // name is the same type.
        self.found.push(format!("{at} changed from {was} to {now}"));
    }

    fn message(&mut self, name: &str, was: &DescriptorProto, now: &DescriptorProto) {
        // Message reserved ranges are end-exclusive.
        let reserved = |number: i32| {
            now.reserved_range
                .iter()
                .any(|range| (range.start()..range.end()).contains(&number))
        };
        let fields: BTreeMap<i32, &FieldDescriptorProto> = now
            .field
            .iter()
            .map(|field| (field.number(), field))
            .collect();
        for old in &was.field {
            let number = old.number();
            let at = format!("{name} field {number} ({})", old.name());
            let Some(new) = fields.get(&number) else {
                if !reserved(number) {
                    self.found
                        .push(format!("{at} is gone without its number reserved"));
                }
                continue;
            };
            if shape(old) != shape(new) {
                self.found.push(format!(
                    "{at} changed from {} to {}",
                    shape(old),
                    shape(new)
                ));
            } else if !old.type_name().is_empty() {
                self.types(&at, old.type_name(), new.type_name());
            }
            if oneof(was, old).is_some() != oneof(now, new).is_some() {
                self.found
                    .push(format!("{at} moved into or out of a oneof"));
            }
        }
        // Fields that shared a oneof still share one, and fields from
        // different oneofs never come to share one.
        let mut joined: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for old in &was.field {
            if let (Some(before), Some(after)) = (
                oneof(was, old),
                fields.get(&old.number()).and_then(|new| oneof(now, new)),
            ) {
                joined.entry(after).or_default().insert(before);
            }
        }
        let mut seen: BTreeMap<i32, i32> = BTreeMap::new();
        for (after, befores) in &joined {
            if befores.len() > 1 {
                self.found.push(format!(
                    "{name}: fields of different oneofs now share the oneof {}",
                    now.oneof_decl[*after as usize].name()
                ));
            }
            for before in befores {
                if seen
                    .insert(*before, *after)
                    .is_some_and(|other| other != *after)
                {
                    self.found.push(format!(
                        "{name}: the oneof {} was split",
                        was.oneof_decl[*before as usize].name()
                    ));
                }
            }
        }
        for range in &was.reserved_range {
            for new in now
                .field
                .iter()
                .filter(|field| (range.start()..range.end()).contains(&field.number()))
            {
                self.found.push(format!(
                    "{name} field {} ({}) reuses a reserved number",
                    new.number(),
                    new.name()
                ));
            }
        }
    }

    fn enumeration(&mut self, name: &str, was: &EnumDescriptorProto, now: &EnumDescriptorProto) {
        // Enum reserved ranges are end-inclusive.
        let within = |range: &prost_types::enum_descriptor_proto::EnumReservedRange,
                      number: i32| {
            (range.start()..=range.end()).contains(&number)
        };
        let numbers: BTreeSet<i32> = now.value.iter().map(|value| value.number()).collect();
        for old in &was.value {
            let number = old.number();
            // A name that moved while its old number still stands means that
            // number now says something else.
            if let Some(moved) = now
                .value
                .iter()
                .find(|new| new.name() == old.name() && new.number() != number)
                && numbers.contains(&number)
            {
                self.found.push(format!(
                    "{name} value {} moved from {number} to {}, and {number} now means another value",
                    old.name(),
                    moved.number()
                ));
            }
            if !numbers.contains(&number)
                && !now.reserved_range.iter().any(|range| within(range, number))
            {
                self.found.push(format!(
                    "{name} value {number} ({}) is gone without its number reserved",
                    old.name()
                ));
            }
        }
        for range in &was.reserved_range {
            for new in now
                .value
                .iter()
                .filter(|value| within(range, value.number()))
            {
                self.found.push(format!(
                    "{name} value {} ({}) reuses a reserved number",
                    new.number(),
                    new.name()
                ));
            }
        }
    }

    fn service(&mut self, name: &str, was: &ServiceDescriptorProto, now: &ServiceDescriptorProto) {
        let streams = |method: &prost_types::MethodDescriptorProto| {
            (method.client_streaming(), method.server_streaming())
        };
        for old in &was.method {
            let at = format!("{name}.{}", old.name());
            let Some(new) = now.method.iter().find(|new| new.name() == old.name()) else {
                self.found.push(format!("{at} is gone"));
                continue;
            };
            if streams(old) != streams(new) {
                self.found.push(format!("{at} changed whether it streams"));
            }
            self.types(&format!("{at} request"), old.input_type(), new.input_type());
            self.types(
                &format!("{at} response"),
                old.output_type(),
                new.output_type(),
            );
        }
    }
}

/// What a field's bytes are: its cardinality and kind, a message or enum by
/// kind alone, since its type is compared apart.
fn shape(field: &FieldDescriptorProto) -> String {
    let repeated = if field.label() == Label::Repeated {
        "repeated "
    } else {
        ""
    };
    let kind = match field.r#type() {
        Type::Message => "a message".to_owned(),
        Type::Enum => "an enum".to_owned(),
        other => other
            .as_str_name()
            .trim_start_matches("TYPE_")
            .to_lowercase(),
    };
    format!("{repeated}{kind}")
}

/// The real oneof a field belongs to; proto3 `optional` is a synthetic
/// oneof that changes nothing on the wire.
fn oneof(message: &DescriptorProto, field: &FieldDescriptorProto) -> Option<i32> {
    field
        .oneof_index
        .filter(|_| !field.proto3_optional())
        .filter(|index| (*index as usize) < message.oneof_decl.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `amux.v1` file compiled by the vendored protoc.
    fn schema(source: &str) -> FileDescriptorSet {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.proto");
        std::fs::write(
            &file,
            format!("syntax = \"proto3\";\npackage {PACKAGE};\n{source}"),
        )
        .unwrap();
        let out = dir.path().join("t.bin");
        let status = Command::new(protoc_bin_vendored::protoc_bin_path().unwrap())
            .arg("-I")
            .arg(dir.path())
            .arg(format!("--descriptor_set_out={}", out.display()))
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success(), "protoc failed on {source}");
        FileDescriptorSet::decode(std::fs::read(out).unwrap().as_slice()).unwrap()
    }

    fn check(old: &str, new: &str) -> Vec<String> {
        breaks(&schema(old), &schema(new))
    }

    const BASE: &str = r#"
        message Thing {
          bytes id = 1;
          string name = 2;
          optional uint32 count = 3;
          oneof of { string text = 4; bytes blob = 5; }
          Kind kind = 6;
        }
        enum Kind { KIND_UNSPECIFIED = 0; KIND_A = 1; KIND_B = 2; }
        service Things {
          rpc GetThing(Thing) returns (Thing);
          rpc Watch(Thing) returns (stream Thing);
        }
    "#;

    #[test]
    fn additions_and_renames_are_compatible() {
        let grown = r#"
            message Thing {
              bytes key = 1;
              string label = 2;
              optional uint32 count = 3;
              oneof body { string text = 4; bytes blob = 5; Other other = 8; }
              Kind kind = 6;
              repeated string tags = 7;
            }
            message Other {}
            enum Kind { KIND_UNKNOWN = 0; KIND_FIRST = 1; KIND_B = 2; KIND_C = 3; }
            service Things {
              rpc GetThing(Thing) returns (Thing);
              rpc Watch(Thing) returns (stream Thing);
              rpc Forget(Thing) returns (Other);
            }
            service More { rpc Hello(Other) returns (Other); }
        "#;
        assert_eq!(check(BASE, BASE), Vec::<String>::new());
        assert_eq!(check(BASE, grown), Vec::<String>::new());
    }

    #[test]
    fn a_field_goes_only_by_reserving_its_number() {
        let dropped = BASE.replace("string name = 2;", "");
        assert_eq!(
            check(BASE, &dropped),
            [".amux.v1.Thing field 2 (name) is gone without its number reserved"]
        );
        let reserved = BASE.replace("string name = 2;", "reserved 2;");
        assert_eq!(check(BASE, &reserved), Vec::<String>::new());
        let reused = reserved.replace("reserved 2;", "bytes name = 2;");
        assert_eq!(
            check(&reserved, &reused),
            [".amux.v1.Thing field 2 (name) reuses a reserved number"]
        );
    }

    #[test]
    fn renumbering_retyping_and_cardinality_break() {
        let renumbered = BASE.replace("string name = 2;", "string name = 9;");
        assert_eq!(check(BASE, &renumbered).len(), 1);
        let retyped = BASE.replace("string name = 2;", "bytes name = 2;");
        assert_eq!(
            check(BASE, &retyped),
            [".amux.v1.Thing field 2 (name) changed from string to bytes"]
        );
        let repeated = BASE.replace("string name = 2;", "repeated string name = 2;");
        assert_eq!(check(BASE, &repeated).len(), 1);
        let widened = BASE.replace("optional uint32 count = 3;", "optional uint64 count = 3;");
        assert_eq!(check(BASE, &widened).len(), 1);
    }

    #[test]
    fn proto3_optional_is_not_a_oneof() {
        let plain = BASE.replace("optional uint32 count = 3;", "uint32 count = 3;");
        assert_eq!(check(BASE, &plain), Vec::<String>::new());
        assert_eq!(check(&plain, BASE), Vec::<String>::new());
    }

    #[test]
    fn oneof_membership_holds() {
        let out = BASE.replace(
            "oneof of { string text = 4; bytes blob = 5; }",
            "string text = 4; oneof of { bytes blob = 5; }",
        );
        assert_eq!(
            check(BASE, &out),
            [".amux.v1.Thing field 4 (text) moved into or out of a oneof"]
        );
        let split = BASE.replace(
            "oneof of { string text = 4; bytes blob = 5; }",
            "oneof of { string text = 4; } oneof other { bytes blob = 5; }",
        );
        assert_eq!(
            check(BASE, &split),
            [".amux.v1.Thing: the oneof of was split"]
        );
    }

    #[test]
    fn a_type_may_be_renamed_or_swapped_for_one_that_reads_the_same() {
        let renamed = BASE.replace("Kind kind = 6;", "Sort kind = 6;").replace(
            "enum Kind { KIND_UNSPECIFIED = 0; KIND_A = 1; KIND_B = 2; }",
            "enum Sort { SORT_UNSPECIFIED = 0; SORT_A = 1; SORT_B = 2; }",
        );
        assert_eq!(check(BASE, &renamed), Vec::<String>::new());
        let emptied = "message Empty {} service S { rpc Go(Empty) returns (Empty); }";
        let grown = "message Empty {} message GoRequest { string why = 1; } \
                     service S { rpc Go(GoRequest) returns (Empty); }";
        assert_eq!(check(emptied, grown), Vec::<String>::new());
        let shrunk = renamed.replace(" SORT_B = 2;", "");
        assert_eq!(
            check(BASE, &shrunk),
            [
                ".amux.v1.Thing field 6 (kind): .amux.v1.Kind read as .amux.v1.Sort value 2 (KIND_B) is gone without its number reserved"
            ]
        );
        let wrapped = "message Inner { string text = 1; } message Outer { Inner inner = 1; } \
                       service S { rpc Go(Inner) returns (Inner); }";
        let unwrapped = "message Inner { string text = 1; } message Outer { Inner inner = 1; } \
                         service S { rpc Go(Outer) returns (Inner); }";
        assert_eq!(
            check(wrapped, unwrapped),
            [
                ".amux.v1.S.Go request: .amux.v1.Inner read as .amux.v1.Outer field 1 (text) changed from string to a message"
            ]
        );
    }

    #[test]
    fn a_detail_message_keeps_its_name() {
        let detail = "message AmbiguousAgentName { string name = 1; } \
                      message Error { AmbiguousAgentName detail = 1; }";
        let renamed = detail.replace("AmbiguousAgentName", "AgentNameAmbiguous");
        let found = check(detail, &renamed);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|line| line.contains("travels as an error detail's type"))
        );
    }

    #[test]
    fn an_enum_value_goes_only_by_reserving_its_number() {
        let dropped = BASE.replace(" KIND_B = 2;", "");
        assert_eq!(
            check(BASE, &dropped),
            [".amux.v1.Kind value 2 (KIND_B) is gone without its number reserved"]
        );
        let reserved = BASE.replace(" KIND_B = 2;", " reserved 2;");
        assert_eq!(check(BASE, &reserved), Vec::<String>::new());
        let shifted = BASE.replace(
            "KIND_UNSPECIFIED = 0; KIND_A = 1; KIND_B = 2;",
            "KIND_UNSPECIFIED = 0; KIND_NEW = 1; KIND_A = 2; KIND_B = 3;",
        );
        assert_eq!(
            check(BASE, &shifted),
            [
                ".amux.v1.Kind value KIND_A moved from 1 to 2, and 1 now means another value",
                ".amux.v1.Kind value KIND_B moved from 2 to 3, and 2 now means another value",
            ]
        );
        let reused = reserved.replace(" reserved 2;", " KIND_C = 2;");
        assert_eq!(
            check(&reserved, &reused),
            [".amux.v1.Kind value 2 (KIND_C) reuses a reserved number"]
        );
    }

    #[test]
    fn methods_keep_their_names_and_signatures() {
        let renamed = BASE.replace("rpc GetThing(", "rpc Fetch(");
        assert_eq!(check(BASE, &renamed), [".amux.v1.Things.GetThing is gone"]);
        let unstreamed = BASE.replace("returns (stream Thing)", "returns (Thing)");
        assert_eq!(check(BASE, &unstreamed).len(), 1);
        let gone = BASE.replace("service Things", "service Stuff");
        assert_eq!(check(BASE, &gone), ["service .amux.v1.Things is gone"]);
    }

    #[test]
    fn other_packages_are_exempt() {
        let mut moved = schema(BASE);
        for file in &mut moved.file {
            file.package = Some("amux.dump".to_owned());
        }
        assert_eq!(breaks(&moved, &schema("")), Vec::<String>::new());
    }
}
