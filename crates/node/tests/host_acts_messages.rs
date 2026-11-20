//! Messages to a host's own agents, as each kind of caller sends them: the
//! sender the recipient is handed is the caller, and the caller is what
//! decides whether the message may wake a recipient that has stopped.

use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::sender;

use crate::host_acts::{EVERYONE, Host, Setup, Who, create, message};
use crate::support;

/// The sender on what the recipient is handed is whoever called: a
/// person, wherever they are, or the calling agent, named by its own host
/// and the name its host gave in the call. The request has nowhere to
/// name anyone else.
#[tokio::test(flavor = "multi_thread")]
async fn a_messages_sender_is_its_caller() {
    let mut setup = Setup::new();
    let recipient = setup.agent("recipient", None, true);
    let host = setup.start().await;

    for who in EVERYONE {
        host.agents(who)
            .send_message(message(recipient, &format!("from {who:?}")))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
    }
    let handed: Vec<_> = host
        .synthetic(recipient)
        .inputs()
        .into_iter()
        .map(|handed| match handed {
            Handed::Message(envelope) => *envelope,
            Handed::Input(input) => panic!("an agent message, not {input:?}"),
        })
        .collect();
    assert_eq!(handed.len(), EVERYONE.len());
    for (who, envelope) in EVERYONE.into_iter().zip(handed) {
        let from = envelope.from.and_then(|from| from.value);
        match (host.calling_agent(who), from) {
            (None, Some(sender::Value::Human(_))) => {}
            (Some((agent_host, agent)), Some(sender::Value::Agent(from))) => {
                let named = from.agent.unwrap();
                assert_eq!(named.host_id, agent_host.as_bytes(), "{who:?}");
                assert_eq!(named.agent_id, agent.as_bytes(), "{who:?}");
                let name = if who == Who::AgentHere {
                    "caller-here"
                } else {
                    "caller-there"
                };
                assert_eq!(from.name, name);
            }
            (calling, from) => panic!("{who:?} ({calling:?}) sent as {from:?}"),
        }
    }
    host.stop().await;
}

/// Waits for `agent` to have exited, filed closed as a one-shot agent that
/// finished its work is, and returns its incarnation.
async fn settled_closed(host: &Host, agent: uuid::Uuid) -> u32 {
    until("the one-shot agent to finish and settle", || async {
        match host.row(agent).await {
            Some(row)
                if row.lifecycle == wire::Lifecycle::Exited as i32
                    && row.filing == wire::Filing::Closed as i32 =>
            {
                Ok(row.incarnation)
            }
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap()
}

/// Waits for `agent` to have exited.
async fn exited(host: &Host, agent: uuid::Uuid) {
    until("the agent to exit", || async {
        match host.row(agent).await {
            Some(row) if row.lifecycle == wire::Lifecycle::Exited as i32 => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();
}

/// A message may wake an agent that has stopped: a person's always,
/// wherever the person is, and its parent's always, of this host or a
/// paired one; another agent's only while the agent is not closed. The
/// caller says which it is.
#[tokio::test(flavor = "multi_thread")]
async fn who_may_wake_a_stopped_agent_is_the_callers_to_say() {
    let setup = Setup::new();
    let launch = setup.install.launch(
        "oneshot",
        vec![text("done"), provider_fakes::script::Step::TurnEnd],
    );
    let host = setup
        .start_with(move |options| options.launch = launch)
        .await;
    let work = host.install.work.clone();
    let child_of = |who: Who, name: &'static str| {
        let host = &host;
        let work = work.clone();
        async move {
            let made = host
                .agents(who)
                .create_agent(create(&work, name, Some("go")))
                .await
                .unwrap_or_else(|status| panic!("{who:?} creating: {status:?}"))
                .into_inner()
                .agent
                .unwrap();
            uuid::Uuid::from_slice(&made.agent_id).unwrap()
        }
    };
    let here = child_of(Who::AgentHere, "child-here").await;
    let there = child_of(Who::AgentThere, "child-there").await;

    // Woken by a message, a one-shot agent works the message as its turn,
    // finishes and settles again.
    let wakes = |who: Who, agent: uuid::Uuid| {
        let host = &host;
        async move {
            let before = settled_closed(host, agent).await;
            host.agents(who)
                .send_message(message(agent, "one more thing"))
                .await
                .unwrap_or_else(|status| panic!("{who:?} waking: {status:?}"));
            let after = settled_closed(host, agent).await;
            assert_eq!(after, before + 1, "{who:?} woke it into a new incarnation");
        }
    };
    let refused = |who: Who, agent: uuid::Uuid| {
        let host = &host;
        async move {
            let before = settled_closed(host, agent).await;
            let refused = host
                .agents(who)
                .send_message(message(agent, "hello?"))
                .await
                .unwrap_err();
            assert_eq!(refused.code(), Code::FailedPrecondition, "{who:?}");
            assert_eq!(
                refused.message(),
                "the recipient is settled, done with; only the person or its parent start it again"
            );
            assert_eq!(host.row(agent).await.unwrap().incarnation, before);
        }
    };

    refused(Who::AgentThere, here).await;
    for who in [Who::PersonHere, Who::AgentHere, Who::PersonThere] {
        wakes(who, here).await;
    }
    refused(Who::AgentHere, there).await;
    wakes(Who::AgentThere, there).await;

    // Filed open again, a stopped agent is any agent's to wake.
    for (agent, stranger) in [(here, Who::AgentThere), (there, Who::AgentHere)] {
        settled_closed(&host, agent).await;
        host.agents(Who::PersonHere)
            .set_filing(wire::SetFilingRequest {
                agent_id: agent.as_bytes().to_vec(),
                filing: wire::Filing::Open as i32,
            })
            .await
            .unwrap();
        let before = host.row(agent).await.unwrap().incarnation;
        host.agents(stranger)
            .send_message(message(agent, "are you there?"))
            .await
            .unwrap_or_else(|status| panic!("{stranger:?} waking an open agent: {status:?}"));
        exited(&host, agent).await;
        assert_eq!(host.row(agent).await.unwrap().incarnation, before + 1);
    }

    host.stop().await;
}

/// A message the recipient could not read whole is refused rather than
/// delivered as an empty message or with a file missing, whoever sends it:
/// one with no body this build knows, or an attachment it does not know.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_this_host_cannot_read_is_refused() {
    let mut setup = Setup::new();
    let recipient = setup.agent("recipient", None, true);
    let host = setup.start().await;
    for who in EVERYONE {
        let unreadable = wire::SendMessageRequest {
            message: Some(wire::MessageBody {
                text: "see this".into(),
                attachments: vec![wire::Attachment { of: None }],
            }),
            ..message(recipient, "")
        };
        let refused = host.agents(who).send_message(unreadable).await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{who:?}");
        assert_eq!(refused.message(), attachments::UNREADABLE);

        let bodiless = wire::SendMessageRequest {
            message: None,
            ..message(recipient, "")
        };
        let refused = host.agents(who).send_message(bodiless).await.unwrap_err();
        assert_eq!(refused.code(), Code::InvalidArgument, "{who:?}");
        assert_eq!(refused.message(), "a message carries words or attachments");
    }
    assert!(host.synthetic(recipient).inputs().is_empty());
    host.stop().await;
}

/// A caller that sends again with the id it chose, as one whose answer
/// was lost does, makes one message: the recipient is handed it once.
#[tokio::test(flavor = "multi_thread")]
async fn a_resend_with_the_same_id_is_one_message() {
    let mut setup = Setup::new();
    let recipient = setup.agent("recipient", None, true);
    let host = setup.start().await;
    for who in EVERYONE {
        let sent = message(recipient, &format!("from {who:?}"));
        for _ in 0..2 {
            let answer = host
                .agents(who)
                .send_message(sent.clone())
                .await
                .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
            assert_eq!(answer.into_inner().envelope_id, sent.id);
        }
    }
    assert_eq!(
        host.synthetic(recipient).inputs().len(),
        EVERYONE.len(),
        "each caller's message handed over once"
    );
    host.stop().await;
}
