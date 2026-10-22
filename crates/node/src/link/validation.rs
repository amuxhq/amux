//! Address-validation tokens.
//!
//! A listener answers a QUIC Initial from an address it has not validated
//! with a Retry, which costs the dialler a round trip. Once a connection's
//! address is validated the listener sends the client tokens in NEW_TOKEN
//! frames; the client keeps them per server and presents one on its next
//! connection from that address, which the listener then takes as
//! validated and answers at once. A token is honoured once, and only from
//! the IP address it was given to.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use std::{fmt, fs};

use bytes::Bytes;

use super::resumption::file_name;
use crate::HostId;

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

/// The directory a profile keeps the tokens servers gave it in, one file
/// per host and, beneath [`RELAY_TOKENS_DIR`], per relay name.
pub const CLIENT_TOKENS_DIR: &str = "client-tokens";
const RELAY_TOKENS_DIR: &str = "relay";

/// The server a dial's tokens are kept for. A host and the relay are kept
/// by who they are, so a relaunched process dialling them again presents
/// the token its last one was given; any other server by its address, for
/// the process only.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Server {
    Host(HostId),
    Relay(String),
    Address(SocketAddr),
}

impl Server {
    fn file(&self, dir: &Path) -> Option<PathBuf> {
        match self {
            Self::Host(host) => Some(dir.join(host.to_string())),
            Self::Relay(name) => Some(dir.join(RELAY_TOKENS_DIR).join(file_name(name))),
            Self::Address(_) => None,
        }
    }
}

/// The tokens the servers a profile dials have given it, by server. quinn
/// keys tokens by server name, which every paired host shares, so each
/// dial is handed a store for its one server.
#[derive(Default)]
pub(crate) struct ServerTokens {
    servers: Mutex<Servers>,
    /// Where hosts' and the relay's tokens are kept across processes; in
    /// memory when none.
    dir: Option<PathBuf>,
}

#[derive(Default)]
struct Servers {
    tokens: HashMap<Server, VecDeque<Bytes>>,
    /// The servers whose saved tokens this process has read.
    read: HashSet<Server>,
}

impl fmt::Debug for ServerTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTokens")
            .field("dir", &self.dir)
            .field("servers", &lock(&self.servers).tokens.len())
            .finish()
    }
}

impl ServerTokens {
    /// Keeps hosts' and the relay's tokens in `dir`.
    pub(crate) fn keeping(dir: PathBuf) -> Self {
        Self {
            servers: Mutex::default(),
            dir: Some(dir),
        }
    }

    /// The store a dial to `server` keeps its tokens in.
    pub(crate) fn for_server(self: &Arc<Self>, server: Server) -> Arc<dyn quinn::TokenStore> {
        Arc::new(ForServer {
            tokens: self.clone(),
            server,
        })
    }

    fn file(&self, server: &Server) -> Option<PathBuf> {
        server.file(self.dir.as_deref()?)
    }

    /// `server`'s tokens, with those an earlier process saved read in the
    /// first time.
    fn of<'a>(&self, servers: &'a mut Servers, server: &Server) -> Option<&'a mut VecDeque<Bytes>> {
        if servers.read.insert(server.clone())
            && let Some(file) = self.file(server)
            && let Ok(bytes) = fs::read(&file)
        {
            let saved = decode(&bytes);
            if !saved.is_empty() {
                servers.tokens.insert(server.clone(), saved);
            }
        }
        servers.tokens.get_mut(server)
    }

    /// Writes `server`'s tokens over its file, or deletes the file when
    /// none are left. Not synced: a token a crash loses costs one Retry.
    fn save(&self, server: &Server, tokens: Option<&VecDeque<Bytes>>) {
        let Some(file) = self.file(server) else {
            return;
        };
        let written = match tokens.filter(|tokens| !tokens.is_empty()) {
            None => match fs::remove_file(&file) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
            Some(tokens) => write_replacing(&file, &encode(tokens)),
        };
        if let Err(error) = written {
            tracing::debug!(%error, "address-validation tokens could not be saved");
        }
    }
}

struct ForServer {
    tokens: Arc<ServerTokens>,
    server: Server,
}

impl quinn::TokenStore for ForServer {
    fn insert(&self, _: &str, token: Bytes) {
        let store = &self.tokens;
        let mut servers = lock(&store.servers);
        if store.of(&mut servers, &self.server).is_none()
            && servers.tokens.len() >= TOKEN_SERVERS
            && let Some(evicted) = servers.tokens.keys().next().cloned()
        {
            servers.tokens.remove(&evicted);
            store.save(&evicted, None);
        }
        let tokens = servers.tokens.entry(self.server.clone()).or_default();
        if tokens.len() >= TOKENS_PER_SERVER {
            tokens.pop_front();
        }
        tokens.push_back(token);
        store.save(&self.server, Some(tokens));
    }

    /// The newest token, gone from disk before the dial presents it, as a
    /// token is honoured once.
    fn take(&self, _: &str) -> Option<Bytes> {
        let store = &self.tokens;
        let mut servers = lock(&store.servers);
        let tokens = store.of(&mut servers, &self.server)?;
        let token = tokens.pop_back();
        store.save(&self.server, Some(tokens));
        if tokens.is_empty() {
            servers.tokens.remove(&self.server);
        }
        token
    }
}

/// Each token behind its two-byte length.
fn encode(tokens: &VecDeque<Bytes>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for token in tokens {
        let Ok(len) = u16::try_from(token.len()) else {
            continue;
        };
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(token);
    }
    bytes
}

/// The tokens in `bytes`, as many as are whole, the newest
/// [`TOKENS_PER_SERVER`].
fn decode(mut bytes: &[u8]) -> VecDeque<Bytes> {
    let mut tokens = VecDeque::new();
    while let [high, low, rest @ ..] = bytes {
        let len = usize::from(u16::from_be_bytes([*high, *low]));
        let Some((token, rest)) = rest.split_at_checked(len) else {
            break;
        };
        if tokens.len() >= TOKENS_PER_SERVER {
            tokens.pop_front();
        }
        tokens.push_back(Bytes::copy_from_slice(token));
        bytes = rest;
    }
    tokens
}

/// Replaces `path` with `bytes` all at once, readable by this user only.
fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a token file needs a parent directory"))?;
    fs::create_dir_all(dir)?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
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
        let relay = Server::Address("192.0.2.1:443".parse().unwrap());
        let desk = Server::Address("192.0.2.2:7000".parse().unwrap());
        tokens
            .for_server(relay.clone())
            .insert("amux-device.local", Bytes::from_static(b"relay"));
        assert_eq!(tokens.for_server(desk).take("amux-device.local"), None);
        assert_eq!(
            tokens.for_server(relay.clone()).take("another name"),
            Some(Bytes::from_static(b"relay"))
        );
        assert_eq!(tokens.for_server(relay).take("amux-device.local"), None);
    }

    #[test]
    fn validation_tokens_kept_on_disk_are_presented_by_the_next_process_once() {
        let dir = tempfile::tempdir().unwrap();
        let desk = Server::Host(HostId::new_v4());
        let relay = Server::Relay("relay.example".to_owned());
        let lan = Server::Address("192.0.2.2:7000".parse().unwrap());
        let process = || Arc::new(ServerTokens::keeping(dir.path().to_owned()));

        let first = process();
        for server in [&desk, &relay, &lan] {
            for token in [&b"older"[..], b"newer"] {
                first
                    .for_server(server.clone())
                    .insert("", Bytes::copy_from_slice(token));
            }
        }

        let second = process();
        assert_eq!(
            second.for_server(desk.clone()).take(""),
            Some(Bytes::from_static(b"newer"))
        );
        assert_eq!(
            second.for_server(relay.clone()).take(""),
            Some(Bytes::from_static(b"newer"))
        );
        assert_eq!(
            second.for_server(lan.clone()).take(""),
            None,
            "a server known only by its address is kept in memory"
        );

        // What the second process took is gone from disk for the third.
        let third = process();
        assert_eq!(
            third.for_server(desk.clone()).take(""),
            Some(Bytes::from_static(b"older"))
        );
        assert_eq!(third.for_server(desk.clone()).take(""), None);
        assert_eq!(
            process().for_server(desk).take(""),
            None,
            "each token is presented once"
        );
        assert_eq!(
            process().for_server(relay).take(""),
            Some(Bytes::from_static(b"older"))
        );
    }
}
