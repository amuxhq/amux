//! The installation's profiles as a person manages them: listed and
//! watched, created, renamed and deleted, signed in and out. The
//! front door serves these on its socket; an embedder that serves no
//! sockets calls them here.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use tokio::sync::watch;
use wire::ErrorCode;

use crate::ProfileId;
use crate::daemon::{Hosted, Installation};
use crate::edge::{Cloud, Edge, account};
use crate::profiles::{self, PROFILE_SOCKET};
use crate::runtime::ProfileRuntime;

/// How long a profile's removal waits for its store to close before it
/// removes the profile's files.
const PROFILE_CLOSES_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// A hosted profile as the registry lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileState {
    pub id: ProfileId,
    pub label: String,
    /// Where the profile's client socket is when the daemon serves sockets.
    pub socket: PathBuf,
    /// The account it is bound to, kept after a sign-out; none for a
    /// profile nobody has signed in on.
    pub account: Option<AccountState>,
    /// Its standing with that account; unbound for a profile not in
    /// service.
    pub cloud: Cloud,
}

/// The account a profile is bound to, as its service described it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountState {
    /// The account's subject at its service.
    pub subject: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub picture: Option<String>,
}

/// A sign-in to bind a profile with: the refresh token the client's login
/// obtained, from the OAuth client it was issued to.
pub struct SignIn {
    /// The profile to bind; none lets the registry pick one.
    pub profile: Option<ProfileId>,
    pub cloud_url: String,
    pub refresh_token: String,
    /// The OAuth client the token refreshes under; empty is the CLI's.
    pub client_id: String,
    /// Whether a profile that already holds agents or paired hosts is
    /// adopted.
    pub adopt: bool,
}

/// Why the registry refused.
#[derive(Debug, thiserror::Error)]
pub enum ProfileRefusal {
    #[error("the daemon is shutting down")]
    Stopping,
    #[error("no profile {0}")]
    NoProfile(ProfileId),
    #[error("the profile is not in service")]
    NotInService,
    #[error("a profile needs a name")]
    NoName,
    #[error("another profile is named {0}")]
    NameTaken(String),
    #[error("an installation keeps at least one profile")]
    LastProfile,
    #[error("the profile has running agents; stop them first")]
    AgentsRunning,
    #[error("{0}")]
    BadService(String),
    #[error("the login carries no refresh token")]
    NoRefreshToken,
    /// The identity service turned the login's token down.
    #[error("{0}")]
    Rejected(String),
    /// The account service could not be asked who the login is.
    #[error("{0}")]
    Unreachable(String),
    #[error("that account is already signed in on another profile")]
    AccountElsewhere,
    #[error("the profile belongs to another account")]
    OtherAccount,
    #[error("the profile already holds agents or paired hosts; confirm adopting it")]
    HoldsData,
    #[error("{0}")]
    Failed(String),
}

impl ProfileRefusal {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::Stopping | Self::NotInService | Self::Unreachable(_) => ErrorCode::Unavailable,
            Self::NoProfile(_) => ErrorCode::NotFound,
            Self::NoName | Self::BadService(_) | Self::NoRefreshToken => ErrorCode::InvalidArgument,
            Self::NameTaken(_) | Self::AccountElsewhere => ErrorCode::AlreadyExists,
            Self::LastProfile | Self::AgentsRunning | Self::OtherAccount | Self::HoldsData => {
                ErrorCode::FailedPrecondition
            }
            Self::Rejected(_) => ErrorCode::Unauthenticated,
            Self::Failed(_) => ErrorCode::Internal,
        };
        crate::grpc::wire_error(code, self.to_string())
    }

    fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }

    /// Why a sign-in failed, as analytics reads the refusal.
    fn sign_in_failure(&self) -> analytics::SignInFailure {
        use analytics::SignInFailure;
        match self {
            Self::Rejected(_) => SignInFailure::Rejected,
            Self::Unreachable(_) => SignInFailure::Unreachable,
            Self::AccountElsewhere => SignInFailure::AccountElsewhere,
            Self::OtherAccount | Self::HoldsData => SignInFailure::ProfileConflict,
            _ => SignInFailure::Other,
        }
    }
}

/// A hosted profile as the registry lists it.
pub(crate) fn state(hosted: &Hosted) -> ProfileState {
    let edge = hosted.runtime.edge();
    ProfileState {
        id: hosted.entry.id,
        label: hosted.entry.label.clone(),
        socket: hosted.runtime.dir().join(PROFILE_SOCKET),
        account: edge
            .as_ref()
            .and_then(|edge| edge.account().record())
            .map(|record| AccountState {
                subject: record.subject,
                name: record.name,
                email: record.email,
                picture: record.picture,
            }),
        cloud: edge.map_or(Cloud::Unbound, |edge| edge.cloud()),
    }
}

/// The installation's profiles, for the front door and for an embedder
/// that calls them in process.
#[derive(Clone)]
pub struct Registry {
    installation: Weak<Installation>,
}

impl Registry {
    pub(crate) fn new(installation: Weak<Installation>) -> Registry {
        Registry { installation }
    }

    pub(crate) fn installation(&self) -> Result<Arc<Installation>, ProfileRefusal> {
        self.installation.upgrade().ok_or(ProfileRefusal::Stopping)
    }

    /// Every hosted profile, oldest first.
    pub fn list(&self) -> Result<Vec<ProfileState>, ProfileRefusal> {
        Ok(Self::listed(&*self.installation()?))
    }

    fn listed(installation: &Installation) -> Vec<ProfileState> {
        let hosted = installation.hosted.lock().unwrap();
        let mut hosted: Vec<&Hosted> = hosted.values().collect();
        hosted.sort_by_key(|hosted| (hosted.entry.created_ms, hosted.entry.id));
        hosted.into_iter().map(state).collect()
    }

    /// One hosted profile.
    pub fn profile(&self, id: ProfileId) -> Result<ProfileState, ProfileRefusal> {
        Self::one(&*self.installation()?, id)
    }

    fn one(installation: &Installation, id: ProfileId) -> Result<ProfileState, ProfileRefusal> {
        installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(state)
            .ok_or(ProfileRefusal::NoProfile(id))
    }

    /// A hosted profile's runtime.
    pub(crate) fn runtime(&self, id: ProfileId) -> Result<Arc<ProfileRuntime>, ProfileRefusal> {
        Ok(self
            .installation()?
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .ok_or(ProfileRefusal::NoProfile(id))?
            .runtime
            .clone())
    }

    /// A hosted profile's network edge.
    pub fn edge(&self, id: ProfileId) -> Result<Arc<Edge>, ProfileRefusal> {
        Self::edge_of(&*self.installation()?, id)
    }

    fn edge_of(installation: &Installation, id: ProfileId) -> Result<Arc<Edge>, ProfileRefusal> {
        installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .ok_or(ProfileRefusal::NoProfile(id))?
            .runtime
            .edge()
            .ok_or(ProfileRefusal::NotInService)
    }

    /// Rings whenever the profile list moves from now on; the list is
    /// read afresh with [`Registry::list`].
    pub fn subscribe(&self) -> Result<watch::Receiver<()>, ProfileRefusal> {
        Ok(self.installation()?.moved.subscribe())
    }

    /// A new profile nobody has signed in on, named `label` or "profile".
    pub async fn create(&self, label: Option<&str>) -> Result<ProfileState, ProfileRefusal> {
        let installation = self.installation()?;
        let label = label
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or("profile");
        let _change = installation.profile_changes.lock().await;
        let id = installation
            .create(label)
            .await
            .map_err(ProfileRefusal::failed)?;
        Self::one(&installation, id)
    }

    /// Names a profile afresh; no two profiles share a name.
    pub async fn rename(&self, id: ProfileId, label: &str) -> Result<ProfileState, ProfileRefusal> {
        let installation = self.installation()?;
        let label = label.trim();
        if label.is_empty() {
            return Err(ProfileRefusal::NoName);
        }
        let _change = installation.profile_changes.lock().await;
        let profiles = profiles::list(&installation.data_dir).map_err(ProfileRefusal::failed)?;
        if profiles
            .iter()
            .any(|entry| entry.id != id && entry.label == label)
        {
            return Err(ProfileRefusal::NameTaken(label.to_owned()));
        }
        let mut entry = profiles
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or(ProfileRefusal::NoProfile(id))?;
        entry.label = label.to_owned();
        profiles::write_entry(&installation.data_dir, &entry).map_err(ProfileRefusal::failed)?;
        let renamed = {
            let mut hosted = installation.hosted.lock().unwrap();
            let hosted = hosted.get_mut(&id).ok_or(ProfileRefusal::NoProfile(id))?;
            hosted.entry = entry;
            state(hosted)
        };
        installation.publish();
        Ok(renamed)
    }

    /// Deletes a profile: its key, the machines it trusts and its store.
    /// The installation keeps at least one, and a profile with running
    /// agents stays.
    pub async fn delete(&self, id: ProfileId) -> Result<(), ProfileRefusal> {
        let installation = self.installation()?;
        let _change = installation.profile_changes.lock().await;
        let profiles = profiles::list(&installation.data_dir).map_err(ProfileRefusal::failed)?;
        if !profiles.iter().any(|entry| entry.id == id) {
            return Err(ProfileRefusal::NoProfile(id));
        }
        if profiles.len() == 1 {
            return Err(ProfileRefusal::LastProfile);
        }
        let runtime = installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(|hosted| hosted.runtime.clone());
        if runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.live().is_empty())
        {
            return Err(ProfileRefusal::AgentsRunning);
        }
        let hosted = installation.hosted.lock().unwrap().remove(&id);
        let closed = runtime.as_ref().map(|runtime| runtime.driver().closed());
        if let Some(hosted) = hosted {
            hosted
                .runtime
                .stop_edge(wire::LinkCloseReason::UserShutdown)
                .await;
            // As at shutdown: a dump in flight would hold the runtime and
            // its store open past the directory's removal.
            hosted.runtime.finish_dumps().await;
            hosted.runtime.stop_background().await;
            hosted.runtime.stop_watching().await;
        }
        drop(runtime);
        // Some systems refuse to remove a file still open: the directory
        // goes once the store and the ring are closed, which may be on the
        // core thread after a step under way. A handle held elsewhere past
        // the wait leaves the removal to fail on such a system.
        if let Some(closed) = closed {
            let _ = tokio::time::timeout(PROFILE_CLOSES_WITHIN, closed).await;
        }
        // The profile file goes first, so a removal cut short leaves a
        // directory the next start tidies away rather than a profile.
        profiles::discard(&installation.data_dir, id).map_err(ProfileRefusal::failed)?;
        installation.publish();
        Ok(())
    }

    /// Binds a profile to the account a sign-in names; the relay link comes
    /// up from there. Named, that profile is bound; unnamed, the registry
    /// picks the profile already bound to that account, else one that
    /// holds nothing, else a new one.
    pub async fn bind(&self, sign_in: SignIn) -> Result<ProfileState, ProfileRefusal> {
        let installation = self.installation()?;
        let named = sign_in.profile;
        let bound = account::bind(&installation, sign_in).await;
        // A failure that named no profile is the installation's: its oldest
        // profile records it.
        let analytics = bound
            .as_ref()
            .ok()
            .copied()
            .or(named)
            .and_then(|profile| {
                let hosted = installation.hosted.lock().unwrap();
                hosted
                    .get(&profile)
                    .map(|hosted| hosted.runtime.analytics().clone())
            })
            .or_else(|| installation.first_analytics());
        if let Some(analytics) = analytics {
            analytics.record(match &bound {
                Ok(_) => analytics::Event::SignedIn,
                Err(refusal) => analytics::Event::SignInFailed {
                    reason: refusal.sign_in_failure(),
                },
            });
        }
        let id = bound?;
        installation.publish();
        Self::one(&installation, id)
    }

    /// Signs a profile out: its relay link goes down and its refresh token
    /// is forgotten, but it stays tied to its account.
    pub async fn sign_out(&self, id: ProfileId) -> Result<ProfileState, ProfileRefusal> {
        let installation = self.installation()?;
        let edge = Self::edge_of(&installation, id)?;
        edge.sign_out().await.map_err(ProfileRefusal::failed)?;
        edge.analytics().record(analytics::Event::SignedOut);
        installation.publish();
        Self::one(&installation, id)
    }
}
