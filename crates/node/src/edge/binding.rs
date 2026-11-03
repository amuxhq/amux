//! The cloud an account lives in, and who the account is.

use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CloudServiceId(String);

#[derive(Clone, Debug, thiserror::Error)]
#[error("cloud URL must be an HTTP(S) origin without credentials, path, query or fragment")]
pub struct CanonicalizeError;

impl CloudServiceId {
    pub fn canonicalize(value: &str) -> Result<Self, CanonicalizeError> {
        let url = reqwest::Url::parse(value).map_err(|_| CanonicalizeError)?;
        // Check the original path too: URL parsing normalizes /a/.. to /.
        let authority = value.split_once("://").ok_or(CanonicalizeError)?.1;
        let suffix = authority
            .find(['/', '?', '#'])
            .map(|i| &authority[i..])
            .unwrap_or("");
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.path() != "/"
            || value.contains('\\')
            || value.chars().any(char::is_whitespace)
            || !url.username().is_empty()
            || url.password().is_some()
            || authority
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .contains('@')
            || !matches!(suffix, "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CanonicalizeError);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for CloudServiceId {
    type Error = CanonicalizeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::canonicalize(&value)
    }
}
impl From<CloudServiceId> for String {
    fn from(value: CloudServiceId) -> Self {
        value.0
    }
}
impl std::fmt::Display for CloudServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
    /// The profile picture's URL, where the account has one.
    pub picture: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum UserinfoError {
    #[error("userinfo did not provide a subject")]
    MissingSubject,
    #[error("userinfo request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub(crate) async fn fetch_userinfo(
    http: &reqwest::Client,
    cloud_url: &str,
    token: &AccessToken,
) -> Result<UserInfo, UserinfoError> {
    let info: UserInfo = http
        .get(format!("{cloud_url}/connect/userinfo"))
        .timeout(std::time::Duration::from_secs(15))
        .bearer_auth(&token.bearer)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if info.sub.trim().is_empty() {
        return Err(UserinfoError::MissingSubject);
    }
    Ok(info)
}

/// What an account may do, as the account service answers its access read.
/// `pro` is the whole gate; the rest explains it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountAccess {
    pub pro: bool,
    /// When access ends, or ended; none when nothing ends.
    pub until_ms: Option<i64>,
    pub grant: Option<Grant>,
}

/// Why an account has access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grant {
    /// Given without a purchase.
    Granted,
    Purchased {
        store: PurchaseStore,
        will_renew: bool,
        renews_at_ms: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurchaseStore {
    AppStore,
    Web,
}

/// The access read: one GraphQL query, the same the phone made itself.
const ACCESS_QUERY: &str = "{ me { access { pro until grant { __typename \
    ... on Purchased { provider willRenew entitledUntil } } } } }";

#[derive(Deserialize)]
struct AccessAnswer {
    data: Option<AccessData>,
}

#[derive(Deserialize)]
struct AccessData {
    me: Option<AccessMe>,
}

#[derive(Deserialize)]
struct AccessMe {
    access: Option<AccessRead>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessRead {
    pro: bool,
    until: Option<String>,
    grant: Option<GrantRead>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantRead {
    #[serde(rename = "__typename")]
    type_name: String,
    provider: Option<String>,
    will_renew: Option<bool>,
    entitled_until: Option<String>,
}

/// Milliseconds since the epoch of an RFC 3339 time, as GraphQL's DateTime
/// writes it.
fn rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

pub(crate) async fn fetch_access(
    http: &reqwest::Client,
    cloud_url: &str,
    token: &AccessToken,
) -> Result<Option<AccountAccess>, reqwest::Error> {
    let answer: AccessAnswer = http
        .post(format!("{cloud_url}/api/graphql"))
        .timeout(std::time::Duration::from_secs(15))
        .bearer_auth(&token.bearer)
        .json(&serde_json::json!({ "query": ACCESS_QUERY }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(read) = answer
        .data
        .and_then(|data| data.me)
        .and_then(|me| me.access)
    else {
        return Ok(None);
    };
    let grant = read.grant.map(|grant| match grant.type_name.as_str() {
        "Purchased" => Grant::Purchased {
            store: match grant.provider.as_deref() {
                Some("REVENUE_CAT") => PurchaseStore::AppStore,
                _ => PurchaseStore::Web,
            },
            will_renew: grant.will_renew.unwrap_or(false),
            renews_at_ms: grant.entitled_until.as_deref().and_then(rfc3339_ms),
        },
        // A member added later reads as a grant, not a purchase with
        // everything missing.
        _ => Grant::Granted,
    });
    Ok(Some(AccountAccess {
        pro: read.pro,
        until_ms: read.until.as_deref().and_then(rfc3339_ms),
        grant,
    }))
}

#[cfg(test)]
mod access_tests {
    use super::*;

    #[test]
    fn an_access_time_reads_as_milliseconds() {
        assert_eq!(rfc3339_ms("2026-11-03T00:00:00Z"), Some(1_793_664_000_000));
        assert_eq!(
            rfc3339_ms("2026-11-03T01:00:00+01:00"),
            Some(1_793_664_000_000)
        );
        assert_eq!(rfc3339_ms("soon"), None);
    }
}
