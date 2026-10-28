//! Reports leaving the machine: the usage event a broken check records, a
//! person's report written around a dump, and the one upload that sends
//! either to the profile's account service.
//!
//! A report goes to `POST <account service>/api/reports` as
//! `multipart/form-data`, one section per file, each section named after
//! the file it carries:
//!
//! ```text
//! report.json     what the report is: an automatic one's check and reason,
//!                 or a person's note and marks, and each part declared
//!                 present or absent
//! frame.*         the frozen screen: frame.png from the phone, frame.txt
//!                 and frame.json from the terminal; none for an automatic
//!                 report
//! dump.pb.zst     the dump, packed as one wire.DumpPart and compressed
//! ```
//!
//! A person's report is written to `reports/report-<unix ms>-<id>/` with its
//! files and the dump under `dump/`, so what was sent stays readable here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use serde_json::{Value, json};
use uuid::Uuid;
use wire::{DumpFile, DumpRequest, SubmitReportRequest, SubmitReportResponse};

use crate::runtime::ProfileRuntime;

/// The check an agent process names by sentence alone: one from before
/// checks had ids.
pub(crate) const UNNAMED_AGENT_CHECK: &str = "agent.unnamed";
/// The report header's schema this build writes.
pub const REPORT_SCHEMA: u32 = 3;
/// What a report's header is called, and its packed dump.
pub const REPORT_JSON: &str = "report.json";
pub const DUMP_ARCHIVE: &str = "dump.pb.zst";
/// Where a person's report keeps its dump.
pub const REPORT_DUMP: &str = "dump";
/// How a person's report directory is named, before its time and id.
const REPORT_DIR: &str = "report-";
/// The files a client may hand in beside `report.json`: its frozen frame,
/// and the phone's view-state recording.
pub const CLIENT_FILES: &[&str] = &["frame.png", "frame.txt", "frame.json", "trace.jsonl"];
/// How hard the dump is compressed: redacted JSON and protobuf shrink by
/// thirty times at this level, and it costs a fraction of a second.
const ZSTD_LEVEL: i32 = 3;
/// One upload's limit: a bundle of a few megabytes on a slow uplink.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Tries after the first, each after a longer wait, for an answer worth
/// trying again.
const RETRIES: u32 = 2;
const BACKOFF: Duration = Duration::from_secs(5);

/// Which part of amux a check belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Core {
    Daemon,
    Agent,
    Relay,
    Path,
    Transcript,
}

impl Core {
    fn analytics(self) -> analytics::CheckCore {
        match self {
            Core::Daemon => analytics::CheckCore::Daemon,
            Core::Agent => analytics::CheckCore::Agent,
            Core::Relay => analytics::CheckCore::Relay,
            Core::Path => analytics::CheckCore::Path,
            Core::Transcript => analytics::CheckCore::Transcript,
        }
    }

    fn name(self) -> &'static str {
        analytics::Token::as_str(self.analytics())
    }
}

/// Whether the installation sends its automatic dumps, asked before each
/// one so turning it off stops a running daemon at once.
#[derive(Clone, Default)]
pub struct AutoReports(Option<Arc<dyn Fn() -> bool + Send + Sync>>);

impl AutoReports {
    /// Nothing is sent: tests, test networks, and an installation whose
    /// person never turned it on.
    pub fn off() -> Self {
        Self(None)
    }

    pub fn new(gate: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self(Some(gate))
    }

    fn on(&self) -> bool {
        self.0.as_ref().is_some_and(|gate| gate())
    }
}

/// The id a check is reported by: its own, or `<core>.over_budget` for a
/// step over its budget, which `check` names by being empty.
pub(crate) fn check_id(core: Core, check: &str) -> String {
    if check.is_empty() {
        format!("{}.over_budget", core.name())
    } else {
        check.to_owned()
    }
}

/// Records the usage event for a check that broke and that the dump limit
/// let through. `check` is empty for a step over its budget.
pub(crate) fn check_failed(
    analytics: &analytics::Analytics,
    core: Core,
    check: &str,
    held_back: u64,
) {
    let cause = if check.is_empty() {
        analytics::CheckCause::OverBudget
    } else {
        analytics::CheckCause::Invariant
    };
    let id = check_id(core, check);
    let Some(check) = analytics::CheckId::new(&id) else {
        tracing::warn!(check = %id, "a check's id is not a token; not recorded");
        return;
    };
    analytics.record(analytics::Event::CheckFailed {
        core: core.analytics(),
        check,
        cause,
        held_back: u32::try_from(held_back).unwrap_or(u32::MAX),
    });
}

/// Why a report was not sent.
#[derive(Debug, thiserror::Error)]
pub enum NotSent {
    #[error("this profile is not signed in to an amux account")]
    SignedOut,
    #[error("{0}")]
    Refused(String),
    #[error("the account service could not be reached: {0}")]
    Unreachable(String),
    #[error("the report could not be read: {0}")]
    Unreadable(String),
}

/// What the account service said it kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: String,
    pub received_at_ms: i64,
}

/// Why a person's report could not be written.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("{0}")]
    Invalid(String),
    #[error("writing the report: {0}")]
    Write(#[from] std::io::Error),
}

impl ReportError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::Invalid(_) => wire::ErrorCode::InvalidArgument,
            Self::Write(_) => wire::ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

impl ProfileRuntime {
    /// Records `check_failed` on this profile.
    pub(crate) fn check_failed(&self, core: Core, check: &str, held_back: u64) {
        check_failed(self.analytics(), core, check, held_back);
    }

    /// Sends an automatic dump a check asked for, off the caller, when the
    /// installation sends them. A failure is logged; the bundle stays.
    pub(crate) fn send_automatic(self: &Arc<Self>, bundle: &Path, request: &DumpRequest) {
        if !self.auto_reports.on() {
            return;
        }
        let header = json!({
            "schema_version": REPORT_SCHEMA,
            "kind": "automatic",
            "build": crate::version(),
            "created_at_ms": self.clock_now(),
            "core": core_of(request),
            "check": request.check,
            "reason": interpret::redact_text(&request.reason),
            "parts": {
                "frame": absent("an automatic report has no screen"),
                "trace": absent("an automatic report has no screen"),
                "dump": "present",
            },
        });
        let bundle = bundle.to_owned();
        let me = self.clone();
        tokio::spawn(async move {
            let header = serde_json::to_vec_pretty(&header).expect("JSON writes");
            match me.upload(header, Vec::new(), Some(&bundle)).await {
                Ok(receipt) => {
                    tracing::info!(bundle = %bundle.display(), id = %receipt.id, "sent the dump")
                }
                Err(error) => {
                    tracing::warn!(bundle = %bundle.display(), %error, "the dump was not sent")
                }
            }
        });
    }

    /// Writes a person's report around the dump taken when their screen
    /// froze, and sends it when the profile is signed in. Called again with
    /// the report's own directory, it rewrites the client's files and sends
    /// it again: a retry.
    pub async fn submit_report(
        &self,
        request: SubmitReportRequest,
    ) -> Result<SubmitReportResponse, ReportError> {
        let SubmitReportRequest { dump_path, files } = request;
        let mut header = None;
        let mut carried = Vec::new();
        for file in files {
            if file.name == REPORT_JSON {
                header = Some(file.contents);
            } else if CLIENT_FILES.contains(&file.name.as_str()) {
                carried.push(file);
            } else {
                return Err(ReportError::Invalid(format!(
                    "a report carries no file named {:?}",
                    file.name
                )));
            }
        }
        let header = header.ok_or_else(|| ReportError::Invalid("no report.json".to_owned()))?;
        let mut header: Value = serde_json::from_slice(&header)
            .map_err(|error| ReportError::Invalid(format!("report.json: {error}")))?;
        if !header.is_object() {
            return Err(ReportError::Invalid(
                "report.json is not an object".to_owned(),
            ));
        }
        // Placed only once the request reads: from here the dump has moved,
        // and every answer names where to send it again from.
        let (dir, dump) = self.place_report(&dump_path)?;
        let report_path = dir.to_string_lossy().into_owned();
        // The dump is this host's to declare: the client only asked for it.
        header["schema_version"] = json!(REPORT_SCHEMA);
        header["kind"] = json!("report");
        // The client may know why there is none (its dump call failed).
        let declared = header["parts"]["dump"].get("absent").is_some();
        match &dump {
            Some(_) => header["parts"]["dump"] = json!("present"),
            None if declared => {}
            None => header["parts"]["dump"] = absent("the dump could not be taken"),
        }
        let header = serde_json::to_vec_pretty(&header).expect("JSON writes");
        let written = std::iter::once((REPORT_JSON, header.as_slice()))
            .chain(
                carried
                    .iter()
                    .map(|file| (file.name.as_str(), file.contents.as_slice())),
            )
            .try_for_each(|(name, contents)| private_file(&dir.join(name), contents));
        if let Err(error) = written {
            return Ok(SubmitReportResponse {
                report_path,
                not_sent: ReportError::from(error).to_string(),
                ..SubmitReportResponse::default()
            });
        }
        Ok(match self.upload(header, carried, dump.as_deref()).await {
            Ok(receipt) => SubmitReportResponse {
                report_path,
                id: receipt.id,
                received_at_ms: receipt.received_at_ms,
                not_sent: String::new(),
            },
            Err(why) => SubmitReportResponse {
                report_path,
                not_sent: why.to_string(),
                ..SubmitReportResponse::default()
            },
        })
    }

    /// The report's directory and its dump: a new `report-…` directory
    /// with the dump moved under it, or the report directory named again
    /// for a retry. Either must be under this installation's reports.
    fn place_report(&self, named: &str) -> Result<(PathBuf, Option<PathBuf>), ReportError> {
        // Named as this installation names its reports, whatever links the
        // caller's spelling went through.
        let inside = |path: &Path| {
            let reports = self.reports.canonicalize().ok()?;
            let path = path.canonicalize().ok()?;
            let name = path.file_name()?;
            (path.parent() == Some(reports.as_path())).then(|| self.reports.join(name))
        };
        if !named.is_empty() {
            let Some(path) = inside(Path::new(named)) else {
                return Err(ReportError::Invalid(format!(
                    "{named} is not a bundle in this installation's reports"
                )));
            };
            // A report directory, even one whose files a full disk kept
            // from being written.
            let is_report = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(REPORT_DIR));
            if is_report {
                let dump = path.join(REPORT_DUMP);
                return Ok((path, dump.is_dir().then_some(dump)));
            }
            let dir = self.new_report_dir()?;
            let dump = dir.join(REPORT_DUMP);
            std::fs::rename(&path, &dump)?;
            return Ok((dir, Some(dump)));
        }
        Ok((self.new_report_dir()?, None))
    }

    /// A new report's directory. Reports hold prompts, code and paths:
    /// only this user reads them.
    fn new_report_dir(&self) -> std::io::Result<PathBuf> {
        let id = Uuid::new_v4().simple().to_string();
        let dir = self
            .reports
            .join(format!("{REPORT_DIR}{}-{}", self.clock_now(), &id[..8]));
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir)?;
        Ok(dir)
    }

    /// Sends one report: its header, its frame files and its dump packed
    /// and compressed, with the profile's account bearer.
    async fn upload(
        &self,
        header: Vec<u8>,
        carried: Vec<DumpFile>,
        dump: Option<&Path>,
    ) -> Result<Receipt, NotSent> {
        let edge = self.edge().ok_or(NotSent::SignedOut)?;
        let service = edge.account_service().ok_or(NotSent::SignedOut)?;
        let archive = match dump {
            Some(dump) => {
                let dump = dump.to_owned();
                Some(
                    tokio::task::spawn_blocking(move || archive(&dump))
                        .await
                        .map_err(|error| NotSent::Unreadable(error.to_string()))?
                        .map_err(|error| NotSent::Unreadable(error.to_string()))?,
                )
            }
            None => None,
        };
        let mut sections = vec![(REPORT_JSON.to_owned(), header)];
        sections.extend(carried.into_iter().map(|file| (file.name, file.contents)));
        if let Some(archive) = archive {
            sections.push((DUMP_ARCHIVE.to_owned(), archive));
        }
        let (content_type, body) = multipart(&sections);
        let url = format!("{}/api/reports", service.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(UPLOAD_TIMEOUT)
            .build()
            .map_err(|error| NotSent::Unreachable(error.to_string()))?;
        let mut wait = BACKOFF;
        let mut tries = 0;
        loop {
            // Asked each time: a token can lapse during the backoff.
            let token = edge.access_token().await.map_err(|_| NotSent::SignedOut)?;
            let answer = client
                .post(&url)
                .bearer_auth(&token.bearer)
                .header(reqwest::header::CONTENT_TYPE, &content_type)
                .body(body.clone())
                .send()
                .await;
            let again = match answer {
                Ok(response) if response.status().is_success() => {
                    let text = response.text().await.unwrap_or_default();
                    return Ok(receipt(&text));
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    let retry = status.is_server_error()
                        || status == reqwest::StatusCode::REQUEST_TIMEOUT
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                    let why = NotSent::Refused(refusal(status, &text));
                    if !retry {
                        return Err(why);
                    }
                    why
                }
                Err(error) => NotSent::Unreachable(error.to_string()),
            };
            if tries == RETRIES {
                return Err(again);
            }
            tries += 1;
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
    }
}

/// Which core an automatic dump's request is for, as a report names it.
fn core_of(request: &DumpRequest) -> &'static str {
    match request.scope() {
        wire::DumpScope::Agent => Core::Agent.name(),
        wire::DumpScope::Transcript => Core::Transcript.name(),
        _ => Core::Daemon.name(),
    }
}

/// Writes `bytes` readable by this user only, replacing what a retry left.
fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

fn absent(reason: &str) -> Value {
    json!({ "absent": { "reason": reason } })
}

/// A bundle directory as one compressed `wire.DumpPart`.
pub fn archive(bundle: &Path) -> std::io::Result<Vec<u8>> {
    let packed = crate::dump::pack(bundle)?.encode_to_vec();
    zstd::bulk::compress(&packed, ZSTD_LEVEL)
}

/// The body of a `multipart/form-data` request, one section per file, each
/// named after the file it carries, and its content type.
fn multipart(sections: &[(String, Vec<u8>)]) -> (String, Vec<u8>) {
    let boundary = format!("amux.{}", Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (name, bytes) in sections {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", content_type(name)).as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("txt") => "text/plain; charset=utf-8",
        Some("jsonl") => "application/x-ndjson",
        _ => "application/octet-stream",
    }
}

/// The receipt in the account service's answer; one it did not spell is
/// still a report it kept.
fn receipt(text: &str) -> Receipt {
    let value: Value = serde_json::from_str(text).unwrap_or_default();
    let received_at_ms = value["received_at"]
        .as_str()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map_or(0, |at| at.timestamp_millis());
    Receipt {
        id: value["id"].as_str().unwrap_or_default().to_owned(),
        received_at_ms,
    }
}

/// A refusal in the service's own words where it gave any.
fn refusal(status: reqwest::StatusCode, text: &str) -> String {
    let value: Value = serde_json::from_str(text).unwrap_or_default();
    let said = ["reason", "error_description", "error"]
        .iter()
        .find_map(|key| value[*key].as_str().map(str::to_owned));
    // The service's own words where it gave any: they are what a person
    // is shown.
    said.unwrap_or_else(|| status.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_unpacks_to_the_bundle_it_packed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents/a")).unwrap();
        std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("agents/a/row.pb"), b"row").unwrap();
        let archive = archive(dir.path()).unwrap();
        let packed = zstd::stream::decode_all(archive.as_slice()).unwrap();
        let part = wire::DumpPart::decode(packed.as_slice()).unwrap();
        let names: Vec<_> = part.files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["agents/a/row.pb", "manifest.json"]);
    }

    #[test]
    fn a_body_has_one_section_per_file_named_after_it() {
        let (content_type, body) = multipart(&[
            ("report.json".into(), b"{}".to_vec()),
            ("dump.pb.zst".into(), vec![1, 2]),
        ]);
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(text.matches(&format!("--{boundary}\r\n")).count(), 2);
        assert!(text.contains("name=\"report.json\"; filename=\"report.json\""));
        assert!(text.contains("Content-Type: application/octet-stream"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn a_receipt_reads_the_services_id_and_time() {
        let receipt = receipt(r#"{"id":"r1","received_at":"2026-10-27T10:00:00Z"}"#);
        assert_eq!(receipt.id, "r1");
        assert_eq!(receipt.received_at_ms, 1_793_095_200_000);
    }

    #[test]
    fn an_over_budget_step_is_named_by_its_core() {
        let recording = analytics::Recording::new();
        let analytics = analytics::Analytics::new(Uuid::nil(), recording.clone());
        check_failed(&analytics, Core::Path, "", 4);
        check_failed(&analytics, Core::Daemon, "daemon.generation_fell", 0);
        check_failed(&analytics, Core::Agent, "Not An Id", 0);
        let ids: Vec<_> = recording
            .named("check_failed")
            .into_iter()
            .map(|event| event.properties()["check"].clone())
            .collect();
        assert_eq!(
            ids,
            [json!("path.over_budget"), json!("daemon.generation_fell")]
        );
    }
}
