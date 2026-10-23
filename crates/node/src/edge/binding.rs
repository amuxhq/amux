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
