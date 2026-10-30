//! The hook connections terminal Claude waits on for amux's decision.
//!
//! Claude runs the hook command for a permission request or a tool
//! server's form and reads its output as the decision. The command
//! forwards the payload and waits for the agent to answer on the same
//! connection; the agent parks that connection under a number it writes
//! into the payload ([`interpret::HOOK_ANSWER_FIELD`]), and the interpreter
//! answers by that number. An empty answer releases the hook without a
//! decision: the ask was settled in Claude's own terminal, or is over.
//! Every other hook's connection is closed as soon as it is read, so
//! Claude waits on it no longer than the delivery takes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::local_socket::LocalStream;

#[derive(Clone, Default)]
pub(crate) struct HookAnswers(Arc<Mutex<Parked>>);

#[derive(Default)]
struct Parked {
    next: u64,
    waiting: BTreeMap<u64, LocalStream>,
}

impl HookAnswers {
    /// Holds a payload's connection when Claude waits on it for a
    /// decision, and returns the payload naming where its answer goes.
    /// Any other connection is closed here.
    pub(crate) fn hold(&self, payload: Vec<u8>, stream: LocalStream) -> Vec<u8> {
        let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(&payload) else {
            return payload;
        };
        let answered = fields
            .get("hook_event_name")
            .and_then(Value::as_str)
            .is_some_and(|name| claude::hooks::ANSWERED_EVENTS.contains(&name));
        if !answered {
            return payload;
        }
        let mut parked = self.0.lock().expect("hook answers lock");
        parked.next += 1;
        let id = parked.next;
        parked.waiting.insert(id, stream);
        fields.insert(interpret::HOOK_ANSWER_FIELD.to_owned(), id.into());
        serde_json::to_vec(&fields).unwrap_or(payload)
    }

    /// Writes the answer on the connection numbered `id` and closes it. A
    /// connection already answered, or whose hook Claude gave up on, is
    /// nothing to write to.
    pub(crate) fn answer(&self, id: u64, answer: Vec<u8>) {
        let Some(mut stream) = self
            .0
            .lock()
            .expect("hook answers lock")
            .waiting
            .remove(&id)
        else {
            return;
        };
        tokio::spawn(async move {
            if !answer.is_empty() {
                let _ = stream.write_all(&answer).await;
            }
            let _ = stream.shutdown().await;
        });
    }
}
