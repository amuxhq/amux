//! The verbs, fences and observations a story uses, whichever net it runs
//! on: the real one, production daemons on loopback ([`Net`]), or the
//! simulated one, every core in one process stepped by a seed
//! ([`crate::SimNet`]).
//!
//! A story written against [`World`] runs unchanged in both. The two keep
//! one meaning for every wait: it passes only when its condition holds,
//! and fails when the condition cannot come true. A real net knows that by
//! its deadline; a simulated one knows it exactly, once nothing is left to
//! deliver without the story acting.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

use crate::net::{Ack, Net, NetAgent, NetError};
use crate::observe::{self, InventoryObserver, Observer};
use crate::topology::AgentDecl;

/// A net a story drives.
#[allow(
    async_fn_in_trait,
    reason = "stories are generic over the world and await it in place; none is sent between threads"
)]
pub trait World {
    /// An agent the net knows by name.
    fn agent(&self, name: &str) -> Result<&NetAgent, NetError>;
    /// Whether the host's daemon runs.
    fn is_up(&self, host: &str) -> bool;
    /// The generation the host's own rows are served under.
    fn generation(&self, host: &str) -> Result<u64, NetError>;
    /// Policy time, which moves only when the story advances it.
    fn now_ms(&self) -> i64;
    fn advance(&self, by: Duration) -> Result<Ack, NetError>;
    /// Lets every script waiting on the gate go on.
    fn open_gate(&self, name: &str) -> Result<Ack, NetError>;
    /// Where the agent's journal ends now.
    fn journal_end(&self, agent: &str) -> Result<u64, NetError>;

    /// Cuts the link the way a dead connection goes.
    fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError>;
    async fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError>;
    /// Whether both ends of a link route to each other directly now.
    async fn link_up(&self, a: &str, b: &str) -> bool;
    /// Waits until the link is up at both ends, or down at both.
    async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError>;

    /// Crashes the host's daemon; its agents run on.
    async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError>;
    /// Stops the host's daemon cleanly; its agents run on.
    async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError>;
    /// Starts the host's daemon again from what it holds, and relinks it.
    async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError>;

    /// Sends the agent a prompt through its own host.
    async fn send(&self, agent: &str, text: &str) -> Result<wire::SendInputResponse, NetError>;
    /// Sends the agent any input through its own host.
    async fn input(
        &self,
        agent: &str,
        input: wire::Input,
    ) -> Result<wire::SendInputResponse, NetError>;

    /// Starts `decl` as a child of `parent` on the host it names; the
    /// returned row is the child's own once its process said Hello.
    async fn spawn_child(&mut self, parent: &str, decl: AgentDecl)
    -> Result<wire::Agent, NetError>;
    /// Deletes the agent on its own host, as a client does there. Its
    /// children follow on their own hosts by the family rule, after this
    /// returns; the net forgets only this agent.
    async fn delete(&mut self, agent: &str) -> Result<Ack, NetError>;
    /// Stops the agent on its own host, as a client does there, and waits
    /// until it has exited.
    async fn stop(&self, agent: &str) -> Result<Ack, NetError>;
    /// Starts an exited agent's next incarnation on its own host, as a
    /// client does there, seeded with `prompt`; its process plays the same
    /// script again.
    async fn resume(&mut self, agent: &str, prompt: Option<&str>) -> Result<wire::Agent, NetError>;
    /// Sends `envelope` to host `to` as host `from`'s daemon sends a
    /// message on its link: the sender it names is taken as given. A
    /// refusal is [`NetError::Refused`] with the code a peer reads.
    async fn peer_message(
        &self,
        from: &str,
        to: &str,
        envelope: wire::Envelope,
    ) -> Result<wire::SendMessageResponse, NetError>;

    /// Opens a Subscribe stream on the agent at the host and records it.
    async fn observe(&self, host: &str, agent: &str, tail: u32) -> Result<Observer, NetError>;
    /// Opens a SubscribeInventory stream on the host and records it.
    async fn observe_inventory(&self, host: &str) -> Result<InventoryObserver, NetError>;

    /// Waits until the host holds the agent's row at `order`.
    async fn fence(&self, host: &str, agent: &str, order: u64) -> Result<(), NetError>;
    /// Waits until the host holds what the agent's own host holds.
    async fn current(&self, host: &str, agent: &str) -> Result<(), NetError>;
    /// Waits until the agent's `k`th turn has ended on its own host, and
    /// returns the order of the row that ended it.
    async fn turn_ended(&self, agent: &str, k: usize) -> Result<u64, NetError>;
    /// Waits until the input accepted under `input_id` has settled on the
    /// agent's own host, and returns the order of the row reflecting it.
    async fn input_settled(&self, agent: &str, input_id: &[u8]) -> Result<u64, NetError>;
    /// Reads the host's store between its daemon's steps; a host that is
    /// down is an error.
    async fn read_store<T: Send + 'static>(
        &self,
        host: &str,
        read: impl FnOnce(&store::Sqlite) -> T + Send + 'static,
    ) -> Result<T, NetError>;

    /// Fails unless the host's block of the agent's rows is whole against
    /// what the agent's own host holds.
    async fn block_whole(&self, host: &str, agent: &str) -> Result<(), NetError>;
    /// A page of the agent's older items, as a client on the host asks for
    /// it: from the host's own rows, or below them from the agent's host.
    async fn fetch(
        &self,
        host: &str,
        agent: &str,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<wire::ListItemsResponse, NetError>;
    /// Sets which replica agents keep a source open on the host, as its
    /// app does going to the background and back.
    fn set_source_policy(&self, host: &str, policy: node::SourcePolicy) -> Result<Ack, NetError>;
    /// The replica agents with a source open on the host now, in order.
    async fn open_sources(&self, host: &str) -> Result<Vec<store::AgentKey>, NetError>;
    /// Whether `at` has caught up on a live inventory stream from `host`,
    /// so pages of its agents' history are asked of it.
    async fn host_ready(&self, at: &str, host: &str) -> Result<bool, NetError>;

    /// Waits until `probe` answers `Ok`, and returns what it answered.
    async fn until<T, E, F>(&self, what: &str, probe: impl FnMut() -> F) -> Result<T, NetError>
    where
        E: Display,
        F: Future<Output = Result<T, E>>;
    /// Fails if `check` is ever false while the net moves: for `window` on
    /// a real net, until nothing is left to deliver on a simulated one.
    async fn holds_for<F>(
        &self,
        what: &str,
        window: Duration,
        check: impl FnMut() -> F,
    ) -> Result<(), NetError>
    where
        F: Future<Output = bool>;

    /// Ends the net, saying whether it broke anything on the way.
    async fn shutdown(self) -> Result<(), NetError>;
}

impl World for Net {
    fn agent(&self, name: &str) -> Result<&NetAgent, NetError> {
        Net::agent(self, name)
    }

    fn is_up(&self, host: &str) -> bool {
        Net::is_up(self, host)
    }

    fn generation(&self, host: &str) -> Result<u64, NetError> {
        Net::generation(self, host)
    }

    fn now_ms(&self) -> i64 {
        Net::now_ms(self)
    }

    fn advance(&self, by: Duration) -> Result<Ack, NetError> {
        Net::advance(self, by)
    }

    fn open_gate(&self, name: &str) -> Result<Ack, NetError> {
        Net::open_gate(self, name)
    }

    fn journal_end(&self, agent: &str) -> Result<u64, NetError> {
        Net::journal_end(self, agent)
    }

    fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        Net::sever_link(self, a, b)
    }

    async fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        Net::restore_link(self, a, b).await
    }

    async fn link_up(&self, a: &str, b: &str) -> bool {
        Net::link_up(self, a, b).await
    }

    async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError> {
        Net::wait_link(self, a, b, up).await
    }

    async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        Net::kill_daemon(self, host).await
    }

    async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        Net::stop_daemon(self, host).await
    }

    async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        Net::restart_daemon(self, host).await
    }

    async fn send(&self, agent: &str, text: &str) -> Result<wire::SendInputResponse, NetError> {
        Net::send(self, agent, text).await
    }

    async fn input(
        &self,
        agent: &str,
        input: wire::Input,
    ) -> Result<wire::SendInputResponse, NetError> {
        Net::input(self, agent, input).await
    }

    async fn spawn_child(
        &mut self,
        parent: &str,
        decl: AgentDecl,
    ) -> Result<wire::Agent, NetError> {
        Net::spawn_child(self, parent, decl).await
    }

    async fn delete(&mut self, agent: &str) -> Result<Ack, NetError> {
        Net::delete(self, agent).await
    }

    async fn stop(&self, agent: &str) -> Result<Ack, NetError> {
        Net::stop(self, agent).await
    }

    async fn resume(&mut self, agent: &str, prompt: Option<&str>) -> Result<wire::Agent, NetError> {
        Net::resume(self, agent, prompt).await
    }

    async fn peer_message(
        &self,
        from: &str,
        to: &str,
        envelope: wire::Envelope,
    ) -> Result<wire::SendMessageResponse, NetError> {
        let to = self.host(to)?.host_id;
        let mut peer = self
            .edge(from)?
            .peer(to)
            .await
            .map_err(|error| NetError::Host {
                host: from.to_owned(),
                error: format!("no channel to {to}: {error}"),
            })?;
        peer.agents
            .send_message(envelope)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| NetError::Refused(Box::new(status)))
    }

    async fn observe(&self, host: &str, agent: &str, tail: u32) -> Result<Observer, NetError> {
        Net::observe(self, host, agent, tail).await
    }

    async fn observe_inventory(&self, host: &str) -> Result<InventoryObserver, NetError> {
        Net::observe_inventory(self, host).await
    }

    async fn fence(&self, host: &str, agent: &str, order: u64) -> Result<(), NetError> {
        Net::fence(self, host, agent, order).await
    }

    async fn current(&self, host: &str, agent: &str) -> Result<(), NetError> {
        Net::current(self, host, agent).await
    }

    async fn turn_ended(&self, agent: &str, k: usize) -> Result<u64, NetError> {
        Net::turn_ended(self, agent, k).await
    }

    async fn input_settled(&self, agent: &str, input_id: &[u8]) -> Result<u64, NetError> {
        Net::input_settled(self, agent, input_id).await
    }

    async fn read_store<T: Send + 'static>(
        &self,
        host: &str,
        read: impl FnOnce(&store::Sqlite) -> T + Send + 'static,
    ) -> Result<T, NetError> {
        self.runtime(host)?
            .read_store(read)
            .await
            .map_err(|_| NetError::Down(host.to_owned()))
    }

    async fn block_whole(&self, host: &str, agent: &str) -> Result<(), NetError> {
        Net::assert_block_invariant(self, host, agent).await
    }

    async fn fetch(
        &self,
        host: &str,
        agent: &str,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<wire::ListItemsResponse, NetError> {
        let id = Net::agent(self, agent)?.id;
        Ok(self
            .runtime(host)?
            .list_items(&wire::ListItemsRequest {
                agent_id: id.as_bytes().to_vec(),
                before_order,
                page_size: limit,
            })
            .await?)
    }

    fn set_source_policy(&self, host: &str, policy: node::SourcePolicy) -> Result<Ack, NetError> {
        self.runtime(host)?.set_source_policy(policy);
        Ok(Ack {
            installed: format!("{host}'s source policy {policy:?}"),
            at_ms: Net::now_ms(self),
        })
    }

    async fn open_sources(&self, host: &str) -> Result<Vec<store::AgentKey>, NetError> {
        Ok(self.runtime(host)?.open_sources())
    }

    async fn host_ready(&self, at: &str, host: &str) -> Result<bool, NetError> {
        let id = self.host(host)?.host_id;
        Ok(self.runtime(at)?.host_ready(id))
    }

    async fn until<T, E, F>(&self, what: &str, probe: impl FnMut() -> F) -> Result<T, NetError>
    where
        E: Display,
        F: Future<Output = Result<T, E>>,
    {
        Ok(observe::until(what, probe).await?)
    }

    async fn holds_for<F>(
        &self,
        what: &str,
        window: Duration,
        check: impl FnMut() -> F,
    ) -> Result<(), NetError>
    where
        F: Future<Output = bool>,
    {
        Ok(observe::holds_for(what, window, check).await?)
    }

    async fn shutdown(self) -> Result<(), NetError> {
        Net::shutdown(self).await
    }
}
