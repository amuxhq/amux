//! The two OAuth 2.0 exchanges amux makes with the cloud: signing in a
//! device (RFC 8628) and refreshing an access token (RFC 6749 §6). Both are
//! form posts to the cloud's token service, answered in JSON.

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

/// Run the OAuth 2.0 device authorization flow.
pub async fn run_device_flow(cloud_url: &str) -> Result<String, OAuthError> {
    let http = http_client()?;
    let details: DeviceAuthorization = match post(
        &http,
        &format!("{cloud_url}/connect/deviceauthorization"),
        &[("client_id", CLI_CLIENT_ID), ("scope", DEVICE_SCOPES)],
    )
    .await?
    {
        Answer::Ok(details) => details,
        Answer::Refused(error) => return Err(refused(&error)),
    };

    println!("\nTo authenticate, visit:");
    println!("  {}", details.verification_uri);
    if let Some(complete_uri) = &details.verification_uri_complete {
        println!("\nOr open this URL directly:");
        println!("  {complete_uri}");
    }
    println!("\nAnd enter code: {}", details.user_code);
    println!("\nWaiting for authentication...");

    let token_url = format!("{cloud_url}/connect/token");
    let deadline = Instant::now() + Duration::from_secs(details.expires_in);
    let mut interval = details
        .interval
        .map_or(DEFAULT_INTERVAL, Duration::from_secs);
    loop {
        tokio::time::sleep(interval).await;
        let answer = post::<TokenResponse>(
            &http,
            &token_url,
            &[
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", &details.device_code),
                ("client_id", CLI_CLIENT_ID),
            ],
        )
        .await?;
        match answer {
            Answer::Ok(tokens) => return tokens.refresh_token.ok_or(OAuthError::NoRefreshToken),
            Answer::Refused(error) => match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += DEFAULT_INTERVAL,
                _ => return Err(refused(&error)),
            },
        }
        if Instant::now() >= deadline {
            return Err(OAuthError::Request(
                "the sign-in code expired before it was approved".into(),
            ));
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

        assert_eq!(run_device_flow(&service.url).await.unwrap(), "refresh-1");
        let forms = service.forms.lock().unwrap();
        assert_eq!(forms[0].0, "/connect/deviceauthorization");
        assert_eq!(
            forms[0].1,
            "client_id=cli&scope=openid+profile+email+offline_access+api"
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

        let error = run_device_flow(&service.url).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "OAuth request error: access_denied: the person declined"
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
