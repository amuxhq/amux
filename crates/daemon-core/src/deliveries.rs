//! The deliveries outbox and each agent's message lane.
//!
//! A deliveries row is written in the transaction that commits a child's
//! turn end, or when a child's incarnation exits without one. A drain pass
//! hands each row to its parent and deletes it once the parent has
//! accepted; a row whose parent has moved to another incarnation is
//! dropped, because a resumed parent is not waiting for anything, and so is
//! a row for a parent on a host this one is no longer paired with. A
//! parent that is only out of reach keeps its rows waiting, and once one of
//! its rows finds it unreachable the rest of its rows wait for the next
//! pass rather than each waiting out a full patience. A pass that kept
//! rows is followed by another after
//! [`Settings::delivery_retry_ms`](crate::Settings::delivery_retry_ms);
//! anything that may make a row deliverable meanwhile starts one at once,
//! after the pass under way when there is one.
//!
//! A parent on this host is handed its report through its message lane, as
//! any agent message is: one message at a time, deduped against the
//! recipient's items by envelope id, and accepted only once the item the
//! recipient wrote on accepting it has committed. A retry after an answer
//! finds that item, so the store lookup is the dedupe once a message has
//! been accepted. A message handed over again with no answer seen (its
//! patience ran out, or this daemon restarted and adopted the process) is
//! answered by the recipient's process itself, which takes an envelope id
//! once per incarnation. A parent's message to its exited child resumes the child
//! with the message as the new incarnation's first input. A parent on
//! another host is sent the report as a message from the child, and that
//! host's daemon makes the same checks where the parent lives.
//!
//! Nothing is handed to a lane or drained until the start's sweep is done:
//! every own journal has been read to its end by then, so the lookup for an
//! earlier acceptance never runs against a store that is behind it.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use store::{AgentKey, Delivery, StoreError};
use wire::{
    AgentRef, AgentSender, Bootstrap, ChildReport, Envelope, ErrorCode, Human, Lifecycle,
    Refusal as Refused, Rejected, SendInputResponse, Sender, bootstrap, ctl_frame, envelope,
    send_input_response, sender,
};

use crate::families::{Caller, Refusal};
use crate::ids::{AgentId, CallId, HostId, TimerId};
use crate::io::Output;
use crate::payload::{
    Answer, CallError, MessageFrom, MessageRefusal, PeerFailure, PeerRequest, Status,
};
use crate::peers::Waiting;
use crate::{Daemon, codec};

/// How long a message waits for its recipient's process to be connected.
pub const CONNECT_PATIENCE_MS: u64 = 5_000;
/// How long an accepted message waits for its acceptance item to commit.
pub const ACCEPT_PATIENCE_MS: u64 = 60_000;

/// The drain and the lanes: what is in hand and what waits its turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Deliveries {
    /// Each own agent's lane, while a message is in it.
    lanes: BTreeMap<AgentId, MessageLane>,
    /// The pass under way.
    pass: Option<Pass>,
    /// Something may have made a row deliverable during the pass under
    /// way: another follows it.
    again: bool,
    /// The passes begun since start; the newest is the one under way.
    passes: u64,
}

impl Deliveries {
    /// The drain and lanes with no count of passes run.
    pub(crate) fn canonical(&self) -> Deliveries {
        let mut deliveries = self.clone();
        deliveries.passes = 0;
        deliveries
    }

    /// The pass whose report answers someone asking now: the one under
    /// way, else the next to begin.
    pub fn answering_pass(&self) -> u64 {
        if self.pass.is_some() {
            self.passes
        } else {
            self.passes + 1
        }
    }
}

/// What one pass over the deliveries outbox did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DrainReport {
    /// Accepted by the parent, or found already accepted; deleted.
    pub delivered: usize,
    /// For a parent incarnation or a pairing that is gone; deleted unsent.
    pub stale: usize,
    /// Still waiting: the parent is away, exited, unknown or refusing.
    pub kept: usize,
}

/// A parent, by host and agent id.
type Parent = (Vec<u8>, Vec<u8>);

/// One pass: each parent's rows still to go, by envelope id in the
/// outbox's order, and the one row handed to each parent and not yet
/// answered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Pass {
    report: DrainReport,
    queued: BTreeMap<Parent, VecDeque<Vec<u8>>>,
    in_hand: BTreeMap<Parent, Vec<u8>>,
}

impl Pass {
    fn is_done(&self) -> bool {
        self.queued.is_empty() && self.in_hand.is_empty()
    }
}

/// One agent's message under way and those queued behind it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct MessageLane {
    current: Option<InLane>,
    queued: VecDeque<Message>,
}

/// A message for an own agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Message {
    #[serde(with = "codec::message")]
    envelope: Envelope,
    /// Its sender is the recipient's parent, which may resume it.
    resume: bool,
    /// The recipient's incarnation a child's report is for; stale once
    /// another has begun.
    incarnation: Option<u32>,
    whose: Whose,
}

/// Who waits for a message's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Whose {
    /// A client's or a peer's SendMessage.
    Call(CallId),
    /// The drain: the message is the outbox row its envelope id names.
    Report,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct InLane {
    message: Message,
    stage: Stage,
}

/// How far the message under way has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Stage {
    /// Waiting for the recipient's process to be connected.
    Connecting,
    /// Written to the process: waiting for the interpreter's verdict.
    Verdict,
    /// Accepted: waiting for the acceptance item to commit.
    Accepting,
    /// Resuming the exited recipient with the message as its first input:
    /// the registry builds its next spec and starts it.
    Resuming,
}

/// How a row handed to its parent came out.
enum Outcome {
    Accepted { there: bool },
    Stale { there: bool },
    Kept { unreachable: bool },
}

/// Why an outbox row was deleted, as the cheap invariant judges it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// This host's parent wrote its acceptance item.
    Accepted,
    /// The parent's host answered that it accepted.
    AcceptedThere,
    /// The parent's row is gone, or under another incarnation.
    Stale,
    /// The parent's host answered that the incarnation is gone.
    StaleThere,
    /// The parent's host is no longer paired.
    Unpaired,
}

/// What a row comes to once judged.
enum Judged {
    Counted(Outcome),
    Send(Box<Envelope>),
}

/// The envelope id of an outbox row: the same for every re-send of one
/// row, so the receiver's lookup by envelope id catches a duplicate.
pub fn delivery_envelope_id(delivery: &Delivery) -> Vec<u8> {
    let mut name = delivery.child_id.clone();
    name.extend(delivery.incarnation.to_be_bytes());
    name.push(u8::from(delivery.is_exit()));
    name.extend(delivery.turn_id.to_be_bytes());
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &name)
        .as_bytes()
        .to_vec()
}

fn refused(refusal: Refused) -> Rejected {
    Rejected {
        refusal: refusal as i32,
        detail: None,
    }
}

impl Daemon {
    /// The drain and the lanes, for reading.
    pub fn deliveries(&self) -> &Deliveries {
        &self.deliveries
    }

    /// Something changed that may make an outbox row deliverable or stale:
    /// a pass runs once the step is over.
    pub(crate) fn deliveries_due(&mut self) {
        self.touched.deliveries_due = true;
    }

    // --- the drain -------------------------------------------------------------

    /// Ends the step's part of the drain: begins a pass if one is due,
    /// hands each parent its next row, and ends a pass that is done.
    pub(crate) fn settle_deliveries(&mut self, out: &mut Vec<Output>) {
        if std::mem::take(&mut self.touched.deliveries_due) && self.families.swept() {
            if self.deliveries.pass.is_some() {
                self.deliveries.again = true;
            } else {
                self.begin_pass();
            }
        }
        loop {
            let Some(pass) = &self.deliveries.pass else {
                return;
            };
            let free: Vec<Parent> = pass
                .queued
                .keys()
                .filter(|parent| !pass.in_hand.contains_key(*parent))
                .cloned()
                .collect();
            for parent in free {
                self.next_report(&parent, out);
            }
            if !self.deliveries.pass.as_ref().is_some_and(Pass::is_done) {
                return;
            }
            let Some(pass) = self.deliveries.pass.take() else {
                return;
            };
            out.push(Output::Drained {
                pass: self.deliveries.passes,
                report: pass.report,
            });
            if std::mem::take(&mut self.deliveries.again) {
                self.begin_pass();
                continue;
            }
            if pass.report.kept > 0 {
                out.push(Output::ArmTimer {
                    id: TimerId::Deliveries,
                    at_ms: self
                        .timers
                        .now_ms
                        .saturating_add(self.settings.delivery_retry_ms),
                });
            }
            return;
        }
    }

    fn begin_pass(&mut self) {
        let rows = match self.store.deliveries() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "reading the deliveries outbox failed");
                return;
            }
        };
        self.deliveries.passes += 1;
        let mut pass = Pass::default();
        for row in rows {
            pass.queued
                .entry((row.parent.host.clone(), row.parent.agent.clone()))
                .or_default()
                .push_back(delivery_envelope_id(&row));
        }
        self.deliveries.pass = Some(pass);
    }

    /// Hands `parent` its next row this pass, counting each row judged
    /// without sending it, until one is in hand or none is left.
    fn next_report(&mut self, parent: &Parent, out: &mut Vec<Output>) {
        loop {
            let Some(pass) = self.deliveries.pass.as_mut() else {
                return;
            };
            if pass.in_hand.contains_key(parent) {
                return;
            }
            let Some(id) = pass.queued.get_mut(parent).and_then(VecDeque::pop_front) else {
                pass.queued.remove(parent);
                return;
            };
            if pass.queued.get(parent).is_some_and(VecDeque::is_empty) {
                pass.queued.remove(parent);
            }
            let judged = match self.delivery_row(&id) {
                // Gone since the pass began: nothing to count.
                Ok(None) => continue,
                Ok(Some(row)) => self.judge(row),
                Err(error) => Err(error),
            };
            match judged {
                Ok(Judged::Counted(outcome)) => self.count(parent, outcome),
                Ok(Judged::Send(envelope)) => {
                    if let Some(pass) = self.deliveries.pass.as_mut() {
                        pass.in_hand.insert(parent.clone(), id);
                    }
                    self.hand_off(parent, *envelope, out);
                }
                Err(error) => {
                    tracing::warn!(%error, "a delivery failed; it stays in the outbox");
                    self.count(parent, Outcome::Kept { unreachable: false });
                }
            }
        }
    }

    /// The outbox row an envelope id names, if it is still there.
    fn delivery_row(&self, id: &[u8]) -> Result<Option<Delivery>, StoreError> {
        Ok(self
            .store
            .deliveries()?
            .into_iter()
            .find(|row| delivery_envelope_id(row) == id))
    }

    /// What this host can answer about a row before handing it over: the
    /// parent's row here, own or replica, says whether the parent is still
    /// the incarnation the row is for, and whether it is live to receive it.
    fn judge(&mut self, mut row: Delivery) -> Result<Judged, StoreError> {
        if AgentId::from_slice(&row.parent.agent).is_none() {
            return Ok(Judged::Counted(Outcome::Kept { unreachable: false }));
        }
        let own = row.parent.host == self.store.own_host();
        if !own {
            let Some(host) = HostId::from_slice(&row.parent.host) else {
                return Ok(Judged::Counted(Outcome::Kept { unreachable: false }));
            };
            // A parent on a host this one is no longer paired with can
            // never be reached again: the report goes. The child itself
            // carries on.
            if self.sources.knows_trust() && !self.sources.trusts(host) {
                self.remove_delivery(&row, Removal::Unpaired)?;
                return Ok(Judged::Counted(Outcome::Stale { there: false }));
            }
        }
        let Some(parent) = self.store.agent(&row.parent)? else {
            if row.parent_incarnation == 0 {
                tracing::warn!(
                    parent = %crate::hex(&row.parent.agent),
                    "a child's delivery waits: its parent's row is not held"
                );
                return Ok(Judged::Counted(Outcome::Kept { unreachable: false }));
            }
            // Held once and gone now: the parent was deleted (its children
            // follow it) or its host unpaired. Nothing is waiting for this.
            self.remove_delivery(&row, Removal::Stale)?;
            return Ok(Judged::Counted(Outcome::Stale { there: false }));
        };
        if row.parent_incarnation == 0 {
            // Written while the parent's row was not held: stamped now,
            // with the parent's incarnation as its row first shows it.
            row.parent_incarnation = parent.incarnation;
            self.store.put_delivery(&row)?;
        }
        if parent.incarnation != row.parent_incarnation {
            self.remove_delivery(&row, Removal::Stale)?;
            return Ok(Judged::Counted(Outcome::Stale { there: false }));
        }
        if parent.lifecycle == Lifecycle::Exited as i32 {
            // Not waiting now; a resume makes the row stale.
            return Ok(Judged::Counted(Outcome::Kept { unreachable: false }));
        }
        let child = self
            .store
            .agent(&AgentKey::new(self.store.own_host(), row.child_id.clone()))?;
        Ok(Judged::Send(Box::new(Envelope {
            id: delivery_envelope_id(&row),
            context: None,
            from: Some(Sender {
                value: Some(sender::Value::Agent(AgentSender {
                    agent: Some(AgentRef {
                        host_id: self.store.own_host().to_vec(),
                        agent_id: row.child_id.clone(),
                    }),
                    name: child.map(|child| child.name).unwrap_or_default(),
                })),
            }),
            to: Some(AgentRef {
                host_id: row.parent.host.clone(),
                agent_id: row.parent.agent.clone(),
            }),
            body: Some(envelope::Body::Report(ChildReport {
                to_incarnation: row.parent_incarnation,
                from_incarnation: row.incarnation,
                end: Some(row.end.clone()),
            })),
        })))
    }

    /// Hands a report to its parent: through the parent's lane here, or to
    /// the parent's own daemon as a message from the child.
    fn hand_off(&mut self, parent: &Parent, envelope: Envelope, out: &mut Vec<Output>) {
        let incarnation = match &envelope.body {
            Some(envelope::Body::Report(report)) => Some(report.to_incarnation),
            _ => None,
        };
        self.touched_hand_off(&envelope.id);
        if parent.0 == self.store.own_host() {
            let Some(agent) = AgentId::from_slice(&parent.1) else {
                return;
            };
            let message = Message {
                envelope,
                resume: false,
                incarnation,
                whose: Whose::Report,
            };
            self.enqueue(agent, message, out);
            return;
        }
        let Some(host) = HostId::from_slice(&parent.0) else {
            return;
        };
        let made = self.pending.peers.mint();
        self.pending.peers.waiting.insert(
            made,
            Waiting::Report {
                parent: AgentKey::new(parent.0.clone(), parent.1.clone()),
                id: envelope.id.clone(),
            },
        );
        out.push(Output::Call {
            host,
            call: made,
            request: PeerRequest::SendMessage(envelope),
            absorbed: true,
        });
    }

    /// What the parent's host answered to a report sent there. Its answers
    /// read as the lane's would: not found means the incarnation or the
    /// parent is gone, a refusal keeps the row, a call that never left
    /// means the host is away, and one that lost its answer may have
    /// arrived: the row is tried again and the envelope id catches a
    /// duplicate.
    pub(crate) fn report_answered(
        &mut self,
        parent: &AgentKey,
        id: &[u8],
        answer: Answer,
        out: &mut Vec<Output>,
    ) {
        let outcome = match answer {
            Answer::Reply(_) => Outcome::Accepted { there: true },
            Answer::Failed(PeerFailure::Refused(error)) => match error.code() {
                ErrorCode::NotFound => Outcome::Stale { there: true },
                ErrorCode::Aborted | ErrorCode::Unavailable => Outcome::Kept { unreachable: true },
                code => {
                    tracing::debug!(?code, message = %error.message, "a parent's host turned a delivery away; it stays");
                    Outcome::Kept { unreachable: false }
                }
            },
            Answer::Failed(PeerFailure::NotSent | PeerFailure::Lapsed | PeerFailure::Uncertain)
            | Answer::Session(_)
            | Answer::Inventory(_)
            | Answer::Ended => Outcome::Kept { unreachable: true },
        };
        self.report_concluded(parent, id, outcome);
        self.settle_deliveries(out);
    }

    /// A row handed to its parent came out: deleted, or counted kept.
    fn report_concluded(&mut self, parent: &AgentKey, id: &[u8], outcome: Outcome) {
        let removal = match outcome {
            Outcome::Accepted { there: false } => Some(Removal::Accepted),
            Outcome::Accepted { there: true } => Some(Removal::AcceptedThere),
            Outcome::Stale { there: false } => Some(Removal::Stale),
            Outcome::Stale { there: true } => Some(Removal::StaleThere),
            Outcome::Kept { .. } => None,
        };
        if let Some(removal) = removal {
            let removed = self.delivery_row(id).and_then(|row| match row {
                Some(row) => self.remove_delivery(&row, removal),
                None => Ok(()),
            });
            if let Err(error) = removed {
                tracing::warn!(%error, "deleting a delivered outbox row failed");
            }
        }
        let parent = (parent.host.clone(), parent.agent.clone());
        if let Some(pass) = self.deliveries.pass.as_mut()
            && pass.in_hand.get(&parent).is_some_and(|held| held == id)
        {
            pass.in_hand.remove(&parent);
        }
        self.count(&parent, outcome);
    }

    /// Counts a row's outcome in the pass. A parent found unreachable has
    /// the rest of its rows kept for the next pass.
    fn count(&mut self, parent: &Parent, outcome: Outcome) {
        let Some(pass) = self.deliveries.pass.as_mut() else {
            return;
        };
        match outcome {
            Outcome::Accepted { .. } => pass.report.delivered += 1,
            Outcome::Stale { .. } => pass.report.stale += 1,
            Outcome::Kept { unreachable } => {
                pass.report.kept += 1;
                if unreachable && let Some(rest) = pass.queued.remove(parent) {
                    pass.report.kept += rest.len();
                }
            }
        }
    }

    fn remove_delivery(&mut self, row: &Delivery, removal: Removal) -> Result<(), StoreError> {
        self.store.remove_delivery(row)?;
        self.touched.outbox_removed.push((
            row.parent.clone(),
            delivery_envelope_id(row),
            row.parent_incarnation,
            removal,
        ));
        Ok(())
    }

    /// Records a report going to its parent, and whether one with its
    /// envelope id was already in hand: in a lane here, under way or
    /// queued, or sent to its parent's host and not yet answered.
    fn touched_hand_off(&mut self, id: &[u8]) {
        let in_lane = self.deliveries.lanes.values().any(|lane| {
            lane.current
                .iter()
                .map(|current| &current.message)
                .chain(&lane.queued)
                .any(|message| message.envelope.id == id)
        });
        let sent = self
            .pending
            .peers
            .waiting
            .values()
            .any(|waiting| matches!(waiting, Waiting::Report { id: sent, .. } if sent == id));
        self.touched.handed.push((id.to_vec(), in_lane || sent));
    }

    // --- the lanes -------------------------------------------------------------

    /// A message a client, an own agent or a paired host sent to one of
    /// this host's agents. The sender is stamped here from who called,
    /// since the request cannot name one; a paired host stamped its own.
    /// Answered once the recipient has accepted.
    pub(crate) fn send_message(
        &mut self,
        call: CallId,
        mut envelope: Envelope,
        from: MessageFrom,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let refused = |refusal| CallError::Message(refusal);
        let to = envelope
            .to
            .clone()
            .ok_or(refused(MessageRefusal::NoAgent))?;
        if to.host_id != self.store.own_host() {
            return Err(refused(MessageRefusal::OtherHost));
        }
        let agent = AgentId::from_slice(&to.agent_id).ok_or(refused(MessageRefusal::NoAgent))?;
        let recipient = self
            .store
            .agent(&self.own_key(agent))?
            .ok_or(refused(MessageRefusal::NoAgent))?;
        let (sender, parent) = match from {
            MessageFrom::Person | MessageFrom::PeerPerson => (sender::Value::Human(Human {}), None),
            MessageFrom::Agent(caller) => {
                let key = self.own_key(caller);
                let name = self.store.agent(&key)?.map(|row| row.name);
                let sender = AgentSender {
                    agent: Some(AgentRef {
                        host_id: self.store.own_host().to_vec(),
                        agent_id: caller.0.to_vec(),
                    }),
                    name: name.unwrap_or_default(),
                };
                (sender::Value::Agent(sender), Some(key))
            }
            MessageFrom::Peer(sender) => {
                let agent = sender.agent.clone().unwrap_or_default();
                let key = AgentKey::new(agent.host_id, agent.agent_id);
                (sender::Value::Agent(sender), Some(key))
            }
        };
        envelope.from = Some(Sender {
            value: Some(sender),
        });
        // A child's report is for one incarnation of its parent.
        let incarnation = match &envelope.body {
            Some(envelope::Body::Report(report)) => Some(report.to_incarnation),
            _ => None,
        };
        let message = Message {
            envelope,
            resume: parent.is_some() && recipient.parent == parent,
            incarnation,
            whose: Whose::Call(call),
        };
        self.enqueue(agent, message, out);
        Ok(())
    }

    fn enqueue(&mut self, agent: AgentId, message: Message, out: &mut Vec<Output>) {
        self.deliveries
            .lanes
            .entry(agent)
            .or_default()
            .queued
            .push_back(message);
        self.lane_next(agent, out);
    }

    /// Begins the agent's next message, and the one after it while each is
    /// answered at once.
    fn lane_next(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        loop {
            if !self.families.swept() {
                return;
            }
            let Some(lane) = self.deliveries.lanes.get_mut(&agent) else {
                return;
            };
            if lane.current.is_some() {
                return;
            }
            let Some(message) = lane.queued.pop_front() else {
                self.deliveries.lanes.remove(&agent);
                return;
            };
            match self.begin_message(agent, &message, out) {
                Ok(Some(stage)) => {
                    let envelope = (stage == Stage::Resuming).then(|| message.envelope.clone());
                    if let Some(lane) = self.deliveries.lanes.get_mut(&agent) {
                        lane.current = Some(InLane { message, stage });
                    }
                    // Asked once the lane holds the message, so a registry
                    // that refuses at once concludes it.
                    if let Some(envelope) = envelope {
                        self.resume_with(agent, &envelope, out);
                    }
                    return;
                }
                Ok(None) => self.answer_message(agent, &message, Ok(()), out),
                Err(refusal) => self.answer_message(agent, &message, Err(refusal), out),
            }
        }
    }

    /// Every lane waiting for the start's sweep begins.
    pub(crate) fn lanes_swept(&mut self, out: &mut Vec<Output>) {
        let agents: Vec<AgentId> = self.deliveries.lanes.keys().copied().collect();
        for agent in agents {
            self.lane_next(agent, out);
        }
    }

    /// Begins a message: answered at once when it was already accepted or
    /// cannot be, else the stage it waits in.
    fn begin_message(
        &mut self,
        agent: AgentId,
        message: &Message,
        out: &mut Vec<Output>,
    ) -> Result<Option<Stage>, MessageRefusal> {
        let key = self.own_key(agent);
        let store = |error: StoreError| MessageRefusal::Store(error.to_string());
        if self
            .store
            .item_by_input(&key, &message.envelope.id)
            .map_err(store)?
            .is_some()
        {
            return Ok(None);
        }
        let row = self
            .store
            .agent(&key)
            .map_err(store)?
            .ok_or(MessageRefusal::NoAgent)?;
        // Checked here rather than as it arrived: a message ahead of it in
        // the lane may have resumed the recipient.
        if message
            .incarnation
            .is_some_and(|incarnation| incarnation != row.incarnation)
        {
            return Err(MessageRefusal::Stale);
        }
        if row.lifecycle == Lifecycle::Exited as i32 {
            return if message.resume {
                Ok(Some(Stage::Resuming))
            } else {
                Err(MessageRefusal::Rejected(refused(Refused::Exited)))
            };
        }
        Ok(Some(self.offer(agent, message, out)))
    }

    /// Writes the message to the recipient's process once it is connected.
    fn offer(&mut self, agent: AgentId, message: &Message, out: &mut Vec<Output>) -> Stage {
        if !self.families.connected(agent) {
            self.arm_after(TimerId::Lane(agent), CONNECT_PATIENCE_MS, out);
            return Stage::Connecting;
        }
        out.push(Output::WriteControl {
            agent,
            frame: wire::CtlFrame {
                of: Some(ctl_frame::Of::AgentMessage(message.envelope.clone())),
            },
        });
        self.arm_after(TimerId::Lane(agent), self.settings.reply_patience_ms, out);
        Stage::Verdict
    }

    /// Resumes an exited child with its parent's message as the new
    /// incarnation's first input, once the lane holds it as resuming: the
    /// registry builds its next spec from the settings it last ran with
    /// and starts it.
    fn resume_with(&mut self, agent: AgentId, envelope: &Envelope, out: &mut Vec<Output>) {
        let resume = crate::Resume {
            agent,
            bootstrap: Some(Bootstrap {
                of: Some(bootstrap::Of::AgentMessage(envelope.clone())),
            }),
        };
        self.registry_call(
            Caller::Lane,
            agent,
            crate::families::Op::Resume(Box::new(resume)),
            out,
        );
    }

    fn stage(&self, agent: AgentId) -> Option<Stage> {
        Some(self.deliveries.lanes.get(&agent)?.current.as_ref()?.stage)
    }

    fn set_stage(&mut self, agent: AgentId, stage: Stage) {
        if let Some(current) = self
            .deliveries
            .lanes
            .get_mut(&agent)
            .and_then(|lane| lane.current.as_mut())
        {
            current.stage = stage;
        }
    }

    /// Whether the message under way has its acceptance item committed.
    fn accepted(&self, agent: AgentId) -> bool {
        let Some(current) = self
            .deliveries
            .lanes
            .get(&agent)
            .and_then(|lane| lane.current.as_ref())
        else {
            return false;
        };
        self.store
            .item_by_input(&self.own_key(agent), &current.message.envelope.id)
            .ok()
            .flatten()
            .is_some()
    }

    /// Waits for the acceptance item, unless it is in already.
    fn await_acceptance(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if self.accepted(agent) {
            return self.conclude(agent, Ok(()), out);
        }
        self.set_stage(agent, Stage::Accepting);
        self.arm_after(TimerId::Lane(agent), ACCEPT_PATIENCE_MS, out);
    }

    /// Ends the message under way and begins the next.
    fn conclude(
        &mut self,
        agent: AgentId,
        result: Result<(), MessageRefusal>,
        out: &mut Vec<Output>,
    ) {
        let Some(current) = self
            .deliveries
            .lanes
            .get_mut(&agent)
            .and_then(|lane| lane.current.take())
        else {
            return;
        };
        out.push(Output::CancelTimer {
            id: TimerId::Lane(agent),
        });
        self.answer_message(agent, &current.message, result, out);
        self.lane_next(agent, out);
    }

    /// Answers whoever waits for a message.
    fn answer_message(
        &mut self,
        agent: AgentId,
        message: &Message,
        result: Result<(), MessageRefusal>,
        out: &mut Vec<Output>,
    ) {
        match message.whose {
            Whose::Call(call) => out.push(Output::EndCall {
                call,
                status: match result {
                    Ok(()) => Status::Ok,
                    Err(refusal) => Status::Failed(CallError::Message(refusal)),
                },
            }),
            Whose::Report => {
                let outcome = match result {
                    Ok(()) => Outcome::Accepted { there: false },
                    Err(MessageRefusal::Stale) => Outcome::Stale { there: false },
                    Err(MessageRefusal::Lost | MessageRefusal::Unavailable) => {
                        Outcome::Kept { unreachable: true }
                    }
                    Err(refusal) => {
                        tracing::debug!(?refusal, "a parent turned a delivery away; it stays");
                        Outcome::Kept { unreachable: false }
                    }
                };
                let parent = self.own_key(agent);
                self.report_concluded(&parent, &message.envelope.id, outcome);
            }
        }
    }

    /// The recipient's process said Hello: a message waiting for it is
    /// written.
    pub(crate) fn lane_connected(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if self.stage(agent) != Some(Stage::Connecting) {
            return;
        }
        let Some(message) = self
            .deliveries
            .lanes
            .get(&agent)
            .and_then(|lane| lane.current.as_ref())
            .map(|current| current.message.clone())
        else {
            return;
        };
        let stage = self.offer(agent, &message, out);
        self.set_stage(agent, stage);
    }

    /// The recipient's exit is recorded, with everything its process wrote
    /// committed before it.
    pub(crate) fn lane_exited(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        match self.stage(agent) {
            // Nothing was written: the message begins again, against a row
            // that now says exited.
            Some(Stage::Connecting) => {
                let Some(lane) = self.deliveries.lanes.get_mut(&agent) else {
                    return;
                };
                if let Some(current) = lane.current.take() {
                    lane.queued.push_front(current.message);
                }
                out.push(Output::CancelTimer {
                    id: TimerId::Lane(agent),
                });
                self.lane_next(agent, out);
            }
            Some(Stage::Verdict) => self.conclude(agent, Err(MessageRefusal::Lost), out),
            Some(Stage::Accepting) => {
                let result = if self.accepted(agent) {
                    Ok(())
                } else {
                    Err(MessageRefusal::Lost)
                };
                self.conclude(agent, result, out);
            }
            Some(Stage::Resuming) | None => {}
        }
    }

    /// The interpreter's verdict on a message, or none: the connection
    /// ended before it came.
    pub(crate) fn verdict(
        &mut self,
        agent: AgentId,
        input_id: &[u8],
        verdict: Option<SendInputResponse>,
        out: &mut Vec<Output>,
    ) {
        let Some(current) = self
            .deliveries
            .lanes
            .get(&agent)
            .and_then(|lane| lane.current.as_ref())
        else {
            return;
        };
        if current.stage != Stage::Verdict || current.message.envelope.id != input_id {
            return;
        }
        let resume = current.message.resume;
        let envelope = current.message.envelope.clone();
        match verdict.and_then(|verdict| verdict.of) {
            None => self.conclude(agent, Err(MessageRefusal::Lost), out),
            Some(send_input_response::Of::Accepted(_)) => self.await_acceptance(agent, out),
            Some(send_input_response::Of::Rejected(rejected))
                if rejected.refusal() == Refused::Exiting =>
            {
                self.exiting(agent);
                if !resume {
                    return self.conclude(
                        agent,
                        Err(MessageRefusal::Rejected(refused(Refused::Exited))),
                        out,
                    );
                }
                out.push(Output::CancelTimer {
                    id: TimerId::Lane(agent),
                });
                self.set_stage(agent, Stage::Resuming);
                self.resume_with(agent, &envelope, out);
            }
            Some(send_input_response::Of::Rejected(rejected)) => {
                self.conclude(agent, Err(MessageRefusal::Rejected(rejected)), out)
            }
        }
    }

    /// The agent's journal committed: a message waiting for its acceptance
    /// item may have it.
    pub(crate) fn lane_committed(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if matches!(self.stage(agent), Some(Stage::Accepting | Stage::Resuming))
            && self.accepted(agent)
        {
            self.conclude(agent, Ok(()), out);
        }
    }

    /// The message under way ran out of patience.
    pub(crate) fn lane_timer(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let result = match self.stage(agent) {
            Some(Stage::Connecting) => Err(MessageRefusal::Unavailable),
            Some(Stage::Verdict) => {
                // Ahead of anything the conclusion writes, so a retry of
                // the message waits for a verdict of its own.
                if let Some(current) = self
                    .deliveries
                    .lanes
                    .get(&agent)
                    .and_then(|lane| lane.current.as_ref())
                {
                    out.push(Output::ForgetVerdict {
                        agent,
                        input_id: current.message.envelope.id.clone(),
                    });
                }
                Err(MessageRefusal::Lost)
            }
            Some(Stage::Accepting) if self.accepted(agent) => Ok(()),
            Some(Stage::Accepting) => Err(MessageRefusal::Lost),
            Some(Stage::Resuming) | None => return,
        };
        self.conclude(agent, result, out);
    }

    /// The registry finished a resume a message asked for.
    pub(crate) fn lane_resumed(
        &mut self,
        agent: AgentId,
        outcome: Result<(), Refusal>,
        out: &mut Vec<Output>,
    ) {
        if self.stage(agent) != Some(Stage::Resuming) {
            return;
        }
        match outcome {
            Ok(()) => self.await_acceptance(agent, out),
            Err(refusal) => self.conclude(agent, Err(MessageRefusal::Resume(refusal)), out),
        }
    }

    /// A caller let its message go: one not yet written goes with it.
    pub(crate) fn message_cancelled(&mut self, call: CallId, out: &mut Vec<Output>) {
        let mut connecting = None;
        for (agent, lane) in &mut self.deliveries.lanes {
            lane.queued
                .retain(|message| message.whose != Whose::Call(call));
            if lane.current.as_ref().is_some_and(|current| {
                current.message.whose == Whose::Call(call) && current.stage == Stage::Connecting
            }) {
                lane.current = None;
                connecting = Some(*agent);
            }
        }
        if let Some(agent) = connecting {
            out.push(Output::CancelTimer {
                id: TimerId::Lane(agent),
            });
            self.lane_next(agent, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use store::{AgentRow, Sqlite};

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{DumpReason, Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const FAR: HostId = HostId(*b"far-host-0000000");
    const PARENT: AgentId = AgentId(*b"parent-000000000");
    const CHILD: AgentId = AgentId(*b"child-0000000000");

    fn daemon(dir: &tempfile::TempDir) -> Daemon {
        Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                ..StartOptions::default()
            },
        )
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    fn violations(out: &[Output]) -> Vec<&'static str> {
        out.iter()
            .filter_map(|output| match output {
                Output::Dump {
                    reason: DumpReason::Violation(violation),
                } => Some(violation.invariant),
                _ => None,
            })
            .collect()
    }

    fn report_to(parent: AgentKey) -> Delivery {
        Delivery {
            child_id: CHILD.0.to_vec(),
            incarnation: 1,
            turn_id: 1,
            parent,
            parent_incarnation: 1,
            end: wire::child_report::End::TurnEnded(wire::TurnEnded::default()),
        }
    }

    #[test]
    fn a_message_written_to_its_recipient_and_accepted_waits_for_its_item_then_is_lost() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = daemon(&dir);
        daemon
            .store
            .put_agent(&AgentRow::new(daemon.own_key(PARENT), 1, "/src"))
            .unwrap();
        step(&mut daemon, Input::Swept);
        step(
            &mut daemon,
            Input::AgentHello {
                agent: PARENT,
                hello: Default::default(),
            },
        );
        let envelope = Envelope {
            id: b"envelope".to_vec(),
            to: Some(AgentRef {
                host_id: OWN.to_vec(),
                agent_id: PARENT.0.to_vec(),
            }),
            ..Envelope::default()
        };
        let call = CallId(1);
        let out = step(
            &mut daemon,
            Input::ClientCall {
                call,
                request: ClientRequest::SendMessage {
                    envelope: Box::new(envelope),
                    from: MessageFrom::Person,
                },
            },
        );
        assert!(
            out.iter().any(|output| matches!(
                output,
                Output::WriteControl { agent: PARENT, frame }
                    if matches!(frame.of, Some(ctl_frame::Of::AgentMessage(_)))
            )),
            "{out:?}"
        );
        let accepted = SendInputResponse {
            of: Some(send_input_response::Of::Accepted(Default::default())),
        };
        let out = step(
            &mut daemon,
            Input::Verdict {
                agent: PARENT,
                input_id: b"envelope".to_vec(),
                verdict: Some(accepted),
            },
        );
        assert!(
            !out.iter()
                .any(|output| matches!(output, Output::EndCall { .. })),
            "accepted is not answered before its item commits: {out:?}"
        );
        let out = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Lane(PARENT),
            },
        );
        assert!(
            out.contains(&Output::EndCall {
                call,
                status: Status::Failed(CallError::Message(MessageRefusal::Lost)),
            }),
            "{out:?}"
        );
    }

    #[test]
    fn a_message_out_of_reply_patience_has_its_verdict_forgotten_before_its_retry_is_written() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = daemon(&dir);
        daemon
            .store
            .put_agent(&AgentRow::new(daemon.own_key(PARENT), 1, "/src"))
            .unwrap();
        step(&mut daemon, Input::Swept);
        step(
            &mut daemon,
            Input::AgentHello {
                agent: PARENT,
                hello: Default::default(),
            },
        );
        let envelope = Envelope {
            id: b"envelope".to_vec(),
            to: Some(AgentRef {
                host_id: OWN.to_vec(),
                agent_id: PARENT.0.to_vec(),
            }),
            ..Envelope::default()
        };
        let send = |call| Input::ClientCall {
            call: CallId(call),
            request: ClientRequest::SendMessage {
                envelope: Box::new(envelope.clone()),
                from: MessageFrom::Person,
            },
        };
        step(&mut daemon, send(1));
        // The sender's retry waits in the lane behind the first write.
        step(&mut daemon, send(2));
        let out = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Lane(PARENT),
            },
        );
        assert!(
            out.contains(&Output::EndCall {
                call: CallId(1),
                status: Status::Failed(CallError::Message(MessageRefusal::Lost)),
            }),
            "{out:?}"
        );
        // The driver stops waiting on the first write before it writes the
        // retry, so nothing it says of the first write answers the retry.
        let forgotten = out.iter().position(|output| {
            *output
                == Output::ForgetVerdict {
                    agent: PARENT,
                    input_id: b"envelope".to_vec(),
                }
        });
        let retried = out.iter().position(|output| {
            matches!(
                output,
                Output::WriteControl { agent: PARENT, frame }
                    if matches!(&frame.of, Some(ctl_frame::Of::AgentMessage(written)) if written.id == envelope.id)
            )
        });
        assert!(
            matches!((forgotten, retried), (Some(forgotten), Some(retried)) if forgotten < retried),
            "{out:?}"
        );
    }

    #[test]
    fn a_report_handed_to_a_parent_here_while_one_is_in_its_lane_asks_for_a_dump() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = daemon(&dir);
        let parent = daemon.own_key(PARENT);
        daemon
            .store
            .put_agent(&{
                let mut row = AgentRow::new(parent.clone(), 1, "/src");
                row.incarnation = 1;
                row
            })
            .unwrap();
        daemon.store.put_delivery(&report_to(parent)).unwrap();
        step(
            &mut daemon,
            Input::AgentHello {
                agent: PARENT,
                hello: Default::default(),
            },
        );
        let out = step(&mut daemon, Input::Swept);
        let report = out
            .iter()
            .find_map(|output| match output {
                Output::WriteControl {
                    agent: PARENT,
                    frame:
                        wire::CtlFrame {
                            of: Some(ctl_frame::Of::AgentMessage(envelope)),
                        },
                } => Some(envelope.id.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the report goes through the parent's lane: {out:?}"));
        assert_eq!(violations(&out), Vec::<&str>::new());

        // Its verdict lost, the report is concluded lost and kept, and the
        // next pass hands it over again: once at a time.
        let lost = step(
            &mut daemon,
            Input::Verdict {
                agent: PARENT,
                input_id: report.clone(),
                verdict: None,
            },
        );
        assert_eq!(violations(&lost), Vec::<&str>::new());
        let again = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Deliveries,
            },
        );
        assert!(
            again.iter().any(|output| matches!(
                output,
                Output::WriteControl { agent: PARENT, frame }
                    if matches!(&frame.of, Some(ctl_frame::Of::AgentMessage(envelope)) if envelope.id == report)
            )),
            "{again:?}"
        );
        assert_eq!(violations(&again), Vec::<&str>::new());

        // A pass that lost track of the report it handed over, as only a
        // core whose state went wrong could, hands it again while it is in
        // the lane.
        daemon.deliveries.pass = None;
        let out = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Deliveries,
            },
        );
        assert_eq!(violations(&out), vec![crate::REPORT_HANDED_TWICE]);
    }

    #[test]
    fn a_report_handed_to_its_parents_host_while_one_is_out_asks_for_a_dump() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = daemon(&dir);
        let parent = AgentKey::new(FAR.0.to_vec(), PARENT.0.to_vec());
        daemon
            .store
            .put_agent(&{
                let mut row = AgentRow::new(parent.clone(), 1, "/src");
                row.incarnation = 1;
                row
            })
            .unwrap();
        daemon.store.put_delivery(&report_to(parent)).unwrap();
        step(
            &mut daemon,
            Input::TrustRead {
                store: crate::trust::TrustStore::of_hosts(vec![FAR]),
            },
        );
        let out = step(&mut daemon, Input::Swept);
        assert!(
            out.iter().any(|output| matches!(
                output,
                Output::Call {
                    host: FAR,
                    request: PeerRequest::SendMessage(_),
                    ..
                }
            )),
            "{out:?}"
        );
        assert_eq!(violations(&out), Vec::<&str>::new());

        // A pass that lost track of the report it sent, as only a core
        // whose state went wrong could, sends it again while it is out.
        daemon.deliveries.pass = None;
        let out = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Deliveries,
            },
        );
        assert_eq!(violations(&out), vec![crate::REPORT_HANDED_TWICE]);
    }
}
