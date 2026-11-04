//! The OAuth 2.0 exchanges amux makes with the cloud: signing in a device
//! (RFC 8628), refreshing an access token (RFC 6749 §6) and revoking a
//! refresh token at sign-out (RFC 7009). All are form posts to the cloud's
//! token service; the first two are answered in JSON.

use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use thiserror::Error;

use crate::auth::AccessToken;

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("OAuth configuration error: {0}")]
    Config(String),
    #[error("OAuth request error: {0}")]
    Request(String),
    #[error("Refresh token expired or revoked - run 'amux login' to re-authenticate")]
    RefreshTokenExpired,
    #[error("No refresh token returned")]
    NoRefreshToken,
}

/// The OAuth client the CLI signs in as.
pub const CLI_CLIENT_ID: &str = "cli";

/// What a device sign-in asks for: the account's identity, a refresh token
/// that outlives this run, and the cloud's API.
const DEVICE_SCOPES: &str = "openid profile email offline_access api";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// RFC 8628's poll interval when the cloud names none, and what each
/// `slow_down` adds to it.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// The HTTP client for both exchanges. Redirects are refused: following one
/// would forward the credentials to whatever host the redirect names.
fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| OAuthError::Config(e.to_string()))
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

/// How the token service answered one post.
enum Answer<T> {
    Ok(T),
    /// An OAuth error response: its code and description.
    Refused(ErrorResponse),
}

/// Posts `form` to `url` and reads the answer: the success shape on a 2xx,
/// the OAuth error shape otherwise.
async fn post<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Answer<T>, OAuthError> {
    let response = http
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;
    if status.is_success() {
        return serde_json::from_slice(&body)
            .map(Answer::Ok)
            .map_err(|e| OAuthError::Request(format!("unreadable answer from {url}: {e}")));
    }
    match serde_json::from_slice::<ErrorResponse>(&body) {
        Ok(error) => Ok(Answer::Refused(error)),
        Err(_) => Err(OAuthError::Request(format!("{url} answered {status}"))),
    }
}

fn refused(error: &ErrorResponse) -> OAuthError {
    let description = error
        .error_description
        .as_deref()
        .map(|description| format!(": {description}"))
        .unwrap_or_default();
    OAuthError::Request(format!("{}{description}", error.error))
}

/// The device authorization request's parameter naming the machine asking,
/// which the approval page shows so the person can tell their own sign-in
/// from one someone else started. A service that does not know it ignores
/// it.
pub const DEVICE_NAME_PARAMETER: &str = "device_name";

/// A device sign-in under way (RFC 8628): the code the person enters, and
/// the page to enter it on, until [`DeviceSignIn::finish`] has the token.
pub struct DeviceSignIn {
    pub user_code: String,
    pub verification_uri: String,
    /// The page with the code filled in, when the service offers one.
    pub verification_uri_complete: Option<String>,
    /// When the code stops working.
    pub expires_at: SystemTime,
    device_code: String,
    interval: Duration,
    deadline: Instant,
    token_url: String,
    http: reqwest::Client,
}

impl DeviceSignIn {
    /// Asks the cloud for a device code under the CLI's OAuth client,
    /// naming this machine as `device_name`.
    pub async fn start(cloud_url: &str, device_name: &str) -> Result<Self, OAuthError> {
        let http = http_client()?;
        let details: DeviceAuthorization = match post(
            &http,
            &format!("{cloud_url}/connect/deviceauthorization"),
            &[
                ("client_id", CLI_CLIENT_ID),
                ("scope", DEVICE_SCOPES),
                (DEVICE_NAME_PARAMETER, device_name),
            ],
        )
        .await?
        {
            Answer::Ok(details) => details,
            Answer::Refused(error) => return Err(refused(&error)),
        };
        let lifetime = Duration::from_secs(details.expires_in);
        Ok(Self {
            user_code: details.user_code,
            verification_uri: details.verification_uri,
            verification_uri_complete: details.verification_uri_complete,
            expires_at: SystemTime::now() + lifetime,
            device_code: details.device_code,
            interval: details
                .interval
                .map_or(DEFAULT_INTERVAL, Duration::from_secs),
            deadline: Instant::now() + lifetime,
            token_url: format!("{cloud_url}/connect/token"),
            http,
        })
    }

    /// Polls until the person approves the code, and returns the refresh
    /// token it was exchanged for.
    pub async fn finish(mut self) -> Result<String, OAuthError> {
        loop {
            tokio::time::sleep(self.interval).await;
            let answer = post::<TokenResponse>(
                &self.http,
                &self.token_url,
                &[
                    ("grant_type", DEVICE_CODE_GRANT),
                    ("device_code", &self.device_code),
                    ("client_id", CLI_CLIENT_ID),
                ],
            )
            .await?;
            match answer {
                Answer::Ok(tokens) => {
                    return tokens.refresh_token.ok_or(OAuthError::NoRefreshToken);
                }
                Answer::Refused(error) => match error.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => self.interval += DEFAULT_INTERVAL,
                    _ => return Err(refused(&error)),
                },
            }
            if Instant::now() >= self.deadline {
                return Err(OAuthError::Request(
                    "the sign-in code expired before it was approved".into(),
                ));
            }
        }
    }
}

/// Get a new access token using a refresh token, as the client it was
/// issued to.
pub async fn refresh_access_token(
    cloud_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<(AccessToken, Option<String>), OAuthError> {
    let http = http_client()?;
    let tokens: TokenResponse = match post(
        &http,
        &format!("{cloud_url}/connect/token"),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
    )
    .await?
    {
        Answer::Ok(tokens) => tokens,
        Answer::Refused(error) if error.error == "invalid_grant" => {
            return Err(OAuthError::RefreshTokenExpired);
        }
        Answer::Refused(error) => return Err(refused(&error)),
    };

    let access_token = AccessToken {
        bearer: tokens.access_token,
        expires_at: tokens
            .expires_in
            .map(|seconds| SystemTime::now() + Duration::from_secs(seconds)),
        // The identity service answers an OAuth exchange, not an entitlement
        // question; what this account may do comes back from the connect call.
        tier: None,
    };
    Ok((access_token, tokens.refresh_token))
}

/// Tells the cloud a refresh token is no longer wanted, so a copy taken
/// before a sign-out stops working. The service answers success for a
/// token it no longer knows, so only a refusal or no answer is an error.
pub async fn revoke_refresh_token(
    cloud_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<(), OAuthError> {
    let url = format!("{cloud_url}/connect/revocation");
    let response = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        // A sign-out waits on this, and goes ahead without it.
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| OAuthError::Config(e.to_string()))?
        .post(&url)
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ])
        .send()
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.bytes().await.unwrap_or_default();
    Err(match serde_json::from_slice::<ErrorResponse>(&body) {
        Ok(error) => refused(&error),
        Err(_) => OAuthError::Request(format!("{url} answered {status}")),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// A token service that answers each post with the next scripted
    /// `(status, body)` and keeps every form it was sent.
    struct Service {
        url: String,
        forms: Arc<Mutex<Vec<(String, String)>>>,
    }

    async fn serve(answers: Vec<(u16, serde_json::Value)>) -> Service {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let forms = Arc::new(Mutex::new(Vec::new()));
        let seen = forms.clone();
        let mut answers = VecDeque::from(answers);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                // The head, then as much body as it says it has.
                let (head, body) = loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&request).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|length| length.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if body.len() >= length {
                            break (head.to_owned(), body.to_owned());
                        }
                    }
                };
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                seen.lock().unwrap().push((path, body));
                let (status, answer) = answers.pop_front().expect("a scripted answer");
                let answer = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Service { url, forms }
    }

    fn device_authorization() -> (u16, serde_json::Value) {
        (
            200,
            serde_json::json!({
                "device_code": "device-1",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://cloud.test/device",
                "expires_in": 300,
                "interval": 0,
            }),
        )
    }

    #[tokio::test]
    async fn device_sign_in_polls_until_approved_and_keeps_the_refresh_token() {
        let pending = (400, serde_json::json!({"error": "authorization_pending"}));
        let service = serve(vec![
            device_authorization(),
            pending.clone(),
            pending,
            (
                200,
                serde_json::json!({"access_token": "access", "refresh_token": "refresh-1", "expires_in": 3600}),
            ),
        ])
        .await;

        let started = DeviceSignIn::start(&service.url, "Jordan's Mac")
            .await
            .unwrap();
        assert_eq!(started.verification_uri, "https://cloud.test/device");
        assert_eq!(started.finish().await.unwrap(), "refresh-1");
        let forms = service.forms.lock().unwrap();
        assert_eq!(forms[0].0, "/connect/deviceauthorization");
        assert_eq!(
            forms[0].1,
            "client_id=cli&scope=openid+profile+email+offline_access+api&device_name=Jordan%27s+Mac"
        );
        assert_eq!(forms.len(), 4);
        for (path, form) in &forms[1..] {
            assert_eq!(path, "/connect/token");
            assert_eq!(
                form,
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=device-1&client_id=cli"
            );
        }
    }

    #[tokio::test]
    async fn device_sign_in_stops_when_refused() {
        let service = serve(vec![
            device_authorization(),
            (
                400,
                serde_json::json!({"error": "access_denied", "error_description": "the person declined"}),
            ),
        ])
        .await;

        let error = DeviceSignIn::start(&service.url, "desk")
            .await
            .unwrap()
            .finish()
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "OAuth request error: access_denied: the person declined"
        );
    }

    /// A sign-out revokes its refresh token as the client it was issued to;
    /// a refusal is reported, not hidden.
    #[tokio::test]
    async fn revocation_names_the_refresh_token_and_its_client() {
        let service = serve(vec![
            (200, serde_json::json!({})),
            (400, serde_json::json!({"error": "invalid_client"})),
        ])
        .await;
        revoke_refresh_token(&service.url, "cli", "refresh-1")
            .await
            .unwrap();
        let error = revoke_refresh_token(&service.url, "cli", "refresh-1")
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "OAuth request error: invalid_client");
        let forms = service.forms.lock().unwrap();
        assert_eq!(forms[0].0, "/connect/revocation");
        assert_eq!(
            forms[0].1,
            "token=refresh-1&token_type_hint=refresh_token&client_id=cli"
        );
    }

    #[tokio::test]
    async fn refresh_returns_the_new_tokens_and_a_refused_grant_as_expired() {
        let service = serve(vec![
            (
                200,
                serde_json::json!({"access_token": "access-2", "refresh_token": "refresh-2", "expires_in": 60, "token_type": "Bearer"}),
            ),
            (400, serde_json::json!({"error": "invalid_grant"})),
            (500, serde_json::json!({"oops": true})),
        ])
        .await;

        let (access, refresh) = refresh_access_token(&service.url, "phone", "refresh-1")
            .await
            .unwrap();
        assert_eq!(access.bearer, "access-2");
        assert!(access.expires_at.is_some());
        assert_eq!(refresh.as_deref(), Some("refresh-2"));
        assert_eq!(
            service.forms.lock().unwrap()[0].1,
            "grant_type=refresh_token&refresh_token=refresh-1&client_id=phone"
        );
        assert!(matches!(
            refresh_access_token(&service.url, "phone", "refresh-1").await,
            Err(OAuthError::RefreshTokenExpired)
        ));
        assert!(matches!(
            refresh_access_token(&service.url, "phone", "refresh-1").await,
            Err(OAuthError::Request(_))
        ));
    }
}
