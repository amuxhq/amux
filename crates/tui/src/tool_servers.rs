//! Which tool server failures the footer has told about.
//!
//! A server that needs signing in, or failed to start, usually stays that
//! way across every agent on its host, so telling it in each chat opened
//! is noise. The footer tells each failure once while the terminal client
//! runs; the overview keeps listing it for as long as it fails.

use std::collections::HashSet;

use ui_state::HostId;
use wire::{ToolServer, ToolServerStatus};

/// One way one host's tool server fails. The kind is part of it, so a
/// server that stops needing sign-in and then fails to start is told again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Failure {
    host: HostId,
    name: String,
    needs_auth: bool,
}

/// The failures told, for the whole run.
#[derive(Default)]
pub struct Told(HashSet<Failure>);

/// The failures the open chat has shown, kept while it stays open.
#[derive(Default)]
pub struct Shown(HashSet<Failure>);

impl Told {
    /// Looks at the open chat's tool servers, on `host`. A failure this
    /// chat showed that is now ready is forgotten, so failing again is
    /// news. Recovery counts only in the chat that showed the failure:
    /// agents in different folders can carry different servers under one
    /// name, and one agent's healthy `github` says nothing of another's.
    /// Returns the words for what is newly failing, if anything is.
    pub fn look(
        &mut self,
        host: &HostId,
        servers: &[ToolServer],
        shown: &mut Shown,
    ) -> Option<String> {
        for server in servers {
            let now = failure(host, server);
            // Ready ends a failure, and so does failing another way;
            // starting can be a retry on the way back to the same one.
            if now.is_none() && server.status() != ToolServerStatus::Ready {
                continue;
            }
            shown.0.retain(|was| {
                let over = was.name == server.name && Some(was) != now.as_ref();
                if over {
                    self.0.remove(was);
                }
                !over
            });
        }
        let new: Vec<Failure> = servers
            .iter()
            .filter_map(|server| failure(host, server))
            .inspect(|failure| {
                shown.0.insert(failure.clone());
            })
            .filter(|failure| self.0.insert(failure.clone()))
            .collect();
        words(&new)
    }
}

fn failure(host: &HostId, server: &ToolServer) -> Option<Failure> {
    let needs_auth = match server.status() {
        ToolServerStatus::NeedsAuth => true,
        ToolServerStatus::Failed => false,
        ToolServerStatus::Starting | ToolServerStatus::Ready | ToolServerStatus::Unspecified => {
            return None;
        }
    };
    Some(Failure {
        host: host.clone(),
        name: server.name.clone(),
        needs_auth,
    })
}

/// `tool server github needs sign-in · ctrl+o`, or with several,
/// `tool servers linear and github need sign-in, postgres failed to start
/// · ctrl+o`; ctrl+o opens the overview that lists them.
fn words(new: &[Failure]) -> Option<String> {
    if new.is_empty() {
        return None;
    }
    let names = |needs_auth: bool| -> Vec<&str> {
        new.iter()
            .filter(|failure| failure.needs_auth == needs_auth)
            .map(|failure| failure.name.as_str())
            .collect()
    };
    let (sign_in, failed) = (names(true), names(false));
    let mut clauses = Vec::new();
    if !sign_in.is_empty() {
        let verb = if sign_in.len() == 1 { "needs" } else { "need" };
        clauses.push(format!("{} {verb} sign-in", list(&sign_in)));
    }
    if !failed.is_empty() {
        clauses.push(format!("{} failed to start", list(&failed)));
    }
    let noun = if new.len() == 1 {
        "tool server"
    } else {
        "tool servers"
    };
    Some(format!("{noun} {} · ctrl+o", clauses.join(", ")))
}

/// `a`, `a and b`, `a, b and c`.
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, status: ToolServerStatus) -> ToolServer {
        ToolServer {
            name: name.into(),
            status: status as i32,
            error: String::new(),
        }
    }

    const HOST: &[u8] = b"desk";

    #[test]
    fn a_failure_is_told_once_across_chats() {
        let mut told = Told::default();
        let servers = [
            server("linear", ToolServerStatus::NeedsAuth),
            server("github", ToolServerStatus::NeedsAuth),
            server("postgres", ToolServerStatus::Failed),
            server("files", ToolServerStatus::Ready),
        ];
        let mut first = Shown::default();
        assert_eq!(
            told.look(&HOST.to_vec(), &servers, &mut first).as_deref(),
            Some("tool servers linear and github need sign-in, postgres failed to start · ctrl+o"),
        );
        assert_eq!(told.look(&HOST.to_vec(), &servers, &mut first), None);
        // Another chat, or the same one opened again.
        let mut second = Shown::default();
        assert_eq!(told.look(&HOST.to_vec(), &servers, &mut second), None);
    }

    #[test]
    fn another_host_or_kind_is_news() {
        let mut told = Told::default();
        let mut shown = Shown::default();
        let auth = [server("github", ToolServerStatus::NeedsAuth)];
        assert!(told.look(&HOST.to_vec(), &auth, &mut shown).is_some());
        assert_eq!(
            told.look(&b"studio".to_vec(), &auth, &mut Shown::default())
                .as_deref(),
            Some("tool server github needs sign-in · ctrl+o"),
        );
        let failed = [server("github", ToolServerStatus::Failed)];
        assert_eq!(
            told.look(&HOST.to_vec(), &failed, &mut shown).as_deref(),
            Some("tool server github failed to start · ctrl+o"),
        );
    }

    #[test]
    fn recovery_in_the_chat_that_showed_it_makes_a_new_failure_news() {
        let mut told = Told::default();
        let mut shown = Shown::default();
        let failing = [server("github", ToolServerStatus::NeedsAuth)];
        assert!(told.look(&HOST.to_vec(), &failing, &mut shown).is_some());
        // A retry is not recovery.
        let retry = [server("github", ToolServerStatus::Starting)];
        assert_eq!(told.look(&HOST.to_vec(), &retry, &mut shown), None);
        assert_eq!(told.look(&HOST.to_vec(), &failing, &mut shown), None);
        let ready = [server("github", ToolServerStatus::Ready)];
        assert_eq!(told.look(&HOST.to_vec(), &ready, &mut shown), None);
        assert!(told.look(&HOST.to_vec(), &failing, &mut shown).is_some());
    }

    #[test]
    fn a_healthy_namesake_in_another_chat_is_not_recovery() {
        let mut told = Told::default();
        let failing = [server("github", ToolServerStatus::NeedsAuth)];
        let ready = [server("github", ToolServerStatus::Ready)];
        let (mut broken, mut healthy) = (Shown::default(), Shown::default());
        assert!(told.look(&HOST.to_vec(), &failing, &mut broken).is_some());
        // Switching between them never tells it again.
        for _ in 0..2 {
            assert_eq!(told.look(&HOST.to_vec(), &ready, &mut healthy), None);
            let mut again = Shown::default();
            assert_eq!(told.look(&HOST.to_vec(), &failing, &mut again), None);
        }
    }
}
