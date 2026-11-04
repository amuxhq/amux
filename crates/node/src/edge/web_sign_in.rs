//! A one-time link that signs a browser in to the profile's account and
//! opens a page there: how a computer signed in some other way (from a
//! paired phone) shows its person the account's pages with no second
//! sign-in in the browser.

use std::time::Duration;

use tonic::Status;

use super::Edge;

/// How long the account service has to answer.
const LINK_TIMEOUT: Duration = Duration::from_secs(20);

impl Edge {
    /// A link, good once and for minutes, that signs the browser in to the
    /// profile's account and opens `path` on its account service. `path` is
    /// that service's own: relative, and never another site's.
    pub async fn web_sign_in_link(&self, path: &str) -> Result<String, Status> {
        #[derive(serde::Deserialize)]
        struct Link {
            url: String,
        }
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Err(Status::invalid_argument(
                "the path is a page of the account service, such as /offer",
            ));
        }
        let service = self
            .account_service()
            .filter(|_| self.account.wants_cloud())
            .ok_or_else(|| Status::failed_precondition("the profile is not signed in"))?;
        let token = self
            .access_token()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let answer = reqwest::Client::builder()
            .timeout(LINK_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Status::internal(error.to_string()))?
            .post(format!(
                "{}/api/web-sign-in-links",
                service.trim_end_matches('/')
            ))
            .bearer_auth(&token.bearer)
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let status = answer.status();
        if !status.is_success() {
            return Err(Status::unavailable(format!(
                "the account service answered {status} for a sign-in link"
            )));
        }
        answer
            .json::<Link>()
            .await
            .map(|link| link.url)
            .map_err(|error| Status::unavailable(format!("an unreadable sign-in link: {error}")))
    }
}
