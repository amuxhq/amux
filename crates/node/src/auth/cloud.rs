//! Cloud API client for amux.
//!
//! Fetches cloud routing connection details from the cloud API.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use thiserror::Error;

use crate::auth::{AccessToken, AuthError, CredentialProvider};

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("Not authenticated - run 'amux init' to authenticate")]
    NotAuthenticated,
    #[error("Connection failed: {0}")]
    Connection(String),
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("Cloud request rejected: {0}")]
    Rejected(String),
}

/// Response from the cloud `/api/connect` endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiConnectResult {
    host: String,
    port: u16,
    token: String,
    expires_at: DateTime<Utc>,
    tier: crate::Tier,
}

#[derive(Debug, Clone)]
pub(crate) struct CloudRoutingConnectionDetails {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) token: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) tier: crate::Tier,
}

impl From<ApiConnectResult> for CloudRoutingConnectionDetails {
    fn from(result: ApiConnectResult) -> Self {
        Self {
            host: result.host,
            port: result.port,
            token: result.token,
            expires_at: result.expires_at,
            tier: result.tier,
        }
    }
}

/// Fetch connection details from the cloud API using the consumer-owned token.
///
/// Shared by both initial connection and connection-JWT refresh. The
/// `CredentialProvider` owns access-token refresh and persistence; this layer
/// only presents its bearer to `/api/connect`.
async fn refresh_and_fetch_connection(
    cloud_url: &str,
    credentials: &dyn CredentialProvider,
) -> std::result::Result<ApiConnectResult, CloudError> {
    let access_token = credentials
        .access_token()
        .await
        .map_err(cloud_error_from_auth)?;
    match fetch_connection(cloud_url, credentials, &access_token).await {
        Ok(connection) => Ok(connection),
        Err(CloudError::Auth(_)) => {
            let access_token = credentials
                .access_token()
                .await
                .map_err(cloud_error_from_auth)?;
            fetch_connection(cloud_url, credentials, &access_token).await
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn fetch_routing_connection_details(
    cloud_url: &str,
    credentials: &dyn CredentialProvider,
) -> std::result::Result<CloudRoutingConnectionDetails, CloudError> {
    refresh_and_fetch_connection(cloud_url, credentials)
        .await
        .map(Into::into)
}

async fn fetch_connection(
    cloud_url: &str,
    credentials: &dyn CredentialProvider,
    access_token: &AccessToken,
) -> std::result::Result<ApiConnectResult, CloudError> {
    let response = reqwest::Client::new()
        .get(format!("{cloud_url}/api/connect"))
        .bearer_auth(&access_token.bearer)
        .send()
        .await
        .map_err(|error| CloudError::Connection(error.to_string()))?;

    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .map_err(|error| CloudError::Connection(error.to_string()));
    }

    let body = response.text().await.unwrap_or_default();
    let error = cloud_error_from_response(status, &body);
    if matches!(error, CloudError::Auth(_)) {
        credentials.invalidate(access_token);
    }
    Err(error)
}

fn cloud_error_from_response(status: reqwest::StatusCode, body: &str) -> CloudError {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return CloudError::Auth("invalid credentials".to_string());
    }

    let detail = format!("API returned {status}: {body}");
    if status.is_client_error()
        && status != reqwest::StatusCode::REQUEST_TIMEOUT
        && status != reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        CloudError::Rejected(detail)
    } else {
        CloudError::Connection(detail)
    }
}

fn cloud_error_from_auth(error: AuthError) -> CloudError {
    match error {
        AuthError::Unauthenticated | AuthError::AccountMismatch => CloudError::NotAuthenticated,
        AuthError::Provider(message) => CloudError::Connection(message),
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    struct TestCredentials;

    #[async_trait::async_trait]
    impl CredentialProvider for TestCredentials {
        async fn access_token(&self) -> Result<AccessToken, AuthError> {
            unreachable!("fetch_connection receives its access token directly")
        }

        fn invalidate(&self, _token: &AccessToken) {}
    }

    #[test]
    fn connect_response_classification_distinguishes_terminal_failures() {
        assert!(matches!(
            cloud_error_from_response(reqwest::StatusCode::UNAUTHORIZED, ""),
            CloudError::Auth(_)
        ));
        assert!(matches!(
            cloud_error_from_response(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "try again",),
            CloudError::Connection(_)
        ));

        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(matches!(
                cloud_error_from_response(status, "try again"),
                CloudError::Connection(_)
            ));
        }

        for body in ["", "not json", r#"{"error":"forbidden"}"#] {
            let error = cloud_error_from_response(reqwest::StatusCode::FORBIDDEN, body);
            match error {
                CloudError::Rejected(message) => {
                    assert!(message.contains("403 Forbidden"));
                    assert!(message.contains(body));
                }
                other => panic!("unexpected 403 classification for {body:?}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn network_failures_remain_retriable_connection_errors() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let access_token = AccessToken {
            bearer: "test".to_string(),
            expires_at: Some(SystemTime::now()),
            tier: None,
        };

        let error = fetch_connection(
            &format!("http://{address}"),
            &TestCredentials,
            &access_token,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CloudError::Connection(_)));
    }

    #[test]
    fn provider_errors_remain_retriable_cloud_connection_errors() {
        assert!(matches!(
            cloud_error_from_auth(AuthError::Provider("temporary failure".to_string())),
            CloudError::Connection(_)
        ));
        assert!(matches!(
            cloud_error_from_auth(AuthError::Unauthenticated),
            CloudError::NotAuthenticated
        ));
    }
}
