//! The agent process's end of its own socket (`agent_dir::agent_sock`),
//! where its tool server asks it things. The one ask is a spawn, and each
//! goes through here to the daemon.
//!
//! An agent with a parent exits when its work is done, and its work is not
//! done while a child it started still runs. The daemon knows which children
//! exist and says so, but its word and the turn's end reach the process on
//! different sockets, in either order. So the process does not learn of its
//! own spawns from the daemon: it carries each one. The core hears of the
//! child before the request goes to the daemon, and of the daemon's answer
//! before the tool server has it, and so before the provider can end the
//! turn the spawn was made in.

use std::path::PathBuf;

use agent_core::Spawn;
use agent_dir::agent_sock::{self, Spawned};
use tokio::sync::{mpsc, oneshot};
use wire::{AgentAnswer, CreateAgentRequest, agent_answer, agent_ask};

use crate::local_socket::{LocalListener, LocalStream};
use crate::tools;

/// What the process heard of a child its tool server asked for, on its way
/// to the agent's core.
#[derive(Debug)]
pub struct Heard {
    /// The child's agent id, which the tool server chose.
    pub child: Vec<u8>,
    pub heard: Spawn,
    /// Sent once the core has it. The spawn goes no further until then.
    pub taken: oneshot::Sender<()>,
}

/// Serves the socket of the agent in `dir` that its tool server asks on,
/// until the listener fails. Each spawn is told to the core through
/// `heard`, made of the daemon (dialling retried over the window an
/// update's restart takes, the call itself made at most once), and
/// answered as the daemon answered it. An ask this build does not know is
/// refused in words.
pub async fn serve(mut listener: LocalListener, dir: PathBuf, heard: mpsc::UnboundedSender<Heard>) {
    while let Ok(stream) = listener.accept().await {
        tokio::spawn(carry(stream, dir.clone(), heard.clone()));
    }
}

/// One ask and its answer. A spawn that the core could not be told of gets
/// none: the tool server finds its connection closed.
async fn carry(mut stream: LocalStream, dir: PathBuf, heard: mpsc::UnboundedSender<Heard>) {
    let Ok(Some(ask)) = agent_sock::read_ask(&mut stream).await else {
        return;
    };
    let answer: AgentAnswer = match ask.of {
        Some(agent_ask::Of::Spawn(request)) => match spawn(&dir, &heard, request).await {
            Some(spawned) => spawned.into(),
            None => return,
        },
        None => AgentAnswer {
            of: Some(agent_answer::Of::Refused(
                agent_sock::UNKNOWN_ASK.to_owned(),
            )),
        },
    };
    let _ = agent_sock::write_answer(&mut stream, &answer).await;
}

/// One spawn, from the tool server to the daemon and back. A core that is
/// gone takes nothing: the process is exiting and the request is not made.
async fn spawn(
    dir: &std::path::Path,
    heard: &mpsc::UnboundedSender<Heard>,
    request: CreateAgentRequest,
) -> Option<Spawned> {
    let child = request.agent_id.clone();
    if !tell(heard, &child, Spawn::Requested).await {
        return None;
    }
    let (answer, outcome) = tools::create(dir, request).await;
    tell(heard, &child, outcome).await;
    Some(answer)
}

/// Tells the core, and waits until it has taken it.
async fn tell(heard: &mpsc::UnboundedSender<Heard>, child: &[u8], what: Spawn) -> bool {
    let (taken, waited) = oneshot::channel();
    heard
        .send(Heard {
            child: child.to_vec(),
            heard: what,
            taken,
        })
        .is_ok()
        && waited.await.is_ok()
}
