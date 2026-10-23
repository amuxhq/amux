//! A profile's account: which cloud account the profile is bound to, the
//! credential that signs it in, and whether its connection is paused.
//!
//! One private file, `profiles/<id>/account`, written whole on every change.
//! Its absence is an unbound profile; a binding without a refresh token is a
//! profile that signed out but stays tied to its account; `paused` keeps the
//! credential and holds the cloud link down. Signing in or out also forgets
//! the relay token the profile keeps beside it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::binding::CloudServiceId;
use crate::auth::oauth::{self, OAuthError};
use crate::auth::{AccessToken, AuthError, CredentialProvider};
use crate::identity::atomic_replace_private;

pub const ACCOUNT_FILE: &str = "account";
/// An access token this close to expiry is refreshed before it is handed out.
const ACCESS_TOKEN_MARGIN: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountRecord {
    pub(crate) service: CloudServiceId,
    /// The account's subject at that service: what makes two logins the
    /// same account.
    pub(crate) subject: String,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
    /// The profile picture's URL, as the service said at sign-in.
    #[serde(default)]
    pub(crate) picture: Option<String>,
    pub(crate) bound_at_ms: i64,
    /// Absent after a sign-out.
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) paused: bool,
    /// The OAuth client the refresh token was issued to, which is the only
    /// one it refreshes under: the CLI's, or the phone's.
    #[serde(default = "cli_client")]
    pub(crate) client_id: String,
}

fn cli_client() -> String {
    oauth::CLI_CLIENT_ID.to_owned()
}

/// The account file and the access token minted from it.
pub(crate) struct Account {
    path: PathBuf,
    record: Mutex<Option<AccountRecord>>,
    access: Mutex<Option<AccessToken>>,
    /// One change to the record at a time, a refresh's exchange and save
    /// included: a refresh token may be single use, and a refresh that
    /// saved its rotated token over a sign-out or a pause made meanwhile
    /// would undo it.
    changes: tokio::sync::Mutex<()>,
    /// Called after each change to the record: this host's own entry says
    /// whether it is signed in.
    changed: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Account {
    pub(crate) fn open(profile_dir: &Path) -> io::Result<Arc<Self>> {
        let path = profile_dir.join(ACCOUNT_FILE);
        let record = match std::fs::read(&path) {
            Ok(bytes) => Some(
                serde_json::from_slice(&bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Arc::new(Self {
            path,
            record: Mutex::new(record),
            access: Mutex::new(None),
            changes: tokio::sync::Mutex::new(()),
            changed: std::sync::OnceLock::new(),
        }))
    }

    pub(crate) fn record(&self) -> Option<AccountRecord> {
        self.record.lock().unwrap().clone()
    }

    pub(crate) fn intent(&self) -> wire::Intent {
        match &*self.record.lock().unwrap() {
            None => wire::Intent::Unbound,
            Some(record) if record.paused => wire::Intent::Paused,
            Some(record) if record.refresh_token.is_some() => wire::Intent::Bound,
            Some(_) => wire::Intent::LoggedOut,
        }
    }

    /// Whether the cloud link should run: bound, signed in and not paused.
    pub(crate) fn wants_cloud(&self) -> bool {
        self.intent() == wire::Intent::Bound
    }

    pub(crate) fn service(&self) -> Option<CloudServiceId> {
        self.record
            .lock()
            .unwrap()
            .as_ref()
            .map(|record| record.service.clone())
    }

    fn write(&self, record: Option<AccountRecord>) -> io::Result<()> {
        match &record {
            Some(record) => {
                let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
                atomic_replace_private(&self.path, &bytes).map_err(io::Error::other)?;
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            },
        }
        *self.record.lock().unwrap() = record;
        if let Some(changed) = self.changed.get() {
            changed();
        }
        Ok(())
    }

    /// Calls `changed` after each change to the record from now on.
    pub(crate) fn on_change(&self, changed: Box<dyn Fn() + Send + Sync>) {
        let _ = self.changed.set(changed);
    }

    /// Binds the profile to an account, or signs a bound profile in again.
    pub(crate) async fn bind(&self, record: AccountRecord, access: AccessToken) -> io::Result<()> {
        let _change = self.changes.lock().await;
        self.forget_relay_token()?;
        self.write(Some(record))?;
        *self.access.lock().unwrap() = Some(access);
        Ok(())
    }

    /// Forgets the credential and keeps the binding. A refresh under way
    /// finishes first.
    pub(crate) async fn sign_out(&self) -> io::Result<()> {
        let _change = self.changes.lock().await;
        let record = self.record().map(|record| AccountRecord {
            refresh_token: None,
            ..record
        });
        self.forget_relay_token()?;
        self.write(record)?;
        *self.access.lock().unwrap() = None;
        Ok(())
    }

    /// A relay token belongs to the login that fetched it: signing in or
    /// out forgets it before the record changes.
    fn forget_relay_token(&self) -> io::Result<()> {
        match self.path.parent() {
            Some(profile) => super::token::forget_token(profile),
            None => Ok(()),
        }
    }

    /// A refresh under way finishes first.
    pub(crate) async fn set_paused(&self, paused: bool) -> io::Result<()> {
        let _change = self.changes.lock().await;
        let record = self
            .record()
            .ok_or_else(|| io::Error::other("the profile is not bound to an account"))?;
        self.write(Some(AccountRecord { paused, ..record }))
    }

    /// The access token, refreshed first if it is missing or about to
    /// expire. Holds the record still from the exchange to the save.
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        let _change = self.changes.lock().await;
        if let Some(access) = self.access.lock().unwrap().clone()
            && access
                .expires_at
                .is_none_or(|at| at > SystemTime::now() + ACCESS_TOKEN_MARGIN)
        {
            return Ok(access);
        }
        let record = self.record().ok_or(AuthError::Unauthenticated)?;
        let refresh = record
            .refresh_token
            .clone()
            .ok_or(AuthError::Unauthenticated)?;
        let (access, rotated) =
            oauth::refresh_access_token(record.service.as_str(), &record.client_id, &refresh)
                .await
                .map_err(|error| match error {
                    OAuthError::RefreshTokenExpired => AuthError::Unauthenticated,
                    other => AuthError::Provider(other.to_string()),
                })?;
        // Only onto the binding the exchange was made for, still signed in
        // with the token it spent and not paused, and only the token.
        let current = self
            .record()
            .filter(|current| {
                current.service == record.service
                    && current.subject == record.subject
                    && current.refresh_token.as_deref() == Some(refresh.as_str())
                    && !current.paused
            })
            .ok_or(AuthError::Unauthenticated)?;
        if let Some(rotated) = rotated
            && rotated != refresh
        {
            // Written before the access token is used: a refresh token the
            // service rotated is the only one that works from now on.
            self.write(Some(AccountRecord {
                refresh_token: Some(rotated),
                ..current
            }))
            .map_err(|error| AuthError::Provider(error.to_string()))?;
        }
        *self.access.lock().unwrap() = Some(access.clone());
        Ok(access)
    }
}

/// The credential the cloud link presents: an access token minted from the
/// account's refresh token, which the identity service may rotate.
#[derive(Clone)]
pub(crate) struct AccountCredentials(pub(crate) Arc<Account>);

#[async_trait::async_trait]
impl CredentialProvider for AccountCredentials {
    /// The exchange and its save run apart from the caller, so a caller
    /// that stops waiting, such as a cloud link told to stop while it
    /// connects, cannot cancel between the two and lose a rotated token.
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        let account = self.0.clone();
        tokio::spawn(async move { account.access_token().await })
            .await
            .map_err(|error| AuthError::Provider(error.to_string()))?
    }

    fn invalidate(&self, token: &AccessToken) {
        let mut access = self.0.access.lock().unwrap();
        if access
            .as_ref()
            .is_some_and(|cached| cached.bearer == token.bearer)
        {
            *access = None;
        }
    }
}

/// Binds a profile to the account a staged login names: the profile the
/// request names, or the one already bound to that account, or the one
/// profile that holds nothing yet, or a new one. A profile that already
/// holds agents or paired hosts is adopted only when the request says so.
pub(crate) async fn bind(
    installation: &Arc<crate::daemon::Installation>,
    explicit: Option<crate::ProfileId>,
    request: wire::BindProfileRequest,
) -> Result<crate::ProfileId, tonic::Status> {
    use wire::ErrorCode;

    use crate::front_door::failed;

    let service = CloudServiceId::canonicalize(&request.cloud_url)
        .map_err(|error| failed(ErrorCode::InvalidArgument, error.to_string()))?;
    if request.staged_refresh_token.is_empty() {
        return Err(failed(
            ErrorCode::InvalidArgument,
            "the login carries no refresh token",
        ));
    }
    let client_id = if request.client_id.is_empty() {
        cli_client()
    } else {
        request.client_id.clone()
    };
    let (access, rotated) =
        oauth::refresh_access_token(service.as_str(), &client_id, &request.staged_refresh_token)
            .await
            .map_err(|error| failed(ErrorCode::Unauthenticated, error.to_string()))?;
    let refresh = rotated.unwrap_or(request.staged_refresh_token);
    let info = super::binding::fetch_userinfo(&reqwest::Client::new(), service.as_str(), &access)
        .await
        .map_err(|error| failed(ErrorCode::Unavailable, error.to_string()))?;
    let same_account =
        |record: &AccountRecord| record.service == service && record.subject == info.sub;

    let _change = installation.registry_changes.lock().await;
    let hosted = installation
        .hosted
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(id, hosted)| {
            hosted
                .runtime
                .edge()
                .map(|edge| (*id, hosted.position, hosted.runtime.clone(), edge))
        })
        .collect::<Vec<_>>();
    let bound = hosted
        .iter()
        .find(|(_, _, _, edge)| {
            edge.account()
                .record()
                .is_some_and(|record| same_account(&record))
        })
        .map(|(id, ..)| *id);
    let id = match explicit {
        Some(id) => {
            if bound.is_some_and(|bound| bound != id) {
                return Err(failed(
                    ErrorCode::AlreadyExists,
                    "that account is already signed in on another profile",
                ));
            }
            id
        }
        None => match bound {
            Some(id) => id,
            None => {
                let mut unbound = Vec::new();
                for (id, position, runtime, edge) in &hosted {
                    if edge.account().record().is_none()
                        && pristine(runtime, edge)
                            .await
                            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?
                    {
                        unbound.push((*position, *id));
                    }
                }
                unbound.sort();
                match unbound.first() {
                    Some((_, id)) => *id,
                    None => {
                        let label = info.name.clone().unwrap_or_else(|| "account".to_owned());
                        installation
                            .create(&label)
                            .await
                            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?
                    }
                }
            }
        },
    };
    let (runtime, edge) = {
        let hosted = installation.hosted.lock().unwrap();
        let hosted = hosted
            .get(&id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        let edge = hosted
            .runtime
            .edge()
            .ok_or_else(|| failed(ErrorCode::Unavailable, "the profile is not in service"))?;
        (hosted.runtime.clone(), edge)
    };
    let existing = edge.account().record();
    match &existing {
        Some(record) if !same_account(record) => {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile belongs to another account",
            ));
        }
        None if !request.adopt_non_pristine
            && !pristine(&runtime, &edge)
                .await
                .map_err(|error| failed(ErrorCode::Internal, error.to_string()))? =>
        {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile already holds agents or paired hosts; confirm adopting it",
            ));
        }
        _ => {}
    }
    let record = AccountRecord {
        service,
        subject: info.sub,
        name: info.name,
        email: info.email,
        picture: info.picture,
        bound_at_ms: existing.as_ref().map_or_else(
            || crate::link::run::system_time_ms(SystemTime::now()),
            |record| record.bound_at_ms,
        ),
        refresh_token: Some(refresh),
        paused: existing.is_some_and(|record| record.paused),
        client_id,
    };
    edge.bind_account(record, access)
        .await
        .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
    Ok(id)
}

/// Whether a profile holds nothing an account would adopt: no agents and
/// no paired hosts.
async fn pristine(
    runtime: &crate::runtime::ProfileRuntime,
    edge: &super::Edge,
) -> Result<bool, store::StoreError> {
    Ok(edge.has_no_peers() && runtime.read_rows(|store| store.agents()).await?.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio::sync::{Notify, mpsc};

    use super::*;

    /// A token endpoint that answers each refresh only when released,
    /// rotating the refresh token to `r2`, `r3`, ...
    struct TokenEndpoint {
        url: String,
        arrived: mpsc::UnboundedReceiver<()>,
        release: Arc<Notify>,
    }

    impl TokenEndpoint {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let (arrive, arrived) = mpsc::unbounded_channel();
            let release = Arc::new(Notify::new());
            let released = release.clone();
            tokio::spawn(async move {
                let mut n = 1;
                loop {
                    n += 1;
                    let (mut socket, _) = listener.accept().await.unwrap();
                    read_request(&mut socket).await;
                    arrive.send(()).unwrap();
                    released.notified().await;
                    let body = format!(
                        r#"{{"access_token":"a{n}","token_type":"bearer","expires_in":3600,"refresh_token":"r{n}"}}"#
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            });
            Self {
                url,
                arrived,
                release,
            }
        }

        /// Waits for a refresh to reach the endpoint and be held there.
        async fn held(&mut self) {
            self.arrived.recv().await.unwrap();
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    return;
                }
            }
        }
    }

    /// Bound, signed in with `r1`, holding an access token that has
    /// expired, so the next use refreshes.
    async fn signed_in(dir: &Path, endpoint: &TokenEndpoint) -> Arc<Account> {
        let account = Account::open(dir).unwrap();
        let record = AccountRecord {
            service: CloudServiceId::canonicalize(&endpoint.url).unwrap(),
            subject: "subject".into(),
            name: None,
            email: None,
            picture: None,
            bound_at_ms: 1,
            refresh_token: Some("r1".into()),
            paused: false,
            client_id: cli_client(),
        };
        let expired = AccessToken {
            bearer: "a1".into(),
            expires_at: Some(SystemTime::now() - Duration::from_secs(1)),
            tier: None,
        };
        account.bind(record, expired).await.unwrap();
        account
    }

    #[tokio::test]
    async fn signing_in_or_out_forgets_the_kept_relay_token_and_a_pause_keeps_it() {
        use crate::edge::token::{StoredToken, load_token, store_token};
        let dir = tempfile::tempdir().unwrap();
        let endpoint = TokenEndpoint::start().await;
        // Whole seconds: the file keeps milliseconds.
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let kept = StoredToken {
            token: "relay-1".into(),
            expires_at: fetched_at + Duration::from_secs(3600),
            fetched_at,
            tier: crate::Tier::Pro,
            host: "relay.example".into(),
            port: 4433,
        };
        store_token(dir.path(), &kept).unwrap();
        let account = signed_in(dir.path(), &endpoint).await;
        assert_eq!(load_token(dir.path()).unwrap(), None, "signing in");

        store_token(dir.path(), &kept).unwrap();
        account.set_paused(true).await.unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), Some(kept), "pausing");
        account.sign_out().await.unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), None, "signing out");
    }

    #[tokio::test]
    async fn a_sign_out_during_a_refresh_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = TokenEndpoint::start().await;
        let account = signed_in(dir.path(), &endpoint).await;
        let refreshing = tokio::spawn(AccountCredentials(account.clone()).access_token_owned());
        endpoint.held().await;
        let signing_out = tokio::spawn({
            let account = account.clone();
            async move { account.sign_out().await }
        });
        endpoint.release();
        let _ = refreshing.await.unwrap();
        signing_out.await.unwrap().unwrap();

        let reopened = Account::open(dir.path()).unwrap().record().unwrap();
        assert_eq!(reopened.refresh_token, None, "still signed out");
    }

    #[tokio::test]
    async fn a_pause_during_a_refresh_survives_a_restart_with_the_rotated_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = TokenEndpoint::start().await;
        let account = signed_in(dir.path(), &endpoint).await;
        let refreshing = tokio::spawn(AccountCredentials(account.clone()).access_token_owned());
        endpoint.held().await;
        let pausing = tokio::spawn({
            let account = account.clone();
            async move { account.set_paused(true).await }
        });
        endpoint.release();
        refreshing.await.unwrap().unwrap();
        pausing.await.unwrap().unwrap();

        let reopened = Account::open(dir.path()).unwrap().record().unwrap();
        assert!(reopened.paused, "still paused");
        assert_eq!(
            reopened.refresh_token.as_deref(),
            Some("r2"),
            "the rotated token, the only one that works now"
        );
    }

    #[tokio::test]
    async fn a_refresh_whose_caller_stops_waiting_still_saves_the_rotated_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = TokenEndpoint::start().await;
        let account = signed_in(dir.path(), &endpoint).await;
        let credentials = AccountCredentials(account.clone());
        let refreshing = tokio::spawn({
            let credentials = credentials.clone();
            async move { credentials.access_token().await }
        });
        endpoint.held().await;
        refreshing.abort();
        endpoint.release();
        // Served from the refresh the abandoned call started, once it saved.
        let access = credentials.access_token().await.unwrap();
        assert_eq!(access.bearer, "a2");

        let reopened = Account::open(dir.path()).unwrap().record().unwrap();
        assert_eq!(reopened.refresh_token.as_deref(), Some("r2"));
    }

    impl AccountCredentials {
        async fn access_token_owned(self) -> Result<AccessToken, AuthError> {
            self.access_token().await
        }
    }
}
