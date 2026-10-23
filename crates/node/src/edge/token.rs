//! The relay token a profile keeps between launches.
//!
//! The account service issues relay tokens that last a week, so a launch
//! that finds one still valid connects to the relay at once instead of
//! asking the account service first. The token is refreshed in the
//! background once it is an hour old, and hourly while connected, so the
//! plan it carries is never more than an hour stale.
//!
//! One private file, `profiles/<id>/relay-token`, written whole. Signing in
//! or out removes it: a token belongs to the account that fetched it.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::identity::atomic_replace_private;
use crate::routing::LinkConnectorToken;

pub const RELAY_TOKEN_FILE: &str = "relay-token";

/// How close to its expiry a kept token is no longer presented: the relay
/// would refuse it before its first refresh came back.
pub const REUSE_MARGIN: Duration =
    Duration::from_millis(daemon_core::links::REFRESH_BEFORE_EXPIRY_MS);

/// The relay token the profile keeps, reused across launches while valid,
/// refreshed in the background once over an hour old and hourly while
/// connected. The relay it was issued for comes with it: a launch that
/// reuses the token dials that relay without asking where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredToken {
    pub token: String,
    pub expires_at: SystemTime,
    pub fetched_at: SystemTime,
    pub tier: crate::Tier,
    pub host: String,
    pub port: u16,
}

impl StoredToken {
    /// Whether a launch at `now` may present it.
    pub fn usable_at(&self, now: SystemTime) -> bool {
        self.expires_at > now + REUSE_MARGIN
    }

    pub(crate) fn connector_token(&self) -> LinkConnectorToken {
        LinkConnectorToken {
            token: self.token.clone(),
            expires_at: self.expires_at,
            fetched_at: self.fetched_at,
            tier: self.tier,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    token: String,
    expires_at_ms: u64,
    fetched_at_ms: u64,
    tier: crate::Tier,
    host: String,
    port: u16,
}

fn ms(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

fn time(ms: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
}

/// The token the profile keeps, if any. A file that does not read is no
/// token: the next launch fetches one and writes it over.
pub fn load_token(profile: &Path) -> io::Result<Option<StoredToken>> {
    let bytes = match std::fs::read(profile.join(RELAY_TOKEN_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
        tracing::warn!("the kept relay token does not read; a fresh one will be fetched");
        return Ok(None);
    };
    Ok(Some(StoredToken {
        token: record.token,
        expires_at: time(record.expires_at_ms),
        fetched_at: time(record.fetched_at_ms),
        tier: record.tier,
        host: record.host,
        port: record.port,
    }))
}

pub fn store_token(profile: &Path, token: &StoredToken) -> io::Result<()> {
    let record = Record {
        token: token.token.clone(),
        expires_at_ms: ms(token.expires_at),
        fetched_at_ms: ms(token.fetched_at),
        tier: token.tier,
        host: token.host.clone(),
        port: token.port,
    };
    let bytes = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
    atomic_replace_private(&profile.join(RELAY_TOKEN_FILE), &bytes).map_err(io::Error::other)
}

/// Forgets the kept token, as a sign-in, a sign-out or a refusal must.
pub fn forget_token(profile: &Path) -> io::Result<()> {
    match std::fs::remove_file(profile.join(RELAY_TOKEN_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(fetched_at: SystemTime, lasts: Duration) -> StoredToken {
        StoredToken {
            token: "relay-ada-1".to_owned(),
            expires_at: fetched_at + lasts,
            fetched_at,
            tier: crate::Tier::Pro,
            host: "relay.example".to_owned(),
            port: 4433,
        }
    }

    #[test]
    fn a_stored_token_reads_back_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), None);
        let fetched = time(1_700_000_000_123);
        let kept = token(fetched, Duration::from_secs(7 * 24 * 3600));
        store_token(dir.path(), &kept).unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), Some(kept));
        forget_token(dir.path()).unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), None);
        forget_token(dir.path()).unwrap();
    }

    #[test]
    fn a_stored_token_that_does_not_read_is_no_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RELAY_TOKEN_FILE), b"{\"token\":").unwrap();
        assert_eq!(load_token(dir.path()).unwrap(), None);
    }

    #[test]
    fn an_expired_or_nearly_expired_token_is_never_reused() {
        let fetched = time(1_700_000_000_000);
        let week = token(fetched, Duration::from_secs(7 * 24 * 3600));
        assert!(week.usable_at(fetched + Duration::from_secs(6 * 24 * 3600)));
        assert!(!week.usable_at(week.expires_at));
        assert!(!week.usable_at(week.expires_at + Duration::from_secs(1)));
        assert!(!week.usable_at(week.expires_at - REUSE_MARGIN));
        assert!(week.usable_at(week.expires_at - REUSE_MARGIN - Duration::from_secs(1)));
    }

    #[test]
    fn a_stored_token_presents_its_fetch_time() {
        let fetched = time(1_700_000_000_000);
        let kept = token(fetched, Duration::from_secs(3600));
        let presented = kept.connector_token();
        assert_eq!(presented.fetched_at, fetched);
        assert_eq!(presented.expires_at, kept.expires_at);
        assert_eq!(presented.tier, crate::Tier::Pro);
    }
}
