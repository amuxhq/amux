//! Address-validation tokens.
//!
//! A listener answers a QUIC Initial from an address it has not validated
//! with a Retry, which costs the dialler a round trip. Once a connection's
//! address is validated the listener sends the client tokens in NEW_TOKEN
//! frames; the client keeps them per server and presents one on its next
//! connection from that address, which the listener then takes as
//! validated and answers at once. A token is honoured once, and only from
//! the IP address it was given to.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use bytes::Bytes;

/// How many tokens a listener sends each client it validates: one for the
/// next connection and one spare for the one after a lost attempt.
const TOKENS_SENT: u32 = 2;
/// How long a token is honoured.
const TOKEN_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The most used tokens a listener remembers; past it a token is refused
/// as already used, which costs its client one Retry.
const USED_TOKENS: usize = 1 << 20;
/// How often a listener forgets the used tokens past their lifetime.
const PRUNE_EVERY: Duration = Duration::from_secs(60);
/// What a client keeps: tokens for so many servers, so many per server.
const TOKEN_SERVERS: usize = 256;
const TOKENS_PER_SERVER: usize = 2;

/// Makes a listener send validated clients tokens and honour each once.
pub(crate) fn issue_tokens(config: &mut quinn::ServerConfig) {
    let mut tokens = quinn::ValidationTokenConfig::default();
    tokens
        .lifetime(TOKEN_LIFETIME)
        .log(Arc::new(UsedTokens::default()))
        .sent(TOKENS_SENT);
    config.validation_token_config(tokens);
}

/// The tokens a listener has seen used, until each would have expired.
#[derive(Default)]
struct UsedTokens(Mutex<Used>);

#[derive(Default)]
struct Used {
    /// Each used token's nonce, with when it stops being honoured anyway.
    until: HashMap<u128, SystemTime>,
    pruned: Option<SystemTime>,
}

impl quinn::TokenLog for UsedTokens {
    fn check_and_insert(
        &self,
        nonce: u128,
        issued: SystemTime,
        lifetime: Duration,
    ) -> Result<(), quinn::TokenReuseError> {
        let mut used = lock(&self.0);
        let now = SystemTime::now();
        if used
            .pruned
            .is_none_or(|at| now.duration_since(at).unwrap_or_default() >= PRUNE_EVERY)
        {
            used.until.retain(|_, until| *until > now);
            used.pruned = Some(now);
        }
        if used.until.contains_key(&nonce) || used.until.len() >= USED_TOKENS {
            return Err(quinn::TokenReuseError);
        }
        used.until.insert(nonce, issued + lifetime);
        Ok(())
    }
}

/// The tokens the servers a profile dials have given it, by the server's
/// address. quinn keys tokens by server name, which every paired host
/// shares, so each dial is handed a store for its one server.
#[derive(Default)]
pub(crate) struct ServerTokens(Mutex<HashMap<SocketAddr, VecDeque<Bytes>>>);

impl fmt::Debug for ServerTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTokens")
            .field("servers", &lock(&self.0).len())
            .finish()
    }
}

impl ServerTokens {
    /// The store a dial to `server` keeps its tokens in.
    pub(crate) fn for_server(self: &Arc<Self>, server: SocketAddr) -> Arc<dyn quinn::TokenStore> {
        Arc::new(ForServer {
            tokens: self.clone(),
            server,
        })
    }
}

struct ForServer {
    tokens: Arc<ServerTokens>,
    server: SocketAddr,
}

impl quinn::TokenStore for ForServer {
    fn insert(&self, _: &str, token: Bytes) {
        let mut servers = lock(&self.tokens.0);
        if !servers.contains_key(&self.server)
            && servers.len() >= TOKEN_SERVERS
            && let Some(evicted) = servers.keys().next().copied()
        {
            servers.remove(&evicted);
        }
        let tokens = servers.entry(self.server).or_default();
        if tokens.len() >= TOKENS_PER_SERVER {
            tokens.pop_front();
        }
        tokens.push_back(token);
    }

    fn take(&self, _: &str) -> Option<Bytes> {
        let mut servers = lock(&self.tokens.0);
        let tokens = servers.get_mut(&self.server)?;
        let token = tokens.pop_back();
        if tokens.is_empty() {
            servers.remove(&self.server);
        }
        token
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use quinn::TokenLog as _;

    use super::*;

    #[test]
    fn a_used_token_is_refused_the_second_time() {
        let used = UsedTokens::default();
        let issued = SystemTime::now();
        assert!(used.check_and_insert(7, issued, TOKEN_LIFETIME).is_ok());
        assert!(used.check_and_insert(7, issued, TOKEN_LIFETIME).is_err());
        assert!(used.check_and_insert(8, issued, TOKEN_LIFETIME).is_ok());
    }

    #[test]
    fn tokens_are_kept_per_server_whatever_name_the_dial_used() {
        let tokens = Arc::new(ServerTokens::default());
        let relay: SocketAddr = "192.0.2.1:443".parse().unwrap();
        let desk: SocketAddr = "192.0.2.2:7000".parse().unwrap();
        tokens
            .for_server(relay)
            .insert("amux-device.local", Bytes::from_static(b"relay"));
        assert_eq!(tokens.for_server(desk).take("amux-device.local"), None);
        assert_eq!(
            tokens.for_server(relay).take("another name"),
            Some(Bytes::from_static(b"relay"))
        );
        assert_eq!(tokens.for_server(relay).take("amux-device.local"), None);
    }
}
