//! Every message this crate types, held to ACP's own JSON schema.
//!
//! `schema/` holds the schema of ACP version 1 as its release 1.33.0
//! published it: the stable surface, the unstable superset, and the method
//! tables of each. Nothing here is written by hand from the schema: each
//! message is generated from it, once with every field, once with only the
//! required ones and once with every nullable field null, over every member
//! of every union, and read strictly (which refuses a field, tag or
//! spelling the crate does not type), then written back and compared. A
//! field typed under the wrong name, of the wrong type, required where the
//! schema lets it be absent or null fails; so does a required field the
//! crate reads without, and a method, tag or spelling the crate types that
//! the schema does not have.
//!
//! What the crate leaves out on purpose is listed below with why, and only
//! the unstable schema may hold it: the stable surface is typed whole. An
//! entry that no longer matches anything fails too.

use std::collections::BTreeSet;

use acp_protocol::agent::{AuthState, PermissionOptionKind, StopReason};
use acp_protocol::client::{ConfigValueKind, RequestPermissionOutcome};
use acp_protocol::content::{ContentBlock, Role};
use acp_protocol::elicitation::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationPropertySchema,
    ElicitationSchemaType, MultiSelectItems, StringFormat,
};
use acp_protocol::session::{
    AuthMethod, McpServer, SessionConfigOption, SessionConfigOptionCategory, SystemPromptMode,
};
use acp_protocol::update::{
    CompactionStatus, NoticeSeverity, PlanEntryPriority, PlanEntryStatus, SessionUpdate,
    ToolCallContent, ToolCallStatus, ToolKind,
};
use acp_protocol::{
    AgentMessage, AgentNotification, AgentRequest, AgentResponse, ClientMessage,
    ClientNotification, ClientRequest, ClientResponse,
};
use serde_json::{Map, Value, json};

const STABLE: &str = include_str!("../schema/v1-schema.json");
const UNSTABLE: &str = include_str!("../schema/v1-schema.unstable.json");
const STABLE_METHODS: &str = include_str!("../schema/v1-meta.json");

const PROVIDERS: &str = "amux does not choose the model provider an agent calls";
const ARCHIVE: &str = "amux keeps its own list of conversations";
const OWN_SERVER: &str = "amux gives its tool server as a stdio command, not over ACP";
const SUBAGENTS: &str = "agents' own subagents are a draft amux does not show";
const CURSORS: &str = "amux does not fork sessions at a cursor";
const SUGGESTIONS: &str = "amux does not offer prompt suggestions";
const INITIAL_OPTIONS: &str = "amux sets options once a session has started, as every agent can";
const PLAN_ITEMS: &str = "amux shows the stable plan update";

/// Unstable methods left untyped, which decode as unknown.
const UNTYPED_METHODS: &[(&str, &str)] = &[
    ("providers/list", PROVIDERS),
    ("providers/set", PROVIDERS),
    ("providers/disable", PROVIDERS),
    ("session/archive", ARCHIVE),
    ("session/unarchive", ARCHIVE),
    (
        "session/status",
        "amux knows a session's state from its own turns",
    ),
    (
        "session/set_title",
        "amux names its agents itself and reads the agent's title from session_info_update",
    ),
    ("mcp/message", OWN_SERVER),
];

/// Unstable fields of typed objects left untyped: a strict decode refuses
/// them, and a tolerant one keeps them in `extra`.
const UNTYPED_FIELDS: &[(&str, &str, &str)] = &[
    ("AgentCapabilities", "providers", PROVIDERS),
    (
        "ClientCapabilities",
        "plan",
        "amux does not offer the unstable plan updates",
    ),
    ("ClientCapabilities", "subagents", SUBAGENTS),
    (
        "ClientSessionCapabilities",
        "promptSuggestions",
        SUGGESTIONS,
    ),
    ("ClientSessionCapabilities", "cursors", CURSORS),
    ("McpCapabilities", "acp", OWN_SERVER),
    ("ListSessionsRequest", "archived", ARCHIVE),
    ("SessionInfo", "archived", ARCHIVE),
    ("NewSessionRequest", "configOptions", INITIAL_OPTIONS),
    ("LoadSessionRequest", "configOptions", INITIAL_OPTIONS),
    ("ResumeSessionRequest", "configOptions", INITIAL_OPTIONS),
    ("ForkSessionRequest", "configOptions", INITIAL_OPTIONS),
    ("ForkSessionRequest", "cursorId", CURSORS),
];

/// Unstable members of unions left untyped, by the union and the value of
/// its tag: they decode as the union's `Unknown`.
const UNTYPED_MEMBERS: &[(&str, &str, &str)] = &[
    ("SessionUpdate", "plan_update", PLAN_ITEMS),
    ("SessionUpdate", "plan_removed", PLAN_ITEMS),
    ("SessionUpdate", "prompt_suggestions_update", SUGGESTIONS),
    ("SessionUpdate", "cursor_update", CURSORS),
    ("SessionUpdate", "subagent_update", SUBAGENTS),
    ("SessionUpdate", "session_message", SUBAGENTS),
    ("SessionUpdate", "session_message_chunk", SUBAGENTS),
    ("ToolCallContent", "session", SUBAGENTS),
    ("McpServer", "acp", OWN_SERVER),
];

/// Required fields the crate reads without, with why.
const LENIENT_REQUIRED: &[(&str, &str, &str)] = &[
    (
        "ElicitationSessionScope",
        "sessionId",
        "an elicitation's scope is typed as optional fields beside its mode",
    ),
    (
        "ElicitationRequestScope",
        "requestId",
        "an elicitation's scope is typed as optional fields beside its mode",
    ),
    (
        "SetSessionConfigOptionRequest",
        "type",
        "a boolean value says it is one by its JSON type",
    ),
];

struct Schema {
    defs: Map<String, Value>,
}

impl Schema {
    fn parse(text: &str) -> Self {
        let schema: Value = serde_json::from_str(text).expect("the schema is JSON");
        let defs = schema["$defs"].as_object().expect("$defs").clone();
        Self { defs }
    }

    fn def(&self, name: &str) -> &Value {
        self.defs
            .get(name)
            .unwrap_or_else(|| panic!("the schema has no {name}"))
    }

    /// The defs an envelope's `field` (`params`, `result`) may hold.
    fn members(&self, envelope: &str, field: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut nodes = vec![self.def(envelope)];
        while let Some(node) = nodes.pop() {
            if let Some(held) = node.get("properties").and_then(|p| p.get(field)) {
                collect_refs(held, &mut found);
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(Value::Array(branches)) = node.get(key) {
                    nodes.extend(branches);
                }
            }
        }
        // ACP's extension messages, which are any JSON at all.
        found.retain(|name| !name.starts_with("Ext"));
        found.sort();
        found.dedup();
        found
    }

    /// The method a def is the params or result of.
    fn method(&self, def: &str) -> String {
        self.def(def)["x-method"]
            .as_str()
            .unwrap_or_else(|| panic!("{def} names no method"))
            .to_owned()
    }

    /// The values of `field` that tell a union's members apart.
    fn tags(&self, def: &str, field: &str) -> BTreeSet<String> {
        let Some(def) = self.defs.get(def) else {
            return BTreeSet::new();
        };
        branches(def)
            .iter()
            .filter_map(|branch| branch["properties"][field]["const"].as_str())
            .map(str::to_owned)
            .collect()
    }

    /// The spellings a string enum lists.
    fn spellings(&self, def: &str) -> BTreeSet<String> {
        let node = self.def(def);
        let mut found: BTreeSet<String> = branches(node)
            .iter()
            .filter_map(|branch| branch["const"].as_str())
            .map(str::to_owned)
            .collect();
        if let Some(Value::Array(values)) = node.get("enum") {
            found.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        found
    }
}

fn collect_refs(node: &Value, found: &mut Vec<String>) {
    match node {
        Value::Object(object) => {
            if let Some(Value::String(target)) = object.get("$ref") {
                found.push(target.rsplit('/').next().unwrap().to_owned());
            }
            for value in object.values() {
                collect_refs(value, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, found)),
        _ => {}
    }
}

fn branches(node: &Value) -> Vec<&Value> {
    ["anyOf", "oneOf"]
        .iter()
        .filter_map(|key| node.get(*key).and_then(Value::as_array))
        .flatten()
        .collect()
}

/// A union member that only catches what the others do not: ACP's
/// "other" spellings and shapes, which the crate keeps as unknown.
fn catch_all(branch: &Value) -> bool {
    branch.get("not").is_some() || branch["title"] == "other"
}

fn types(node: &Value) -> Vec<&str> {
    match node.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn nullable(node: &Value) -> bool {
    types(node).contains(&"null")
        || branches(node)
            .iter()
            .any(|branch| types(branch) == ["null"])
}

#[derive(Clone, Copy, PartialEq)]
enum Fill {
    /// Every field.
    All,
    /// Only the required fields.
    Required,
    /// Every field, with each optional field that may be null null.
    Nulls,
}

/// A required field of one generated object: where the object is (a JSON
/// pointer), the def it is a field of, and its name.
type Required = (String, String, String);

/// Which allowances some generated value needed, so a stale one fails.
#[derive(Default)]
struct Used {
    fields: BTreeSet<(String, String)>,
    members: BTreeSet<(String, String)>,
}

/// Generates values from the schema. `choice` picks the member of each
/// union, so generating for each choice up to the widest union's size
/// covers every member of every union.
struct Generator<'a> {
    schema: &'a Schema,
    choice: usize,
    fill: Fill,
    required: Vec<Required>,
    used: &'a mut Used,
}

impl Generator<'_> {
    fn value(&mut self, node: &Value, def: &str, at: &str, depth: usize) -> Value {
        assert!(depth < 32, "{def} at {at}: the schema nests too deep");
        if let Some(Value::String(target)) = node.get("$ref") {
            let name = target.rsplit('/').next().unwrap();
            return self.value(self.schema.def(name), name, at, depth + 1);
        }
        if let Some(value) = node.get("const") {
            return value.clone();
        }
        if let Some(Value::Array(values)) = node.get("enum") {
            return values[self.choice % values.len()].clone();
        }
        let kinds = types(node);
        let object = kinds.contains(&"object") || node.get("properties").is_some();
        let members = self.members(node, def);
        if !object {
            if let Some(Value::Array(parts)) = node.get("allOf")
                && let [part] = parts.as_slice()
            {
                return self.value(part, def, at, depth + 1);
            }
            if !members.is_empty() {
                let member = members[self.choice % members.len()];
                return self.value(member, def, at, depth + 1);
            }
        }
        if object || node.get("allOf").is_some() {
            return Value::Object(self.object(node, def, at, depth, &members));
        }
        match kinds.iter().find(|kind| **kind != "null") {
            Some(&"string") => Value::from("text"),
            Some(&"integer") => Value::from(1),
            Some(&"number") => json!(1.5),
            Some(&"boolean") => Value::from(true),
            Some(&"array") => {
                let item = self.value(&node["items"], def, &format!("{at}/0"), depth + 1);
                Value::Array(vec![item])
            }
            Some(other) => panic!("{def} at {at}: type {other}"),
            None if kinds.contains(&"null") => Value::Null,
            // Any JSON at all: a tool's raw input, an error's data.
            None => json!({ "any": [1, "two"] }),
        }
    }

    /// The members of a union a value may be generated as.
    fn members<'n>(&mut self, node: &'n Value, def: &str) -> Vec<&'n Value> {
        let mut members = Vec::new();
        for branch in branches(node) {
            if types(branch) == ["null"] || catch_all(branch) {
                continue;
            }
            let tag = branch["properties"]
                .as_object()
                .into_iter()
                .flat_map(|properties| properties.values())
                .find_map(|property| property["const"].as_str());
            let untyped = tag.is_some_and(|tag| {
                UNTYPED_MEMBERS
                    .iter()
                    .any(|(union, member, _)| *union == def && *member == tag)
            });
            if untyped {
                self.used
                    .members
                    .insert((def.to_owned(), tag.unwrap_or("").to_owned()));
            } else {
                members.push(branch);
            }
        }
        members
    }

    fn object(
        &mut self,
        node: &Value,
        def: &str,
        at: &str,
        depth: usize,
        members: &[&Value],
    ) -> Map<String, Value> {
        let mut object = Map::new();
        let required: BTreeSet<&str> = node["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if let Some(Value::Object(properties)) = node.get("properties") {
            for (name, property) in properties {
                if UNTYPED_FIELDS
                    .iter()
                    .any(|(object, field, _)| *object == def && field == name)
                {
                    self.used.fields.insert((def.to_owned(), name.clone()));
                    continue;
                }
                let needed = required.contains(name.as_str());
                if needed {
                    self.required
                        .push((at.to_owned(), def.to_owned(), name.clone()));
                } else if self.fill == Fill::Required {
                    continue;
                }
                let value = if self.fill == Fill::Nulls && !needed && nullable(property) {
                    Value::Null
                } else {
                    self.value(property, def, &format!("{at}/{name}"), depth + 1)
                };
                object.insert(name.clone(), value);
            }
        } else if let Some(Value::Object(values)) = node.get("additionalProperties") {
            let values = Value::Object(values.clone());
            let value = self.value(&values, def, &format!("{at}/key"), depth + 1);
            object.insert("key".into(), value);
        } else if node.get("additionalProperties") == Some(&Value::Bool(true)) {
            // A vendor's `_meta`.
            object.insert("vendor".into(), json!({ "setting": [1] }));
        }
        let parts = node["allOf"].as_array().into_iter().flatten();
        let member = (!members.is_empty()).then(|| members[self.choice % members.len()]);
        for part in parts.chain(member) {
            match self.value(part, def, at, depth + 1) {
                Value::Object(fields) => object.extend(fields),
                other => panic!("{def} at {at}: a part of an object is {other}"),
            }
        }
        object
    }
}

/// One generated message: what it is, its JSON, and its required fields.
struct Sample {
    name: String,
    fill: Fill,
    value: Value,
    required: Vec<Required>,
}

/// Every distinct sample of `def`, for each choice of union members and
/// each fill.
fn samples(schema: &Schema, def: &str, used: &mut Used) -> Vec<Sample> {
    let widest = schema
        .defs
        .values()
        .map(|node| {
            let spellings = node["enum"].as_array().map_or(0, Vec::len);
            branches(node).len().max(spellings)
        })
        .max()
        .unwrap_or(1);
    let mut samples = Vec::new();
    for fill in [Fill::All, Fill::Required, Fill::Nulls] {
        let mut seen = BTreeSet::new();
        for choice in 0..widest {
            let mut generator = Generator {
                schema,
                choice,
                fill,
                required: Vec::new(),
                used,
            };
            let value = generator.value(schema.def(def), def, "", 0);
            let required = generator.required;
            if seen.insert(value.to_string()) {
                samples.push(Sample {
                    name: format!("{def} (choice {choice})"),
                    fill,
                    value,
                    required,
                });
            }
        }
    }
    samples
}

#[derive(Clone, Copy)]
enum Direction {
    /// What the client sends: read by `strict_client`, written by `encode`.
    Client,
    /// What the agent sends: read by `strict`, written by `encode_server`.
    Agent,
}

/// Reads `line` strictly and writes it back, as the direction's decoder
/// and encoder do.
fn reread(direction: Direction, line: &Value) -> Result<Value, String> {
    let bytes = serde_json::to_vec(line).unwrap();
    let encoded = match direction {
        Direction::Client => match acp_protocol::strict_client(&bytes) {
            Ok(ClientMessage::Unknown(_) | ClientMessage::Extension { .. }) => {
                return Err("not typed".into());
            }
            Ok(message) => acp_protocol::encode(&message),
            Err(drift) => return Err(drift.to_string()),
        },
        Direction::Agent => match acp_protocol::strict(&bytes) {
            Ok(AgentMessage::Unknown(_) | AgentMessage::Extension { .. }) => {
                return Err("not typed".into());
            }
            Ok(message) => acp_protocol::encode_server(&message),
            Err(drift) => return Err(drift.to_string()),
        },
    };
    Ok(serde_json::from_slice(&encoded).unwrap())
}

/// Checks one sample, read by `read`: it reads, writes back the same
/// (unless it holds nulls, which the crate does not keep apart from
/// absence), and stops reading when any required field is taken out.
fn check(
    sample: &Sample,
    read: &dyn Fn(&Value) -> Result<Value, String>,
    failures: &mut Vec<String>,
) {
    let Sample {
        name, fill, value, ..
    } = sample;
    match read(value) {
        Err(error) => failures.push(format!("{name}: {error}\n    {value}")),
        Ok(again) if *fill != Fill::Nulls && again != *value => {
            failures.push(format!("{name}: wrote {again}\n    read {value}"))
        }
        Ok(_) => {}
    }
    // Taken out of a sample with only the required fields, a union's tag
    // can leave another member's whole shape (a terminal sign-in without
    // its type is the agent's own), so only the full samples are checked.
    if *fill != Fill::All {
        return;
    }
    for (at, def, field) in &sample.required {
        let lenient = LENIENT_REQUIRED
            .iter()
            .any(|(object, name, _)| object == def && name == field);
        let mut without = value.clone();
        let Some(Value::Object(object)) = without.pointer_mut(at) else {
            panic!("{name}: nothing at {at}");
        };
        object.remove(field);
        match (read(&without), lenient) {
            (Ok(_), false) => failures.push(format!(
                "{name}: reads without {def}.{field}, which the schema requires"
            )),
            (Err(_), true) => failures.push(format!(
                "{name}: {def}.{field} is listed as read without, but is refused"
            )),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Checked {
    failures: Vec<String>,
    used: Used,
    untyped_methods: BTreeSet<String>,
    lenient: BTreeSet<(String, String)>,
    samples: usize,
}

impl Checked {
    fn all(&mut self, schema: &Schema, def: &str, read: &dyn Fn(&Value) -> Result<Value, String>) {
        for sample in samples(schema, def, &mut self.used) {
            self.samples += 1;
            for (_, object, field) in &sample.required {
                self.lenient.insert((object.clone(), field.clone()));
            }
            check(&sample, read, &mut self.failures);
        }
    }
}

fn untyped(method: &str) -> bool {
    UNTYPED_METHODS.iter().any(|(name, _)| *name == method)
}

fn check_all() -> Checked {
    let schema = Schema::parse(UNSTABLE);
    let mut checked = Checked::default();
    let messages: [(&str, Direction, bool, &[&str]); 4] = [
        (
            "ClientRequest",
            Direction::Client,
            true,
            ClientRequest::METHODS,
        ),
        (
            "ClientNotification",
            Direction::Client,
            false,
            ClientNotification::METHODS,
        ),
        (
            "AgentRequest",
            Direction::Agent,
            true,
            AgentRequest::METHODS,
        ),
        (
            "AgentNotification",
            Direction::Agent,
            false,
            AgentNotification::METHODS,
        ),
    ];
    for (envelope, direction, request, typed) in messages {
        let mut methods = BTreeSet::new();
        let mut defs = schema.members(envelope, "params");
        // The protocol's own notification, which either side sends.
        if !request {
            defs.push("CancelRequestNotification".into());
        }
        for def in defs {
            let method = schema.method(&def);
            methods.insert(method.clone());
            let known = typed.contains(&method.as_str());
            if untyped(&method) {
                checked.untyped_methods.insert(method.clone());
                if known {
                    checked
                        .failures
                        .push(format!("{method} is typed but listed as untyped"));
                }
                continue;
            }
            if !known {
                checked
                    .failures
                    .push(format!("{envelope}: {method} ({def}) is not typed"));
                continue;
            }
            let read = |params: &Value| {
                let mut line = json!({ "jsonrpc": "2.0", "method": method, "params": params });
                if request {
                    line["id"] = Value::from(1);
                }
                reread(direction, &line).map(|again| again["params"].clone())
            };
            checked.all(&schema, &def, &read);
        }
        for method in typed {
            if !methods.contains(*method) {
                checked.failures.push(format!(
                    "{envelope}: the crate types {method}, which the schema does not have"
                ));
            }
        }
    }

    // Answers, read by the method of the request they answer.
    for def in schema.members("AgentResponse", "result") {
        let method = schema.method(&def);
        if untyped(&method) {
            continue;
        }
        let read = |result: &Value| match AgentResponse::strict_read(&method, result) {
            None => Err(format!("{method} has no answer type")),
            Some(Err(error)) => Err(error.to_string()),
            Some(Ok(answer)) => Ok(serde_json::to_value(&answer).unwrap()),
        };
        checked.all(&schema, &def, &read);
    }
    for def in schema.members("ClientResponse", "result") {
        let method = schema.method(&def);
        if untyped(&method) {
            continue;
        }
        // As the answer to its own request, and as a line read without it.
        let read = |result: &Value| match ClientResponse::strict_read(&method, result) {
            None => Err(format!("{method} has no answer type")),
            Some(Err(error)) => Err(error.to_string()),
            Some(Ok(answer)) => {
                let line = json!({ "jsonrpc": "2.0", "id": 1, "result": result });
                let again = reread(Direction::Client, &line)?;
                if again["result"] != serde_json::to_value(&answer).unwrap() {
                    return Err(format!("as a line, wrote {again}"));
                }
                Ok(again["result"].clone())
            }
        };
        checked.all(&schema, &def, &read);
    }

    // An error, from either side.
    for direction in [Direction::Client, Direction::Agent] {
        let read = |error: &Value| {
            let line = json!({ "jsonrpc": "2.0", "id": 1, "error": error });
            reread(direction, &line).map(|again| again["error"].clone())
        };
        checked.all(&schema, "Error", &read);
    }
    checked
}

#[test]
fn every_message_reads_and_writes_as_the_schema_says() {
    let checked = check_all();
    eprintln!(
        "checked {} messages generated from the schema",
        checked.samples
    );
    assert!(
        checked.failures.is_empty(),
        "{} problems in {} generated messages:\n{}",
        checked.failures.len(),
        checked.samples,
        checked.failures.join("\n")
    );
    let stale_fields: Vec<_> = UNTYPED_FIELDS
        .iter()
        .filter(|(object, field, _)| {
            let entry = ((*object).to_owned(), (*field).to_owned());
            !checked.used.fields.contains(&entry)
        })
        .collect();
    let stale_members: Vec<_> = UNTYPED_MEMBERS
        .iter()
        .filter(|(union, member, _)| {
            let entry = ((*union).to_owned(), (*member).to_owned());
            !checked.used.members.contains(&entry)
        })
        .collect();
    let stale_methods: Vec<_> = UNTYPED_METHODS
        .iter()
        .filter(|(method, _)| !checked.untyped_methods.contains(*method))
        .collect();
    let stale_lenient: Vec<_> = LENIENT_REQUIRED
        .iter()
        .filter(|(object, field, _)| {
            let entry = ((*object).to_owned(), (*field).to_owned());
            !checked.lenient.contains(&entry)
        })
        .collect();
    assert!(
        stale_fields.is_empty()
            && stale_members.is_empty()
            && stale_methods.is_empty()
            && stale_lenient.is_empty(),
        "allowances nothing needs: {stale_fields:?} {stale_members:?} {stale_methods:?} \
         {stale_lenient:?}"
    );
}

#[test]
fn only_the_unstable_schema_holds_what_the_crate_leaves_out() {
    let stable = Schema::parse(STABLE);
    let tables: Value = serde_json::from_str(STABLE_METHODS).expect("the method tables are JSON");
    let stable_methods: BTreeSet<&str> = ["agentMethods", "clientMethods", "protocolMethods"]
        .iter()
        .flat_map(|table| tables[table].as_object().expect("a table").values())
        .filter_map(Value::as_str)
        .collect();
    let mut failures = Vec::new();
    for (method, why) in UNTYPED_METHODS {
        if stable_methods.contains(method) {
            failures.push(format!("{method} is stable, so it is typed ({why})"));
        }
    }
    for (object, field, why) in UNTYPED_FIELDS {
        let def = stable.defs.get(*object);
        if def.is_some_and(|def| def["properties"].get(*field).is_some()) {
            failures.push(format!(
                "{object}.{field} is stable, so it is typed ({why})"
            ));
        }
    }
    for (union, member, why) in UNTYPED_MEMBERS {
        let tags = [
            stable.tags(union, "sessionUpdate"),
            stable.tags(union, "type"),
        ];
        if tags.iter().any(|tags| tags.contains(*member)) {
            failures.push(format!(
                "{union} {member} is stable, so it is typed ({why})"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_tag_and_spelling_the_crate_knows_is_the_schemas() {
    let schema = Schema::parse(UNSTABLE);
    let tagged: &[(&str, &str, &[&str])] = &[
        ("SessionUpdate", "sessionUpdate", SessionUpdate::TAGS),
        ("ContentBlock", "type", ContentBlock::TAGS),
        ("ToolCallContent", "type", ToolCallContent::TAGS),
        ("McpServer", "type", McpServer::TAGS),
        ("AuthMethod", "type", AuthMethod::TAGS),
        ("SessionConfigOption", "type", SessionConfigOption::TAGS),
        (
            "ElicitationPropertySchema",
            "type",
            ElicitationPropertySchema::TAGS,
        ),
        ("MultiSelectItems", "type", MultiSelectItems::TAGS),
        (
            "CreateElicitationRequest",
            "mode",
            CreateElicitationRequest::TAGS,
        ),
        (
            "CreateElicitationResponse",
            "action",
            CreateElicitationResponse::TAGS,
        ),
        (
            "RequestPermissionOutcome",
            "outcome",
            RequestPermissionOutcome::TAGS,
        ),
        (
            "SetSessionConfigOptionRequest",
            "type",
            ConfigValueKind::SPELLINGS,
        ),
    ];
    let mut failures = Vec::new();
    for (def, field, known) in tagged {
        let tags = schema.tags(def, field);
        for tag in *known {
            if !tags.contains(*tag) {
                failures.push(format!("{def}: {field} {tag:?} is not the schema's"));
            }
        }
    }
    let spelled: &[(&str, &[&str])] = &[
        ("ToolKind", ToolKind::SPELLINGS),
        ("ToolCallStatus", ToolCallStatus::SPELLINGS),
        ("PlanEntryPriority", PlanEntryPriority::SPELLINGS),
        ("PlanEntryStatus", PlanEntryStatus::SPELLINGS),
        ("NoticeSeverity", NoticeSeverity::SPELLINGS),
        ("CompactionStatus", CompactionStatus::SPELLINGS),
        ("StopReason", StopReason::SPELLINGS),
        ("PermissionOptionKind", PermissionOptionKind::SPELLINGS),
        ("AuthState", AuthState::SPELLINGS),
        ("Role", Role::SPELLINGS),
        ("SystemPromptMode", SystemPromptMode::SPELLINGS),
        (
            "SessionConfigOptionCategory",
            SessionConfigOptionCategory::SPELLINGS,
        ),
        ("StringFormat", StringFormat::SPELLINGS),
        ("ElicitationSchemaType", ElicitationSchemaType::SPELLINGS),
    ];
    for (def, known) in spelled {
        let spellings = schema.spellings(def);
        let known: BTreeSet<String> = known.iter().map(|known| (*known).to_owned()).collect();
        if spellings != known {
            failures.push(format!(
                "{def}: the crate knows {known:?}, the schema lists {spellings:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The generated messages check only what a message reaches, so a stable
/// def none reaches would be held to nothing.
#[test]
fn every_stable_def_is_reached_by_a_message() {
    let stable = Schema::parse(STABLE);
    let envelopes = [
        ("ClientRequest", "params"),
        ("ClientNotification", "params"),
        ("AgentRequest", "params"),
        ("AgentNotification", "params"),
        ("AgentResponse", "result"),
        ("ClientResponse", "result"),
    ];
    let mut frontier: Vec<String> = envelopes
        .iter()
        .flat_map(|(envelope, field)| stable.members(envelope, field))
        .chain(["CancelRequestNotification".to_owned(), "Error".to_owned()])
        .collect();
    let mut reached = BTreeSet::new();
    while let Some(def) = frontier.pop() {
        if reached.insert(def.clone()) {
            collect_refs(stable.def(&def), &mut frontier);
        }
    }
    let unreached: Vec<&String> = stable
        .defs
        .keys()
        .filter(|def| !reached.contains(*def))
        .filter(|def| !envelopes.iter().any(|(envelope, _)| envelope == def))
        .filter(|def| !def.starts_with("Ext"))
        .collect();
    assert!(
        unreached.is_empty(),
        "stable defs no message reaches: {unreached:?}"
    );
}
