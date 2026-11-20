//! The acts on a host's own agents, AgentService, as each kind of caller
//! makes them: a person here, one of this host's agents, a person at a
//! paired host and an agent of a paired host. Calls reach the handlers in
//! memory, with the caller settled as the entry each would arrive on
//! settles it, so every rule is checked against every caller with no
//! transport in between. The paired host is only an id: a call carried
//! from it arrives as its link would deliver it.
//!
//! The agents are synthetic, a directory, a journal and a control socket,
//! so their rows are exactly what each test writes.

use std::path::Path;
use std::sync::Arc;

use analytics::Client;
use node::harness::{Calling, InMemory, Served, served_as};
use node::{Daemon, ProfileRuntime};
use store::{AgentKey, AgentRow};
use support::synthetic::*;
use support::*;
use tonic::{Code, Status};
use wire::agent_service_client::AgentServiceClient;

use crate::support;

pub const SEGMENTS: u64 = 1 << 20;

/// The paired host the calls from "there" come from.
pub fn peer() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x7a1e_0000_0000_4000_8000_0000_0000_0001)
}

/// Who makes a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    /// A person at this machine, on the profile's socket.
    PersonHere,
    /// One of this host's agents, on its tools socket.
    AgentHere,
    /// A person at the paired host, carried in on its link.
    PersonThere,
    /// One of the paired host's agents, carried in on its link.
    AgentThere,
}

pub const EVERYONE: [Who; 4] = [
    Who::PersonHere,
    Who::AgentHere,
    Who::PersonThere,
    Who::AgentThere,
];

impl Who {
    pub fn is_agent(self) -> bool {
        matches!(self, Self::AgentHere | Self::AgentThere)
    }
}

/// A host's installation before its daemon starts: this host's calling
/// agent, live, and the agents each test registers.
pub struct Setup {
    pub install: Install,
    caller: SyntheticAgent,
    there: uuid::Uuid,
    agents: Vec<SyntheticAgent>,
}

impl Setup {
    pub fn new() -> Self {
        Self::with_caller(true)
    }

    /// This host's calling agent live, its process answering, or not, which
    /// the daemon finds exited.
    pub fn with_caller(live: bool) -> Self {
        let install = Install::new();
        let mut caller = SyntheticAgent::new(&install, "caller-here", SEGMENTS);
        caller.register(&install);
        if live {
            caller.go_live();
        }
        Self {
            install,
            caller,
            there: uuid::Uuid::new_v4(),
            agents: Vec::new(),
        }
    }

    /// The parent edge a child of `who`'s carries: an agent's own key, and
    /// none for a person, whose agents have no parent.
    pub fn parent_of(&self, who: Who) -> Option<AgentKey> {
        match who {
            Who::PersonHere | Who::PersonThere => None,
            Who::AgentHere => Some(self.caller.key(&self.install)),
            Who::AgentThere => Some(AgentKey::new(
                peer().as_bytes().to_vec(),
                self.there.as_bytes().to_vec(),
            )),
        }
    }

    /// Registers one of this host's agents under `parent`, live (its
    /// process answering) or not, which the daemon finds exited.
    pub fn agent(&mut self, name: &str, parent: Option<AgentKey>, live: bool) -> uuid::Uuid {
        let mut agent = SyntheticAgent::new(&self.install, name, SEGMENTS);
        agent.parent = parent;
        agent.register(&self.install);
        if live {
            agent.go_live();
        }
        let id = agent.id;
        self.agents.push(agent);
        id
    }

    pub async fn start(self) -> Host {
        self.start_with(|_| {}).await
    }

    /// Starts the daemon, its agents run on the fake provider with nothing
    /// to say unless `options` gives them a script.
    pub async fn start_with(self, options: impl FnOnce(&mut node::StartOptions)) -> Host {
        let idle = self.install.launch("idle", Vec::new());
        let mut start = self.install.options("boot-1", idle);
        options(&mut start);
        let daemon = node::start(start, None).await.expect("the daemon starts");
        let runtime = runtime(&daemon, &self.install);
        Host {
            install: self.install,
            daemon,
            runtime,
            caller: self.caller,
            there: self.there,
            agents: self.agents,
        }
    }
}

/// A host's daemon running, with the agents its setup registered.
pub struct Host {
    pub install: Install,
    pub daemon: Daemon,
    pub runtime: Arc<ProfileRuntime>,
    caller: SyntheticAgent,
    there: uuid::Uuid,
    agents: Vec<SyntheticAgent>,
}

impl Host {
    /// AgentService as `who` reaches it.
    pub fn agents(&self, who: Who) -> AgentServiceClient<InMemory> {
        let here = self.runtime.host();
        let (served, calling) = match who {
            Who::PersonHere => (Served::Profile, Calling::PersonHere(Client::Terminal)),
            Who::AgentHere => (
                Served::Tools(self.caller.id),
                Calling::Agent {
                    host: here,
                    agent: self.caller.id,
                    name: self.caller.name.clone(),
                },
            ),
            Who::PersonThere => (Served::Link, Calling::PersonAt(peer(), Client::Phone)),
            Who::AgentThere => (
                Served::Link,
                Calling::Agent {
                    host: peer(),
                    agent: self.there,
                    name: "caller-there".to_owned(),
                },
            ),
        };
        wire::agent_service_client(served_as(&self.runtime, served, calling))
    }

    /// The agent who calls, when `who` is one: its host and its id.
    pub fn calling_agent(&self, who: Who) -> Option<(uuid::Uuid, uuid::Uuid)> {
        match who {
            Who::PersonHere | Who::PersonThere => None,
            Who::AgentHere => Some((self.runtime.host(), self.caller.id)),
            Who::AgentThere => Some((peer(), self.there)),
        }
    }

    /// One of the synthetic agents the setup registered.
    pub fn synthetic(&self, id: uuid::Uuid) -> &SyntheticAgent {
        std::iter::once(&self.caller)
            .chain(&self.agents)
            .find(|agent| agent.id == id)
            .expect("a registered agent")
    }

    /// Writes a row this host holds as its copy of the paired host's agent.
    pub fn copy(&self, name: &str) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        let key = AgentKey::new(peer().as_bytes().to_vec(), id.as_bytes().to_vec());
        let mut row = AgentRow::new(key, wire::Kind::ClaudeSdk, "/");
        row.name = name.to_owned();
        self.install.store().put_agent(&row).unwrap();
        id
    }

    pub async fn row(&self, id: uuid::Uuid) -> Option<AgentRow> {
        self.runtime
            .read_store(move |store| store.agent_by_id(id.as_bytes()).unwrap())
            .await
            .unwrap()
    }

    /// Waits for every agent registered not live to be listed exited.
    pub async fn settled(&self) {
        for agent in &self.agents {
            let id = agent.id;
            until("the agent's row to settle", || async {
                match self.row(id).await {
                    Some(row) if row.lifecycle == wire::Lifecycle::Exited as i32 => Ok(()),
                    Some(_) if self.runtime.live().contains(&id) => Ok(()),
                    other => Err(format!("{other:?}")),
                }
            })
            .await
            .unwrap();
        }
    }

    pub async fn stop(self) {
        kill_all(&self.runtime).await;
        let Self {
            install,
            daemon,
            runtime,
            caller,
            agents,
            ..
        } = self;
        drop(runtime);
        daemon.shutdown().await.unwrap();
        drop((caller, agents, install));
    }
}

/// A person's prompt, as a composer sends it.
pub fn prompt(text: &str) -> wire::Input {
    sdk_prompt(uuid::Uuid::new_v4().as_bytes(), text)
}

/// A create in `cwd` with the kind's defaults, and a first prompt when
/// given.
pub fn create(cwd: &Path, name: &str, first: Option<&str>) -> wire::CreateAgentRequest {
    wire::CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        name: Some(name.to_owned()),
        cwd: cwd.to_string_lossy().into_owned(),
        agent: Some(wire::create_agent_request::Agent::ClaudeSdk(
            wire::ClaudeCreateConfig::default(),
        )),
        initial_prompt: first.map(|text| submission(b"p0", text)),
        ..Default::default()
    }
}

/// A message to `to` with a fresh id.
pub fn message(to: uuid::Uuid, text: &str) -> wire::SendMessageRequest {
    wire::SendMessageRequest {
        id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        agent_id: to.as_bytes().to_vec(),
        context: None,
        message: Some(wire::MessageBody {
            text: text.to_owned(),
            attachments: Vec::new(),
        }),
    }
}

/// Makes `install.work` a repository with one commit, so the agents
/// working there have changes to read.
pub fn repository(install: &Install) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ][..],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&install.work)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
}

/// Every act that names an agent but a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Rename,
    Input,
    PutBlob,
    PreviewDelete,
    Close,
    Resume,
    Delete,
}

impl Act {
    /// Acts on a live agent, and acts on one that has exited.
    const ON_LIVE: [Self; 4] = [
        Self::Rename,
        Self::Input,
        Self::PutBlob,
        Self::PreviewDelete,
    ];
    const ON_EXITED: [Self; 2] = [Self::Close, Self::Resume];
}

/// `act` on `target`, made by `who`.
async fn act(host: &Host, who: Who, act: Act, target: uuid::Uuid) -> Result<(), Status> {
    let mut agents = host.agents(who);
    let agent_id = target.as_bytes().to_vec();
    match act {
        Act::Rename => agents
            .rename_agent(wire::RenameAgentRequest {
                agent_id,
                name: format!("renamed-{}", &target.simple().to_string()[..8]),
            })
            .await
            .map(drop),
        Act::Input => agents
            .send_input(wire::SendInputRequest {
                agent_id,
                input: Some(prompt("look at the logs")),
            })
            .await
            .map(drop),
        Act::PutBlob => agents
            .put_blob(wire::PutBlobRequest {
                agent_id,
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                bytes: b"some notes".to_vec(),
            })
            .await
            .map(drop),
        Act::PreviewDelete => agents
            .preview_delete(wire::PreviewDeleteRequest { agent_id })
            .await
            .map(drop),
        Act::Close => agents
            .set_filing(wire::SetFilingRequest {
                agent_id,
                filing: wire::Filing::Closed as i32,
            })
            .await
            .map(drop),
        Act::Resume => agents
            .resume_agent(wire::ResumeAgentRequest {
                agent_id,
                initial_prompt: None,
                if_suspended: true,
            })
            .await
            .map(drop),
        Act::Delete => agents
            .delete_agent(wire::DeleteAgentRequest {
                agent_id,
                discard_workspace_changes: false,
            })
            .await
            .map(drop),
    }
}

/// What an act its caller may make answers: the agent's own answer, which
/// for a resume of an agent that is not suspended is that it is not.
fn allowed(act: Act, answer: Result<(), Status>) -> Result<(), String> {
    match (act, answer) {
        (Act::Resume, Err(status)) if status.code() == Code::FailedPrecondition => Ok(()),
        (Act::Resume, other) => Err(format!("{other:?}, not 'not suspended'")),
        (_, Ok(())) => Ok(()),
        (_, Err(status)) => Err(format!("{status:?}")),
    }
}

/// Every call naming an agent, but a message, reaches only the calling
/// agent's own direct children, judged by the parent edge on this host's
/// own row: an agent of a paired host by its host and id together. A
/// person, here or at a paired host, may make every one on any agent.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_acts_only_on_its_own_children_and_a_person_on_any() {
    let mut setup = Setup::new();
    repository(&setup.install);
    // What each caller acts on: its own children for an agent, agents
    // nobody started for a person.
    struct Targets {
        live: uuid::Uuid,
        exited: uuid::Uuid,
        doomed: uuid::Uuid,
    }
    let mut targets = Vec::new();
    for who in EVERYONE {
        let parent = setup.parent_of(who);
        let tag = format!("{who:?}").to_lowercase();
        targets.push(Targets {
            live: setup.agent(&format!("{tag}-live"), parent.clone(), true),
            exited: setup.agent(&format!("{tag}-exited"), parent.clone(), false),
            doomed: setup.agent(&format!("{tag}-doomed"), parent, false),
        });
    }
    let host = setup.start().await;
    host.settled().await;

    // An agent is refused everything on agents it did not start, here or
    // at its own host; nothing reaches them.
    for (i, who) in EVERYONE.into_iter().enumerate() {
        if !who.is_agent() {
            continue;
        }
        for (j, theirs) in targets.iter().enumerate() {
            if i == j {
                continue;
            }
            let acts = Act::ON_LIVE
                .map(|act_| (act_, theirs.live))
                .into_iter()
                .chain(Act::ON_EXITED.map(|act_| (act_, theirs.exited)))
                .chain([(Act::Delete, theirs.doomed)]);
            for (act_, target) in acts {
                let refused = act(&host, who, act_, target).await.unwrap_err();
                assert_eq!(
                    refused.code(),
                    Code::PermissionDenied,
                    "{who:?} {act_:?} on another's agent: {refused:?}"
                );
            }
        }
    }
    for theirs in &targets {
        let row = host.row(theirs.live).await.unwrap();
        assert!(!row.name.starts_with("renamed"), "{}", row.name);
        assert!(host.synthetic(theirs.live).inputs().is_empty());
        let exited = host.row(theirs.exited).await.unwrap();
        assert_ne!(exited.filing, wire::Filing::Closed as i32);
        assert!(host.row(theirs.doomed).await.is_some());
    }

    // Each makes every one on its own: an agent on its children, a person
    // on an agent no one started.
    for (who, mine) in EVERYONE.into_iter().zip(&targets) {
        for act_ in Act::ON_LIVE {
            let answer = act(&host, who, act_, mine.live).await;
            allowed(act_, answer).unwrap_or_else(|why| panic!("{who:?} {act_:?}: {why}"));
        }
        for act_ in Act::ON_EXITED {
            let answer = act(&host, who, act_, mine.exited).await;
            allowed(act_, answer).unwrap_or_else(|why| panic!("{who:?} {act_:?}: {why}"));
        }
        let answer = act(&host, who, Act::Delete, mine.doomed).await;
        allowed(Act::Delete, answer).unwrap_or_else(|why| panic!("{who:?} delete: {why}"));

        let row = host.row(mine.live).await.unwrap();
        assert!(row.name.starts_with("renamed"), "{who:?}: {}", row.name);
        assert_eq!(
            host.synthetic(mine.live).inputs().len(),
            1,
            "{who:?}'s input arrived"
        );
        let exited = host.row(mine.exited).await.unwrap();
        assert_eq!(exited.filing, wire::Filing::Closed as i32, "{who:?}");
        until("the deleted agent's row to go", || async {
            match host.row(mine.doomed).await {
                None => Ok(()),
                Some(_) => Err("still there"),
            }
        })
        .await
        .unwrap();
    }
    host.stop().await;
}

/// An agent starts its children with their kind's defaults, never a
/// permission mode or settings of its choosing, and files them only as
/// closed; pinning one or filing it open again is the person's.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_starts_children_with_defaults_and_files_them_only_closed() {
    let mut setup = Setup::new();
    let mut mine = Vec::new();
    for who in EVERYONE {
        let parent = setup.parent_of(who);
        let tag = format!("{who:?}").to_lowercase();
        mine.push(setup.agent(&format!("{tag}-child"), parent, false));
    }
    let host = setup.start().await;
    host.settled().await;
    let work = host.install.work.clone();

    for (who, child) in EVERYONE.into_iter().zip(mine) {
        let tag = format!("{who:?}").to_lowercase();
        let never_ask = wire::CreateAgentRequest {
            agent: Some(wire::create_agent_request::Agent::ClaudeSdk(
                wire::ClaudeCreateConfig {
                    permission: Some("bypassPermissions".into()),
                    ..Default::default()
                },
            )),
            ..create(&work, &format!("{tag}-bypass"), None)
        };
        let made = host.agents(who).create_agent(never_ask.clone()).await;
        let id = uuid::Uuid::from_slice(&never_ask.agent_id).unwrap();
        if who.is_agent() {
            assert_eq!(made.unwrap_err().code(), Code::PermissionDenied, "{who:?}");
            assert!(host.row(id).await.is_none(), "{who:?}: no child was made");
        } else {
            made.unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
            assert!(host.row(id).await.is_some(), "{who:?}");
        }

        // A person pins only an agent with no parent, so the person's
        // filing is tried on the agent it made just now.
        let target = if who.is_agent() { child } else { id };
        for filing in [wire::Filing::Pinned, wire::Filing::Open] {
            let filed = host
                .agents(who)
                .set_filing(wire::SetFilingRequest {
                    agent_id: target.as_bytes().to_vec(),
                    filing: filing as i32,
                })
                .await;
            if who.is_agent() {
                assert_eq!(
                    filed.unwrap_err().code(),
                    Code::PermissionDenied,
                    "{who:?} {filing:?}"
                );
            } else {
                let filed = filed.unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
                assert_eq!(filed.into_inner().agent.unwrap().filing, filing as i32);
            }
        }
        let closed = host
            .agents(who)
            .set_filing(wire::SetFilingRequest {
                agent_id: target.as_bytes().to_vec(),
                filing: wire::Filing::Closed as i32,
            })
            .await
            .unwrap_or_else(|status| panic!("{who:?} closing: {status:?}"));
        assert_eq!(
            closed.into_inner().agent.unwrap().filing,
            wire::Filing::Closed as i32
        );
    }
    host.stop().await;
}

/// A new agent's parent is whoever called: the calling agent, named by its
/// own host, or none for a person. One of this host's agents that names
/// no folder starts its child where it works itself; an agent of another
/// host has no folder here, and is told to name one.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_agents_parent_is_its_caller() {
    let setup = Setup::new();
    let host = setup.start().await;
    let work = host.install.work.clone();
    let elsewhere = host.install.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = elsewhere.canonicalize().unwrap();

    for who in EVERYONE {
        let tag = format!("{who:?}").to_lowercase();
        let made = host
            .agents(who)
            .create_agent(create(&elsewhere, &format!("{tag}-named"), None))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"))
            .into_inner()
            .agent
            .unwrap();
        let parent = made.parent.map(|parent| (parent.host_id, parent.agent_id));
        let calling = host
            .calling_agent(who)
            .map(|(host, agent)| (host.as_bytes().to_vec(), agent.as_bytes().to_vec()));
        assert_eq!(parent, calling, "{who:?}");
        assert_eq!(Path::new(&made.cwd), elsewhere, "{who:?}");

        // A person's empty folder is this host's home, as typed, which a
        // test leaves alone.
        if !who.is_agent() {
            continue;
        }
        let unnamed = wire::CreateAgentRequest {
            cwd: String::new(),
            ..create(&work, &format!("{tag}-unnamed"), None)
        };
        let made = host.agents(who).create_agent(unnamed).await;
        if who == Who::AgentHere {
            let made = made.unwrap().into_inner().agent.unwrap();
            assert_eq!(
                Path::new(&made.cwd),
                work.canonicalize().unwrap(),
                "where its parent works"
            );
        } else {
            let refused = made.unwrap_err();
            assert_eq!(refused.code(), Code::InvalidArgument);
            assert!(
                refused.message().contains("needs a folder there"),
                "{refused:?}"
            );
        }
    }
    host.stop().await;
}

/// A create whose calling agent has already exited is refused, since the
/// family rule would only stop the child again. A person's agent has no
/// parent to have ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_create_for_an_ended_parent_is_refused() {
    let setup = Setup::with_caller(false);
    let host = setup.start().await;
    let work = host.install.work.clone();
    let caller = host.calling_agent(Who::AgentHere).unwrap().1;
    until("the calling agent to exit", || async {
        match host.row(caller).await {
            Some(row) if row.lifecycle == wire::Lifecycle::Exited as i32 => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();

    let refused = host
        .agents(Who::AgentHere)
        .create_agent(create(&work, "late-child", None))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert_eq!(
        refused.message(),
        format!("agent {caller} has ended and starts no more agents")
    );
    for who in [Who::PersonHere, Who::PersonThere] {
        host.agents(who)
            .create_agent(create(&work, &format!("{who:?}").to_lowercase(), None))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
    }
    host.stop().await;
}

/// The attachment limit is the owner's: a file over it is refused here,
/// whoever sent it and however it came, before anything is looked up.
#[tokio::test(flavor = "multi_thread")]
async fn the_owner_holds_the_attachment_limit() {
    let mut setup = Setup::new();
    let mut mine = Vec::new();
    for who in EVERYONE {
        let parent = setup.parent_of(who);
        mine.push(setup.agent(&format!("{who:?}").to_lowercase(), parent, false));
    }
    let host = setup.start().await;
    let too_large = vec![0; wire::ATTACHMENT_SIZE_LIMIT + 1];
    for (who, agent) in EVERYONE.into_iter().zip(mine) {
        let put = |bytes: Vec<u8>| wire::PutBlobRequest {
            agent_id: agent.as_bytes().to_vec(),
            name: "huge.bin".into(),
            mime: "application/octet-stream".into(),
            bytes,
        };
        let refused = host
            .agents(who)
            .put_blob(put(too_large.clone()))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), Code::ResourceExhausted, "{who:?}");
        assert!(refused.message().contains("huge.bin"), "{refused:?}");
        host.agents(who)
            .put_blob(put(b"small".to_vec()))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
    }
    host.stop().await;
}

/// A message's id is the caller's, sixteen bytes. A send of any other
/// length is refused, and so is one of the shape before this one, whose
/// envelope decodes where the id is.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_id_is_sixteen_bytes() {
    let mut setup = Setup::new();
    let recipient = setup.agent("recipient", None, true);
    let host = setup.start().await;
    let old_shape = {
        use prost::Message as _;
        /// A send as builds before the caller was its sender wrote it.
        #[derive(Clone, PartialEq, prost::Message)]
        struct OldSend {
            #[prost(message, optional, tag = "1")]
            envelope: Option<wire::Envelope>,
        }
        let old = OldSend {
            envelope: Some(wire::Envelope {
                id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                to: Some(wire::AgentRef {
                    host_id: host.runtime.host().as_bytes().to_vec(),
                    agent_id: recipient.as_bytes().to_vec(),
                }),
                body: Some(wire::envelope::Body::Message(wire::MessageBody {
                    text: "from an older tool server".into(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
        };
        wire::SendMessageRequest::decode(old.encode_to_vec().as_slice()).unwrap()
    };
    for who in EVERYONE {
        for length in [0, 4, 15, 17, 32] {
            let refused = host
                .agents(who)
                .send_message(wire::SendMessageRequest {
                    id: vec![7; length],
                    ..message(recipient, "hello")
                })
                .await
                .unwrap_err();
            assert_eq!(refused.code(), Code::InvalidArgument, "{who:?} {length}");
        }
        let refused = host
            .agents(who)
            .send_message(old_shape.clone())
            .await
            .unwrap_err();
        assert_eq!(refused.code(), Code::InvalidArgument, "{who:?} old shape");
        let sent = message(recipient, "hello");
        let answer = host
            .agents(who)
            .send_message(sent.clone())
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        assert_eq!(answer.into_inner().envelope_id, sent.id);
    }
    assert_eq!(
        host.synthetic(recipient).inputs().len(),
        EVERYONE.len(),
        "only the well-formed sends arrived"
    );
    host.stop().await;
}

/// An agent this host holds only a copy of is its own host's to act on:
/// every act on it, a message included, is not found here, and the answer
/// names the host that owns it.
#[tokio::test(flavor = "multi_thread")]
async fn an_act_on_a_copy_is_not_found_and_names_its_owner() {
    let setup = Setup::new();
    let host = setup.start().await;
    let copy = host.copy("theirs");
    let owner = peer().to_string();
    for who in EVERYONE {
        let mut answers = Vec::new();
        for act_ in Act::ON_LIVE
            .into_iter()
            .chain(Act::ON_EXITED)
            .chain([Act::Delete])
        {
            answers.push((format!("{act_:?}"), act(&host, who, act_, copy).await));
        }
        let sent = host.agents(who).send_message(message(copy, "hi")).await;
        answers.push(("SendMessage".into(), sent.map(drop)));
        for (act_, answer) in answers {
            let refused = answer.unwrap_err();
            assert_eq!(
                refused.code(),
                Code::NotFound,
                "{who:?} {act_}: {refused:?}"
            );
            assert!(
                refused.message().contains(&owner),
                "{who:?} {act_} names the owner: {refused:?}"
            );
        }
    }
    assert!(host.row(copy).await.is_some(), "the copy is untouched");
    host.stop().await;
}

/// Whether this host runs the kind asked for is its own word: a kind it
/// does not run is refused here, for every caller, in words naming it.
#[tokio::test(flavor = "multi_thread")]
async fn the_owner_refuses_a_kind_it_does_not_run() {
    let setup = Setup::new();
    let host = setup
        .start_with(|options| options.edge.kinds = vec![wire::Kind::ClaudeSdk])
        .await;
    let work = host.install.work.clone();
    for who in EVERYONE {
        let codex = wire::CreateAgentRequest {
            agent: Some(wire::create_agent_request::Agent::Codex(
                wire::CodexCreateConfig::default(),
            )),
            ..create(&work, &format!("{who:?}").to_lowercase(), None)
        };
        let id = uuid::Uuid::from_slice(&codex.agent_id).unwrap();
        let refused = host.agents(who).create_agent(codex).await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{who:?}");
        assert!(
            refused.message().ends_with("does not run Codex agents"),
            "{refused:?}"
        );
        assert!(host.row(id).await.is_none());
    }
    host.stop().await;
}

/// A create with no kind names no interpreter, and is refused before
/// anything is made, whoever asks.
#[tokio::test(flavor = "multi_thread")]
async fn a_create_without_a_kind_is_invalid() {
    let setup = Setup::new();
    let host = setup.start().await;
    let work = host.install.work.clone();
    for who in EVERYONE {
        let kindless = wire::CreateAgentRequest {
            agent: None,
            ..create(&work, &format!("{who:?}").to_lowercase(), None)
        };
        let id = uuid::Uuid::from_slice(&kindless.agent_id).unwrap();
        let refused = host.agents(who).create_agent(kindless).await.unwrap_err();
        assert_eq!(refused.code(), Code::InvalidArgument, "{who:?}");
        assert_eq!(refused.message(), "a new agent names its kind");
        assert!(host.row(id).await.is_none(), "{who:?}: no agent was made");
    }
    host.stop().await;
}

/// What a delete would take is answered to whoever may delete: a person,
/// here or at a paired host, about any agent, and an agent about its own
/// children only. An agent in a folder no workspace of this host's holds
/// takes nothing with it but says what is uncommitted there.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_preview_is_answered_to_whoever_may_delete() {
    let mut setup = Setup::new();
    repository(&setup.install);
    std::fs::write(setup.install.work.join("draft.txt"), "one\ntwo\n").unwrap();
    let mut mine = Vec::new();
    for who in EVERYONE {
        let parent = setup.parent_of(who);
        mine.push(setup.agent(&format!("{who:?}").to_lowercase(), parent, false));
    }
    let stranger = setup.agent("stranger", None, false);
    let host = setup.start().await;
    let preview = |who: Who, agent: uuid::Uuid| {
        let mut agents = host.agents(who);
        async move {
            agents
                .preview_delete(wire::PreviewDeleteRequest {
                    agent_id: agent.as_bytes().to_vec(),
                })
                .await
                .map(tonic::Response::into_inner)
        }
    };
    for (who, agent) in EVERYONE.into_iter().zip(mine) {
        let answer = preview(who, agent)
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        assert!(!answer.removes_workspace, "{who:?}");
        assert_eq!(answer.branch.as_deref(), Some("main"), "{who:?}");
        let uncommitted = answer.uncommitted.unwrap_or_default();
        assert_eq!((uncommitted.files, uncommitted.added), (1, 2), "{who:?}");

        let asked = preview(who, stranger).await;
        if who.is_agent() {
            let refused = asked.unwrap_err();
            assert_eq!(
                refused.code(),
                Code::PermissionDenied,
                "{who:?}: {refused:?}"
            );
        } else {
            asked.unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        }
    }
    host.stop().await;
}
