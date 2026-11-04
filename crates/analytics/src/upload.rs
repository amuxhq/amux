//! The installation's one uploader: a bounded queue every profile's handle
//! pushes to, flushed in batches per host, retried a little and then
//! dropped. Nothing is written to disk; what a crash or a full queue loses
//! is lost.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::context::{Context, Endpoint, events_url};
use crate::{HostId, Recorded, Sink};

/// How the uploader batches, retries and gives up.
#[derive(Clone, Debug)]
pub struct Params {
    /// A batch waits at most this long for more events.
    pub flush_every: Duration,
    /// A batch this large is sent at once.
    pub batch: usize,
    /// Events waiting beyond this many are dropped.
    pub queue: usize,
    /// Tries after the first, each after a longer wait.
    pub retries: u32,
    /// The wait before the first retry; each later one doubles it.
    pub backoff: Duration,
    /// One request's limit.
    pub request_timeout: Duration,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            flush_every: Duration::from_secs(60),
            batch: 50,
            queue: 1000,
            retries: 2,
            backoff: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }
}

/// The server takes at most this many events in one request.
pub const MAX_EVENTS_PER_REQUEST: usize = 100;

/// Whether sending is still wanted, asked before every send: the person
/// may have turned telemetry off since the uploader started.
pub type Gate = Arc<dyn Fn() -> bool + Send + Sync>;

/// What the uploader needs to know about the installation.
pub struct Upload {
    pub installation_id: Uuid,
    pub context: Context,
    pub endpoint: Endpoint,
    /// Where an unbound profile's events go under [`Endpoint::Accounts`].
    pub default_base: String,
    pub gate: Gate,
    /// Whether a client has shown the person the notice. Until one has,
    /// nothing is sent: events wait in memory, as many as the queue holds,
    /// and go once it has.
    pub notice: Gate,
}

/// The installation's profiles, as the uploader asks after them.
#[async_trait::async_trait]
pub trait Accounts: Send + Sync {
    /// The account service a host's profile is signed in to: `Some(None)`
    /// for a profile with none, `None` once the host is no longer served
    /// here and its events are dropped.
    fn service(&self, host: HostId) -> Option<Option<String>>;
    /// A bearer for the host's account service, when it is signed in.
    async fn bearer(&self, host: HostId) -> Option<String>;
}

/// How a request was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posted {
    Accepted,
    /// Worth trying again: the server or the network failed, or it asked
    /// for less.
    Retry,
    /// The bearer was refused; the same events go again without it.
    Unauthorized,
    /// Never worth trying again.
    Refused,
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn post(&self, url: &str, bearer: Option<&str>, body: Vec<u8>) -> Posted;
}

/// Posts over HTTPS.
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new(timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait::async_trait]
impl Transport for Http {
    async fn post(&self, url: &str, bearer: Option<&str>, body: Vec<u8>) -> Posted {
        let mut request = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        match request.send().await {
            Ok(response) => answer(response.status().as_u16()),
            Err(_) => Posted::Retry,
        }
    }
}

fn answer(status: u16) -> Posted {
    match status {
        200..=299 => Posted::Accepted,
        401 => Posted::Unauthorized,
        408 | 429 | 500..=599 => Posted::Retry,
        _ => Posted::Refused,
    }
}

enum Command {
    Event(HostId, Recorded),
    Flush(oneshot::Sender<()>),
}

/// The queue's end the profiles' handles push to.
struct Queue(mpsc::Sender<Command>);

impl Sink for Queue {
    fn record(&self, host: HostId, recorded: Recorded) {
        // Full means the uploader is behind or the network is down: the
        // event is dropped rather than held up or kept.
        let _ = self.0.try_send(Command::Event(host, recorded));
    }
}

/// The running uploader. Dropping it stops it without a flush.
pub struct Uploader {
    commands: mpsc::Sender<Command>,
    task: JoinHandle<()>,
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Uploader {
    pub fn start(
        upload: Upload,
        accounts: Arc<dyn Accounts>,
        transport: Arc<dyn Transport>,
        params: Params,
    ) -> Uploader {
        let (commands, received) = mpsc::channel(params.queue.max(1));
        let task = tokio::spawn(run(upload, accounts, transport, params, received));
        Uploader { commands, task }
    }

    /// Where the profiles' handles record to.
    pub fn sink(&self) -> Arc<dyn Sink> {
        Arc::new(Queue(self.commands.clone()))
    }

    /// Sends everything waiting, giving up after `within`.
    pub async fn flush(&self, within: Duration) {
        self.flusher().flush(within).await;
    }

    /// A way to flush that outlives a borrow of the uploader.
    pub fn flusher(&self) -> Flusher {
        Flusher(self.commands.clone())
    }
}

/// Asks the uploader to send everything waiting.
#[derive(Clone)]
pub struct Flusher(mpsc::Sender<Command>);

impl Flusher {
    /// Sends everything waiting, giving up after `within`.
    pub async fn flush(&self, within: Duration) {
        let (done, flushed) = oneshot::channel();
        let _ = tokio::time::timeout(within, async {
            if self.0.send(Command::Flush(done)).await.is_ok() {
                let _ = flushed.await;
            }
        })
        .await;
    }
}

async fn run(
    upload: Upload,
    accounts: Arc<dyn Accounts>,
    transport: Arc<dyn Transport>,
    params: Params,
    mut received: mpsc::Receiver<Command>,
) {
    let sender = Sender {
        upload,
        accounts,
        transport,
        params,
    };
    let mut waiting: Vec<(HostId, Recorded)> = Vec::new();
    let mut tick = tokio::time::interval(sender.params.flush_every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tokio::select! {
            command = received.recv() => match command {
                None => {
                    if (sender.upload.notice)() {
                        sender.send(std::mem::take(&mut waiting)).await;
                    }
                    return;
                }
                Some(Command::Event(host, recorded)) => {
                    // Checked as each event arrives, not only at send time:
                    // what happens while sharing is off must not go out if
                    // sharing is turned back on before the next flush.
                    if !(sender.upload.gate)() {
                        continue;
                    }
                    waiting.push((host, recorded));
                    if waiting.len() >= sender.params.batch {
                        sender.flush(&mut waiting).await;
                        tick.reset();
                    }
                }
                Some(Command::Flush(done)) => {
                    let mut flushes = vec![done];
                    while let Ok(command) = received.try_recv() {
                        match command {
                            Command::Event(host, recorded) => {
                                if (sender.upload.gate)() {
                                    waiting.push((host, recorded));
                                }
                            }
                            Command::Flush(done) => flushes.push(done),
                        }
                    }
                    sender.flush(&mut waiting).await;
                    tick.reset();
                    for done in flushes {
                        let _ = done.send(());
                    }
                }
            },
            _ = tick.tick() => {
                if !waiting.is_empty() {
                    sender.flush(&mut waiting).await;
                }
            }
        }
    }
}

struct Sender {
    upload: Upload,
    accounts: Arc<dyn Accounts>,
    transport: Arc<dyn Transport>,
    params: Params,
}

impl Sender {
    /// Sends what is waiting once the notice has been shown; until then
    /// keeps it, the newest as many as the queue holds.
    async fn flush(&self, waiting: &mut Vec<(HostId, Recorded)>) {
        if (self.upload.notice)() {
            self.send(std::mem::take(waiting)).await;
            return;
        }
        let over = waiting.len().saturating_sub(self.params.queue.max(1));
        waiting.drain(..over);
    }

    /// Sends one request per host, in the order the hosts first appear.
    async fn send(&self, events: Vec<(HostId, Recorded)>) {
        if events.is_empty() || !(self.upload.gate)() {
            return;
        }
        let mut hosts: Vec<(HostId, Vec<Recorded>)> = Vec::new();
        for (host, recorded) in events {
            match hosts.iter_mut().find(|(seen, _)| *seen == host) {
                Some((_, list)) => list.push(recorded),
                None => hosts.push((host, vec![recorded])),
            }
        }
        for (host, events) in hosts {
            let Some(service) = self.accounts.service(host) else {
                continue;
            };
            let base = self
                .upload
                .endpoint
                .base(service.as_deref(), &self.upload.default_base)
                .to_owned();
            // A bearer goes only to the service that issued it.
            let bearer = match service {
                Some(service) if service.trim_end_matches('/') == base => {
                    self.accounts.bearer(host).await
                }
                _ => None,
            };
            for chunk in events.chunks(MAX_EVENTS_PER_REQUEST) {
                let body = self.body(host, chunk);
                self.post(&events_url(&base), bearer.as_deref(), body).await;
            }
        }
    }

    fn body(&self, host: HostId, events: &[Recorded]) -> Vec<u8> {
        let body = json!({
            "installation_id": self.upload.installation_id.to_string(),
            "host_id": host.to_string(),
            "context": self.upload.context.to_json(),
            "events": events.iter().map(Recorded::to_json).collect::<Vec<Value>>(),
        });
        serde_json::to_vec(&body).expect("a JSON value serialises")
    }

    async fn post(&self, url: &str, mut bearer: Option<&str>, body: Vec<u8>) {
        let mut wait = self.params.backoff;
        let mut tries = 0;
        loop {
            match self.transport.post(url, bearer, body.clone()).await {
                Posted::Accepted | Posted::Refused => return,
                // A stale bearer costs the request its account, not its
                // events.
                Posted::Unauthorized if bearer.is_some() => {
                    bearer = None;
                    continue;
                }
                Posted::Unauthorized => return,
                Posted::Retry => {}
            }
            if tries >= self.params.retries || !(self.upload.gate)() {
                tracing::debug!(url, "analytics: giving up on a batch");
                return;
            }
            tries += 1;
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::SystemTime;

    use super::*;
    use crate::context::Channel;
    use crate::{Analytics, Event};

    #[derive(Default)]
    struct Server {
        requests: Mutex<Vec<(String, Option<String>, Value)>>,
        answers: Mutex<Vec<Posted>>,
    }

    impl Server {
        fn requests(&self) -> Vec<(String, Option<String>, Value)> {
            self.requests.lock().unwrap().clone()
        }

        fn answer_next(&self, answers: &[Posted]) {
            self.answers.lock().unwrap().extend_from_slice(answers);
        }
    }

    #[async_trait::async_trait]
    impl Transport for Server {
        async fn post(&self, url: &str, bearer: Option<&str>, body: Vec<u8>) -> Posted {
            self.requests.lock().unwrap().push((
                url.to_owned(),
                bearer.map(str::to_owned),
                serde_json::from_slice(&body).unwrap(),
            ));
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                Posted::Accepted
            } else {
                answers.remove(0)
            }
        }
    }

    #[derive(Default)]
    struct Profiles(HashMap<HostId, Option<(String, String)>>);

    #[async_trait::async_trait]
    impl Accounts for Profiles {
        fn service(&self, host: HostId) -> Option<Option<String>> {
            self.0
                .get(&host)
                .map(|account| account.as_ref().map(|(service, _)| service.clone()))
        }

        async fn bearer(&self, host: HostId) -> Option<String> {
            self.0
                .get(&host)?
                .as_ref()
                .map(|(_, bearer)| bearer.clone())
        }
    }

    const UNBOUND: HostId = HostId::from_u128(1);
    const BOUND: HostId = HostId::from_u128(2);

    fn profiles() -> Arc<Profiles> {
        let mut profiles = Profiles::default();
        profiles.0.insert(UNBOUND, None);
        profiles.0.insert(
            BOUND,
            Some(("https://amux.sh".to_owned(), "token".to_owned())),
        );
        Arc::new(profiles)
    }

    fn start(server: &Arc<Server>, gate: Gate, endpoint: Endpoint) -> Uploader {
        start_noticed(server, gate, open(), endpoint)
    }

    fn start_noticed(
        server: &Arc<Server>,
        gate: Gate,
        notice: Gate,
        endpoint: Endpoint,
    ) -> Uploader {
        Uploader::start(
            Upload {
                installation_id: Uuid::from_u128(9),
                context: Context::detect("0.8.0", Channel::Stable),
                endpoint,
                default_base: "https://amux.sh".into(),
                gate,
                notice,
            },
            profiles(),
            server.clone(),
            Params {
                queue: 8,
                batch: 3,
                ..Params::default()
            },
        )
    }

    fn accounts() -> Endpoint {
        Endpoint::Accounts
    }

    fn open() -> Gate {
        Arc::new(|| true)
    }

    fn opened() -> Event {
        Event::ClientOpened {
            client: crate::Client::Terminal,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_batch_goes_at_once_and_a_short_one_at_the_minute() {
        let server = Arc::new(Server::default());
        let uploader = start(&server, open(), accounts());
        let handle = Analytics::new(UNBOUND, uploader.sink());
        handle.record(Event::Installed);
        handle.record(opened());
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(server.requests().is_empty(), "two events wait for more");
        handle.record(Event::SignedOut);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(server.requests().len(), 1, "the third fills the batch");
        handle.record(Event::RelayRefused);
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert_eq!(server.requests().len(), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "the rest goes at the minute");
        assert_eq!(requests[1].2["events"][0]["event"], "relay_refused");
    }

    #[tokio::test(start_paused = true)]
    async fn the_body_is_the_contract_and_the_bearer_goes_only_to_its_service() {
        let server = Arc::new(Server::default());
        let uploader = start(&server, open(), accounts());
        Analytics::new(UNBOUND, uploader.sink()).record(Event::Installed);
        Analytics::new(BOUND, uploader.sink()).record(Event::SignedIn {
            via: crate::Via::Browser,
        });
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "one request per host");
        let (url, bearer, body) = &requests[0];
        assert_eq!(url, "https://amux.sh/api/events");
        assert_eq!(bearer, &None);
        assert_eq!(body["installation_id"], Uuid::from_u128(9).to_string());
        assert_eq!(body["host_id"], UNBOUND.to_string());
        assert_eq!(body["context"]["version"], "0.8.0");
        assert_eq!(body["context"]["channel"], "stable");
        assert_eq!(body["events"][0]["event"], "installed");
        assert_eq!(body["events"][0]["properties"], json!({}));
        assert!(
            body["events"][0]["timestamp"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        assert_eq!(requests[1].1.as_deref(), Some("token"));

        // An override elsewhere never carries the account's bearer.
        let server = Arc::new(Server::default());
        let uploader = start(
            &server,
            open(),
            Endpoint::Fixed("http://localhost:5000".into()),
        );
        Analytics::new(BOUND, uploader.sink()).record(Event::SignedIn {
            via: crate::Via::Browser,
        });
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests[0].0, "http://localhost:5000/api/events");
        assert_eq!(requests[0].1, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_drops_and_a_failing_server_is_given_up_on() {
        let server = Arc::new(Server::default());
        // Hold the uploader in a retry so the queue backs up behind it.
        server.answer_next(&[Posted::Retry, Posted::Retry, Posted::Retry]);
        let uploader = start(&server, open(), accounts());
        let handle = Analytics::new(UNBOUND, uploader.sink());
        for _ in 0..3 {
            handle.record(Event::Installed);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        for _ in 0..20 {
            handle.record(opened());
        }
        // The first try and two retries, 5 s then 10 s apart, then dropped.
        tokio::time::sleep(Duration::from_secs(16)).await;
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(
            requests.len(),
            3 + 3,
            "three tries, then what the queue held"
        );
        let delivered: usize = requests[3..]
            .iter()
            .map(|(_, _, body)| body["events"].as_array().unwrap().len())
            .sum();
        assert_eq!(delivered, 8, "the queue held eight; the rest were dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_bearer_is_dropped_and_the_events_sent_without_it() {
        let server = Arc::new(Server::default());
        server.answer_next(&[Posted::Unauthorized]);
        let uploader = start(&server, open(), accounts());
        Analytics::new(BOUND, uploader.sink()).record(Event::SignedIn {
            via: crate::Via::Browser,
        });
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1.as_deref(), Some("token"));
        assert_eq!(requests[1].1, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_gate_sends_nothing() {
        let server = Arc::new(Server::default());
        let on = Arc::new(AtomicBool::new(false));
        let gate = {
            let on = on.clone();
            Arc::new(move || on.load(Ordering::SeqCst))
        };
        let uploader = start(&server, gate, accounts());
        let handle = Analytics::new(UNBOUND, uploader.sink());
        handle.record(Event::Installed);
        uploader.flush(Duration::from_secs(5)).await;
        assert!(server.requests().is_empty());
        on.store(true, Ordering::SeqCst);
        handle.record(Event::SignedOut);
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].2["events"].as_array().unwrap().len(),
            1,
            "what waited while the gate was closed was dropped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn what_happens_while_off_stays_unsent_when_turned_back_on_before_a_flush() {
        let server = Arc::new(Server::default());
        let on = Arc::new(AtomicBool::new(false));
        let gate = {
            let on = on.clone();
            Arc::new(move || on.load(Ordering::SeqCst))
        };
        let uploader = start(&server, gate, accounts());
        let handle = Analytics::new(UNBOUND, uploader.sink());
        handle.record(Event::Installed);
        tokio::task::yield_now().await;
        on.store(true, Ordering::SeqCst);
        handle.record(Event::SignedOut);
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let events = requests[0].2["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "signed_out");
    }

    /// Before a client has shown the notice nothing is sent, however full
    /// the batch or however many minutes pass; what waited goes once it
    /// has, the newest as many as the queue holds.
    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_until_the_notice_is_shown() {
        let server = Arc::new(Server::default());
        let shown = Arc::new(AtomicBool::new(false));
        let notice = {
            let shown = shown.clone();
            Arc::new(move || shown.load(Ordering::SeqCst))
        };
        let uploader = start_noticed(&server, open(), notice, accounts());
        let handle = Analytics::new(UNBOUND, uploader.sink());
        handle.record(Event::Installed);
        for _ in 0..9 {
            tokio::task::yield_now().await;
            handle.record(Event::RelayRefused);
        }
        tokio::time::sleep(Duration::from_secs(120)).await;
        uploader.flush(Duration::from_secs(5)).await;
        assert!(server.requests().is_empty(), "nothing before the notice");
        shown.store(true, Ordering::SeqCst);
        uploader.flush(Duration::from_secs(5)).await;
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let events = requests[0].2["events"].as_array().unwrap();
        assert_eq!(events.len(), 8, "as many as the queue holds");
        assert!(events.iter().all(|event| event["event"] == "relay_refused"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_host_no_longer_served_is_dropped() {
        let server = Arc::new(Server::default());
        let uploader = start(&server, open(), accounts());
        Analytics::new(HostId::from_u128(77), uploader.sink()).record(Event::Installed);
        uploader.flush(Duration::from_secs(5)).await;
        assert!(server.requests().is_empty());
    }

    #[test]
    fn statuses() {
        assert_eq!(answer(202), Posted::Accepted);
        assert_eq!(answer(401), Posted::Unauthorized);
        assert_eq!(answer(429), Posted::Retry);
        assert_eq!(answer(503), Posted::Retry);
        assert_eq!(answer(400), Posted::Refused);
        assert_eq!(answer(413), Posted::Refused);
    }

    #[test]
    fn recorded_at_is_utc_seconds() {
        let recorded = Recorded {
            event: Event::Installed,
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_759_665_600),
        };
        assert_eq!(recorded.to_json()["timestamp"], "2025-10-05T12:00:00Z");
    }
}
