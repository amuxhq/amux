//! Fetching a sent report from the bucket the account service stores
//! reports in, for `repro`. The service keeps a report's files under
//! `reports/<user id>/<report id>/`, so a report id alone is found by
//! listing `reports/` and a prefix copied from the service is listed as
//! it is.
//!
//! The bucket is any S3-compatible store; where it is and a read-only key
//! for it come from the environment ([`Bucket::from_env`]), never from
//! this source. Requests are signed by `curl --aws-sigv4`, given its
//! options on standard input so the key never reaches a process listing.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The environment variables a fetch reads.
pub const ENDPOINT: &str = "AMUX_REPORTS_S3_ENDPOINT";
pub const BUCKET: &str = "AMUX_REPORTS_S3_BUCKET";
pub const ACCESS_KEY_ID: &str = "AMUX_REPORTS_S3_ACCESS_KEY_ID";
pub const SECRET_ACCESS_KEY: &str = "AMUX_REPORTS_S3_SECRET_ACCESS_KEY";
/// Optional: the signing region, `auto` (what R2 signs with) when unset.
pub const REGION: &str = "AMUX_REPORTS_S3_REGION";

/// Where the account service keeps reports, under its own user ids.
const REPORTS: &str = "reports/";

/// A bucket and the key that reads it.
#[derive(Clone)]
pub struct Bucket {
    /// The store's address, scheme and host, with no bucket in it.
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
}

impl std::fmt::Debug for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bucket")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

/// One page of a bucket listing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub keys: Vec<String>,
    /// Where the next page starts, when there is one.
    pub next: Option<String>,
}

impl Bucket {
    /// The bucket the environment names, or which variables are missing.
    pub fn from_env() -> Result<Bucket, String> {
        Self::from_vars(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
    }

    /// [`Bucket::from_env`] over any source of variables.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Bucket, String> {
        let missing: Vec<&str> = [ENDPOINT, BUCKET, ACCESS_KEY_ID, SECRET_ACCESS_KEY]
            .into_iter()
            .filter(|name| var(name).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "fetching a report needs {} in the environment",
                missing.join(", ")
            ));
        }
        let var = |name| var(name).unwrap_or_default();
        Ok(Bucket {
            endpoint: var(ENDPOINT).trim_end_matches('/').to_owned(),
            bucket: var(BUCKET),
            access_key_id: var(ACCESS_KEY_ID),
            secret_access_key: var(SECRET_ACCESS_KEY),
            region: Some(var(REGION))
                .filter(|region| !region.is_empty())
                .unwrap_or_else(|| "auto".to_owned()),
        })
    }

    /// A ListObjectsV2 request for the keys under `prefix`, from `after`
    /// on, addressed by path as every S3-compatible store accepts.
    pub fn list_url(&self, prefix: &str, after: Option<&str>) -> String {
        let mut url = format!(
            "{}/{}?list-type=2&prefix={}",
            self.endpoint,
            encode(&self.bucket, false),
            encode(prefix, false)
        );
        if let Some(after) = after {
            url.push_str("&continuation-token=");
            url.push_str(&encode(after, false));
        }
        url
    }

    /// A GetObject request for `key`.
    pub fn object_url(&self, key: &str) -> String {
        format!(
            "{}/{}/{}",
            self.endpoint,
            encode(&self.bucket, false),
            encode(key, true)
        )
    }

    /// The options curl reads from standard input for one signed request
    /// to `url`, written to `output` when given and to standard output
    /// otherwise: the key as its user, signed for this region's `s3`.
    pub fn curl_config(&self, url: &str, output: Option<&Path>) -> String {
        let mut config = format!(
            "silent\nshow-error\nfail\nuser = {}\naws-sigv4 = {}\nurl = {}\n",
            quoted(&format!(
                "{}:{}",
                self.access_key_id, self.secret_access_key
            )),
            quoted(&format!("aws:amz:{}:s3", self.region)),
            quoted(url),
        );
        if let Some(output) = output {
            config.push_str(&format!("output = {}\n", quoted(&output.to_string_lossy())));
        }
        config
    }

    fn curl(&self, url: &str, output: Option<&Path>) -> Result<Vec<u8>, String> {
        let mut child = Command::new("curl")
            .args(["--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("curl does not run: {error}"))?;
        let config = self.curl_config(url, output);
        child
            .stdin
            .take()
            .expect("curl's input is piped")
            .write_all(config.as_bytes())
            .map_err(|error| format!("curl does not read its options: {error}"))?;
        let done = child
            .wait_with_output()
            .map_err(|error| format!("curl did not finish: {error}"))?;
        if done.status.success() {
            Ok(done.stdout)
        } else {
            Err(format!(
                "{url}: {}",
                String::from_utf8_lossy(&done.stderr).trim()
            ))
        }
    }

    /// Every key under `prefix`, page by page.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut keys = Vec::new();
        let mut after = None;
        loop {
            let body = self.curl(&self.list_url(prefix, after.as_deref()), None)?;
            let page = listing(&String::from_utf8_lossy(&body));
            keys.extend(page.keys);
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(keys),
            }
        }
    }

    /// Downloads the report `named` into `into`: a report id as the
    /// account service gave it (with or without hyphens), or the prefix its
    /// files are under. Returns the report's id.
    pub fn fetch(&self, named: &str, into: &Path) -> Result<PathBuf, String> {
        let (id, keys) = match report_id(named) {
            Some(id) => {
                let keys: Vec<String> = self
                    .list(REPORTS)?
                    .into_iter()
                    .filter(|key| key.split('/').nth(2) == Some(id.as_str()))
                    .collect();
                (id, keys)
            }
            None => {
                let prefix = format!("{}/", named.trim_end_matches('/'));
                let id = prefix
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                (id, self.list(&prefix)?)
            }
        };
        if keys.is_empty() {
            return Err(format!("no report {named} in the bucket"));
        }
        let dir = into.join(&id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        }
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for key in &keys {
            let Some(name) = key.rsplit('/').next().filter(|name| plain(name)) else {
                return Err(format!("a stored file named {key:?}"));
            };
            self.curl(&self.object_url(key), Some(&dir.join(name)))?;
        }
        Ok(dir)
    }
}

/// A report id in the form the service stores it under (32 hex digits,
/// lower case), when `named` is one.
pub fn report_id(named: &str) -> Option<String> {
    let hex: String = named.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// A file name with no way out of the directory it is written into.
fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

/// The keys of a ListObjectsV2 answer and where its next page starts.
pub fn listing(xml: &str) -> Listing {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<Contents>") {
        rest = &rest[at..];
        let end = rest.find("</Contents>").unwrap_or(rest.len());
        if let Some(key) = element(&rest[..end], "Key") {
            keys.push(key);
        }
        rest = &rest[end..];
    }
    let truncated = element(xml, "IsTruncated").is_some_and(|value| value == "true");
    Listing {
        keys,
        next: element(xml, "NextContinuationToken").filter(|_| truncated),
    }
}

/// The text of the first `<name>` element in `xml`, unescaped.
fn element(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{name}>"))?;
    Some(
        xml[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// `text` percent-encoded but for S3's unreserved characters, and `/` when
/// it separates a key's parts.
fn encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A value in a curl config file.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
