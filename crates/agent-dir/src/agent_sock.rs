//! The agent's own socket, `private/agent.sock`: where an agent's tool
//! server asks its agent process for what only that process can do.
//!
//! The agent process listens and its tool server is the only client. One
//! connection carries one ask and its answer, in the control socket's
//! framing: an [`AgentAsk`] and back an [`AgentAnswer`]. Each says which
//! kind it is, so a later build can add an ask; an agent process from
//! before that ask existed reads one it does not know and refuses it in
//! words.
//!
//! The one ask today is a spawn: the `CreateAgentRequest` the daemon takes,
//! as the tool server would have sent it there. The agent process hands it
//! to its daemon and the answer back, so it knows of every child it asked
//! for from before the daemon does.
//!
//! Nothing richer is needed: an ask has one answer, and the tool server
//! turns any refusal into words for the model anyway, so no status code
//! travels and the agent process serves no gRPC.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};
use wire::{
    AgentAnswer, AgentAsk, CreateAgentRequest, CreateAgentResponse, agent_answer, agent_ask,
};

use crate::ctl::{read_message, write_message};
use crate::{AGENT_SOCK, PRIVATE, local_socket};

/// What an agent process answers an ask it does not know.
pub const UNKNOWN_ASK: &str = "this agent's process does not know that request";

/// The socket of the agent in `dir` that its tool server asks on.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(PRIVATE).join(AGENT_SOCK)
}

/// Why an ask got no answer.
#[derive(Debug)]
pub enum AskError {
    /// Nothing listens on the socket: the ask went nowhere.
    NotListening(io::Error),
    /// The agent process had the ask and went away, or stopped making
    /// sense, before it answered: it may have acted on it.
    Lost(io::Error),
}

/// Asks the agent process in `dir` and waits for its answer.
pub async fn ask(dir: &Path, ask: &AgentAsk) -> Result<AgentAnswer, AskError> {
    let mut stream = local_socket::connect(&path(dir))
        .await
        .map_err(AskError::NotListening)?;
    answer(&mut stream, ask).await.map_err(AskError::Lost)
}

async fn answer(stream: &mut local_socket::LocalStream, ask: &AgentAsk) -> io::Result<AgentAnswer> {
    write_message(stream, ask).await?;
    read_message(stream)
        .await?
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
}

/// How a spawn ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Spawned {
    /// The daemon made the child: its answer, as it gave it.
    Created(Box<CreateAgentResponse>),
    /// There is no child, or nobody can say whether there is: why, in the
    /// words the model reads.
    Refused(String),
}

impl From<Spawned> for AgentAnswer {
    fn from(spawned: Spawned) -> Self {
        Self {
            of: Some(match spawned {
                Spawned::Created(response) => agent_answer::Of::Spawned(*response),
                Spawned::Refused(why) => agent_answer::Of::Refused(why),
            }),
        }
    }
}

/// Asks the agent process in `dir` for the child `request` describes and
/// waits for its answer.
pub async fn spawn(dir: &Path, request: &CreateAgentRequest) -> Result<Spawned, AskError> {
    let asked = AgentAsk {
        of: Some(agent_ask::Of::Spawn(request.clone())),
    };
    match ask(dir, &asked).await?.of {
        Some(agent_answer::Of::Spawned(response)) => Ok(Spawned::Created(Box::new(response))),
        Some(agent_answer::Of::Refused(why)) => Ok(Spawned::Refused(why)),
        None => Err(AskError::Lost(io::Error::new(
            io::ErrorKind::InvalidData,
            "an answer to a spawn of no kind this build knows",
        ))),
    }
}

/// The ask a tool server sent on its connection, or None when it sent
/// none. An ask of a kind this build does not know reads as one with
/// nothing in it.
pub async fn read_ask<R: AsyncRead + Unpin>(input: &mut R) -> io::Result<Option<AgentAsk>> {
    read_message(input).await
}

pub async fn write_answer<W: AsyncWrite + Unpin>(
    out: &mut W,
    answer: &AgentAnswer,
) -> io::Result<()> {
    write_message(out, answer).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both answers to a spawn come back as they were written.
    #[tokio::test]
    async fn a_spawns_answer_reads_back_as_written() {
        let created = Spawned::Created(Box::new(CreateAgentResponse {
            agent: Some(wire::Agent {
                agent_id: b"child".to_vec(),
                name: "helper".into(),
                incarnation: 1,
                ..Default::default()
            }),
        }));
        let refused = Spawned::Refused("no trusted host is named stranger".into());
        for answer in [created, refused] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join(PRIVATE)).unwrap();
            let mut listener = local_socket::LocalListener::bind(&path(dir.path())).unwrap();
            let request = CreateAgentRequest {
                agent_id: b"child".to_vec(),
                ..Default::default()
            };
            let (asked, served) = tokio::join!(spawn(dir.path(), &request), async {
                let mut stream = listener.accept().await.unwrap();
                let ask = read_ask(&mut stream).await.unwrap();
                write_answer(&mut stream, &answer.clone().into())
                    .await
                    .unwrap();
                ask
            });
            assert_eq!(
                served.and_then(|ask| ask.of),
                Some(agent_ask::Of::Spawn(request))
            );
            assert_eq!(asked.unwrap(), answer);
        }
    }

    /// A directory whose process binds no such socket takes no ask; one
    /// that hangs up on an ask it took may have acted on it.
    #[tokio::test]
    async fn an_ask_with_no_answer_says_whether_it_was_taken() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(PRIVATE)).unwrap();
        let request = CreateAgentRequest::default();
        assert!(matches!(
            spawn(dir.path(), &request).await,
            Err(AskError::NotListening(_))
        ));

        let mut listener = local_socket::LocalListener::bind(&path(dir.path())).unwrap();
        let (asked, ()) = tokio::join!(spawn(dir.path(), &request), async {
            let mut stream = listener.accept().await.unwrap();
            let _ = read_ask(&mut stream).await;
        });
        assert!(matches!(asked, Err(AskError::Lost(_))), "{asked:?}");
    }
}
