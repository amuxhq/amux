//! The relay's scaling curve: the production relay on loopback, held by 10,
//! 100 and 1,000 hosts at once, measured for the bytes it carries between
//! them and for how long a new stream through it takes to answer.
//!
//! Each level starts a fresh relay serving its TLS-over-TCP carrier, the
//! listener a deployment runs beside QUIC. QUIC is not used: the relay
//! admits ten new QUIC handshakes a minute from one address, and every host
//! here dials from the same loopback address. Hosts come in pairs, one pair
//! to an account, as a person's laptop and desktop do. Every host:
//!
//! - opens [`CurveOptions::opens_per_host`] streams to its partner one after
//!   another, all hosts at once, each timed from the open to the partner's
//!   one-byte answer (stream-open latency);
//! - then opens one stream to its partner and writes into it as fast as the
//!   relay takes the bytes; what every partner reads over a fixed window is
//!   the aggregate throughput.
//!
//! The hosts are bare: one link to the relay and raw streams, with no
//! handshake inside a stream, so the work measured is the relay's and the
//! two carriers' TLS. They share the process and its cores with the relay.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use node::harness::{
    AuthenticatedLinkUser, ByteStream, LinkTokenAuthenticator, RelayHost, SystemClock,
};
use node::{CloudLinkServer, RelayIdentity, Tier};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::{DESKTOP_REFERENCE_STATE, Machine};

/// The concurrent connections the curve is drawn at.
pub const LEVELS: [usize; 3] = [10, 100, 1_000];
/// Where a machine's baseline lives, under the model's directory.
pub const BASELINE_ROOT: &str = "perf/relay";
/// The carrier every host links over, recorded so a baseline is only
/// compared with a run over the same one.
pub const CARRIER: &str = "TLS over TCP";
const SCHEMA: u32 = 1;
const RELAY_NAME: &str = "localhost";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PHASE_TIMEOUT: Duration = Duration::from_secs(180);
/// How many hosts dial at once: the relay finishes 128 TLS handshakes at a
/// time and refuses none, so this only bounds the dialling side's sockets.
const DIAL_CONCURRENCY: usize = 64;
const CHUNK: usize = 64 * 1024;
const PING: u8 = b'p';
const BULK: u8 = b'b';

/// How far a run may fall behind its baseline before the comparison
/// fails. One-sided: a run faster than its baseline passes and the report
/// says by how much. From three consecutive runs of one build on the
/// enrolled machine: throughput moved by up to 4% and the median open by
/// up to 2%, so their bands leave room for a busier machine; the 99th
/// percentile moved by up to 76% (4.2 to 7.4 ms at ten connections, and
/// 349 or 525 ms at a thousand, two modes), so it fails only past double.
pub const THROUGHPUT_BAND: f64 = 0.15;
pub const OPEN_P50_BAND: f64 = 0.25;
pub const OPEN_P99_BAND: f64 = 1.00;

#[derive(Clone, Debug)]
pub struct CurveOptions {
    pub levels: Vec<usize>,
    /// Fresh relays per level. Throughput is the median round's; the open
    /// latencies of every round are pooled, so the 99th percentile at ten
    /// connections rests on a thousand opens rather than two hundred.
    pub rounds: usize,
    /// Streams each host opens for the latency sample. With the bulk
    /// stream it stays under the relay's thirty stream opens a minute per
    /// link.
    pub opens_per_host: usize,
    pub warm_up: Duration,
    pub window: Duration,
}

impl CurveOptions {
    pub fn full() -> Self {
        Self {
            levels: LEVELS.to_vec(),
            rounds: 5,
            opens_per_host: 20,
            warm_up: Duration::from_secs(1),
            window: Duration::from_secs(3),
        }
    }
}

/// One level of the curve: the median round's throughput, and the
/// percentiles of every round's opens together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub connections: usize,
    /// Aggregate bytes carried between hosts, MiB per second.
    pub throughput_mib_s: f64,
    /// From a stream's open to the partner's first answer, milliseconds.
    pub open_p50_ms: f64,
    pub open_p99_ms: f64,
    /// Opens timed over all rounds.
    pub opens: usize,
}

/// A whole run as a baseline records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub schema: u32,
    pub model: String,
    pub os: String,
    pub profile: String,
    pub carrier: String,
    pub reference_state: String,
    pub cpus: usize,
    /// The 1, 5 and 15 minute load averages before and after the run.
    pub load_before: [f64; 3],
    pub load_after: [f64; 3],
    pub rounds: usize,
    pub points: Vec<Point>,
}

impl Curve {
    pub fn baseline_path(machine: &Machine) -> PathBuf {
        Path::new(BASELINE_ROOT)
            .join(&machine.model)
            .join("baseline.json")
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the relay baseline {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parse the relay baseline {}", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("write the relay baseline {}", path.display()))
    }

    pub fn print(&self) {
        println!("relay scaling curve");
        println!(
            "machine: {} · macOS {} · {} CPUs · profile {} · {}",
            self.model, self.os, self.cpus, self.profile, self.reference_state
        );
        println!(
            "carrier: {} · {} rounds per level: median throughput, pooled opens",
            self.carrier, self.rounds
        );
        println!(
            "load average before: {} · after: {}",
            load(self.load_before),
            load(self.load_after)
        );
        println!("connections | throughput | open p50 | open p99 | opens");
        for point in &self.points {
            println!(
                "{} | {:.1} MiB/s | {:.3} ms | {:.3} ms | {}",
                point.connections,
                point.throughput_mib_s,
                point.open_p50_ms,
                point.open_p99_ms,
                point.opens
            );
        }
    }
}

fn load(values: [f64; 3]) -> String {
    format!("{:.2} {:.2} {:.2}", values[0], values[1], values[2])
}

/// One figure of a run against its baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    pub connections: usize,
    pub metric: &'static str,
    pub baseline: f64,
    pub measured: f64,
    /// Signed change, positive where the run is worse.
    pub worse_by: f64,
    pub band: f64,
    pub passed: bool,
}

/// Judges `run` against `baseline`: refused when they were not measured
/// the same way, failing where a figure is worse than its band allows.
pub fn compare(run: &Curve, baseline: &Curve) -> Result<Vec<Judgement>> {
    for (what, ours, theirs) in [
        ("model", &run.model, &baseline.model),
        ("profile", &run.profile, &baseline.profile),
        ("carrier", &run.carrier, &baseline.carrier),
        (
            "reference state",
            &run.reference_state,
            &baseline.reference_state,
        ),
    ] {
        if ours != theirs {
            bail!("the baseline was recorded with {what} {theirs:?}, this run is {ours:?}");
        }
    }
    if run.schema != baseline.schema {
        bail!(
            "the baseline has schema {}, this run {}",
            baseline.schema,
            run.schema
        );
    }
    let recorded = baseline
        .points
        .iter()
        .map(|point| (point.connections, point))
        .collect::<BTreeMap<_, _>>();
    let mut judgements = Vec::new();
    for point in &run.points {
        let Some(base) = recorded.get(&point.connections) else {
            bail!(
                "the baseline has no figure at {} connections",
                point.connections
            );
        };
        for (metric, measured, baseline, band, higher_is_better) in [
            (
                "throughput",
                point.throughput_mib_s,
                base.throughput_mib_s,
                THROUGHPUT_BAND,
                true,
            ),
            (
                "open p50",
                point.open_p50_ms,
                base.open_p50_ms,
                OPEN_P50_BAND,
                false,
            ),
            (
                "open p99",
                point.open_p99_ms,
                base.open_p99_ms,
                OPEN_P99_BAND,
                false,
            ),
        ] {
            let change = (measured - baseline) / baseline;
            let worse_by = if higher_is_better { -change } else { change };
            judgements.push(Judgement {
                connections: point.connections,
                metric,
                baseline,
                measured,
                worse_by,
                band,
                passed: worse_by <= band,
            });
        }
    }
    Ok(judgements)
}

pub fn print_judgements(judgements: &[Judgement]) {
    println!("connections | metric | baseline | measured | worse by | band | verdict");
    for judgement in judgements {
        let unit = if judgement.metric == "throughput" {
            "MiB/s"
        } else {
            "ms"
        };
        println!(
            "{} | {} | {:.3} {unit} | {:.3} {unit} | {:+.1}% | {:.0}% | {}",
            judgement.connections,
            judgement.metric,
            judgement.baseline,
            judgement.measured,
            judgement.worse_by * 100.0,
            judgement.band * 100.0,
            if judgement.passed { "PASS" } else { "FAIL" }
        );
    }
}

/// Measures the curve on this machine.
pub async fn run(
    machine: &Machine,
    options: &CurveOptions,
    relay_runtime: &tokio::runtime::Handle,
) -> Result<Curve> {
    let load_before = load_average()?;
    let mut points = Vec::new();
    for &connections in &options.levels {
        let mut throughputs = Vec::new();
        let mut latencies = Vec::new();
        for round in 0..options.rounds {
            let measured = measure(connections, options, relay_runtime)
                .await
                .with_context(|| format!("{connections} connections, round {}", round + 1))?;
            eprintln!(
                "{connections} connections, round {}: {:.1} MiB/s, open p50 {:.3} ms, p99 {:.3} ms",
                round + 1,
                measured.throughput_mib_s,
                percentile(&measured.latencies, 0.50),
                percentile(&measured.latencies, 0.99),
            );
            throughputs.push(measured.throughput_mib_s);
            latencies.extend(measured.latencies);
        }
        latencies.sort();
        points.push(Point {
            connections,
            throughput_mib_s: median(throughputs.into_iter()),
            open_p50_ms: percentile(&latencies, 0.50),
            open_p99_ms: percentile(&latencies, 0.99),
            opens: latencies.len(),
        });
    }
    Ok(Curve {
        schema: SCHEMA,
        model: machine.model.clone(),
        os: machine.os.clone(),
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .to_owned(),
        carrier: CARRIER.to_owned(),
        reference_state: DESKTOP_REFERENCE_STATE.to_owned(),
        cpus: std::thread::available_parallelism().map_or(0, usize::from),
        load_before,
        load_after: load_average()?,
        rounds: options.rounds,
        points,
    })
}

/// One round at one level, on a fresh relay.
async fn measure(
    connections: usize,
    options: &CurveOptions,
    relay_runtime: &tokio::runtime::Handle,
) -> Result<Round> {
    if !connections.is_multiple_of(2) {
        bail!("hosts come in pairs; {connections} connections is odd");
    }
    let relay = relay_runtime.spawn(Relay::start()).await??;
    let accounts = (0..connections / 2)
        .map(|_| Uuid::new_v4())
        .collect::<Vec<_>>();
    let hosts = tokio::time::timeout(PHASE_TIMEOUT, relay.link_hosts(&accounts))
        .await
        .map_err(|_| anyhow!("hosts did not all link to the relay"))??;
    tokio::time::timeout(PHASE_TIMEOUT, relay.wait_for_pairs(&accounts))
        .await
        .map_err(|_| anyhow!("the relay never held both hosts of every account"))?;

    let received = Arc::new(AtomicU64::new(0));
    let mut serving = JoinSet::new();
    for host in &hosts {
        let host = host.clone();
        let received = received.clone();
        serving.spawn(async move {
            let mut streams = JoinSet::new();
            while let Some(stream) = host.accept().await {
                streams.spawn(answer(stream, received.clone()));
            }
        });
    }

    let mut timing = JoinSet::new();
    for (index, host) in hosts.iter().enumerate() {
        let host = host.clone();
        let partner = hosts[index ^ 1].id();
        let opens = options.opens_per_host;
        timing.spawn(async move {
            let mut latencies = Vec::with_capacity(opens);
            for _ in 0..opens {
                let started = Instant::now();
                let mut stream = host.open(partner).await?;
                stream.write_all(&[PING]).await?;
                stream.flush().await?;
                let mut answer = [0_u8; 1];
                stream.read_exact(&mut answer).await?;
                latencies.push(started.elapsed());
                stream.shutdown().await?;
            }
            Ok::<_, std::io::Error>(latencies)
        });
    }
    let mut latencies = Vec::new();
    tokio::time::timeout(PHASE_TIMEOUT, async {
        while let Some(timed) = timing.join_next().await {
            latencies.extend(timed?.context("time a stream open")?);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("stream opens did not finish"))??;
    latencies.sort();

    let mut writing = JoinSet::new();
    for (index, host) in hosts.iter().enumerate() {
        let host = host.clone();
        let partner = hosts[index ^ 1].id();
        writing.spawn(async move {
            let mut stream = host.open(partner).await?;
            stream.write_all(&[BULK]).await?;
            let chunk = vec![0x5a_u8; CHUNK];
            loop {
                stream.write_all(&chunk).await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), std::io::Error>(())
        });
    }
    tokio::time::sleep(options.warm_up).await;
    let (from, started) = (received.load(Ordering::Relaxed), Instant::now());
    tokio::time::sleep(options.window).await;
    let (to, elapsed) = (received.load(Ordering::Relaxed), started.elapsed());
    // A writer that ended inside the window failed, and its absence would
    // read as a slower relay.
    if let Some(ended) = writing.try_join_next() {
        bail!("a bulk writer ended early: {ended:?}");
    }
    writing.abort_all();
    serving.abort_all();
    drop(hosts);

    Ok(Round {
        throughput_mib_s: (to - from) as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
        latencies,
    })
}

/// What one round measured: its aggregate throughput and every open's
/// latency, sorted.
struct Round {
    throughput_mib_s: f64,
    latencies: Vec<Duration>,
}

/// A partner's side of a stream: one byte back for a timed open, a count
/// of everything read for a bulk one.
async fn answer(mut stream: ByteStream, received: Arc<AtomicU64>) {
    let mut tag = [0_u8; 1];
    if stream.read_exact(&mut tag).await.is_err() {
        return;
    }
    if tag[0] == PING {
        if stream.write_all(&tag).await.is_ok() && stream.flush().await.is_ok() {
            let _ = stream.read(&mut tag).await;
        }
        return;
    }
    let mut buffer = vec![0_u8; CHUNK];
    while let Ok(read @ 1..) = stream.read(&mut buffer).await {
        received.fetch_add(read as u64, Ordering::Relaxed);
    }
}

/// The production relay on loopback with a stand-in for the cloud's
/// credentials: a token is the account it names.
struct Relay {
    server: CloudLinkServer,
    addr: std::net::SocketAddr,
    connector: tokio_rustls::TlsConnector,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    async fn start() -> Result<Self> {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![RELAY_NAME.to_owned()])
                .context("make the relay's certificate")?;
        let acceptor = node::create_tls_acceptor(
            cert.pem().as_bytes(),
            signing_key.serialize_pem().as_bytes(),
        )
        .context("load the relay's certificate")?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone())?;
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server = CloudLinkServer::new(
            RelayIdentity {
                host_id: Uuid::new_v4(),
                name: RELAY_NAME.to_owned(),
                authenticator: Arc::new(AccountTokens),
                clock: Arc::new(SystemClock),
            },
            // The curve reads no dump the relay's core asks for.
            None,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let task = server.serve_on_tls_tcp_listener(listener, acceptor, HANDSHAKE_TIMEOUT);
        Ok(Self {
            server,
            addr,
            connector: tokio_rustls::TlsConnector::from(Arc::new(client)),
            task,
        })
    }

    /// Links two hosts per account, `DIAL_CONCURRENCY` dialling at once,
    /// in account order: hosts 2k and 2k+1 are account k's.
    async fn link_hosts(&self, accounts: &[Uuid]) -> Result<Vec<Arc<RelayHost>>> {
        let slots = Arc::new(Semaphore::new(DIAL_CONCURRENCY));
        let mut dialling = JoinSet::new();
        for (index, account) in accounts
            .iter()
            .flat_map(|account| [account, account])
            .enumerate()
        {
            let slots = slots.clone();
            let connector = self.connector.clone();
            let addr = self.addr;
            let token = account.to_string();
            dialling.spawn(async move {
                let _slot = slots.acquire_owned().await?;
                let tcp = TcpStream::connect(addr).await?;
                tcp.set_nodelay(true)?;
                let tls = connector
                    .connect(RELAY_NAME.try_into()?, tcp)
                    .await
                    .context("TLS handshake with the relay")?;
                let host =
                    RelayHost::connect(tls, token, SystemTime::now() + Duration::from_secs(3600))
                        .await
                        .context("link to the relay")?;
                Ok::<_, anyhow::Error>((index, Arc::new(host)))
            });
        }
        let mut hosts = Vec::with_capacity(accounts.len() * 2);
        while let Some(linked) = dialling.join_next().await {
            hosts.push(linked??);
        }
        hosts.sort_by_key(|(index, _)| *index);
        Ok(hosts.into_iter().map(|(_, host)| host).collect())
    }

    async fn wait_for_pairs(&self, accounts: &[Uuid]) {
        for account in accounts {
            while self.server.user_links(*account).await.len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct AccountTokens;

#[tonic::async_trait]
impl LinkTokenAuthenticator for AccountTokens {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        Ok(AuthenticatedLinkUser {
            user_id: token
                .parse()
                .map_err(|_| tonic::Status::unauthenticated("not an account"))?,
            client_id: "relay-curve".to_owned(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
            tier: Tier::Pro,
        })
    }
}

/// Nearest-rank percentile of sorted durations, in milliseconds.
fn percentile(sorted: &[Duration], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((quantile * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1].as_secs_f64() * 1_000.0
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values = values.collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

/// The 1, 5 and 15 minute load averages.
#[cfg(unix)]
pub fn load_average() -> Result<[f64; 3]> {
    let mut values = [0_f64; 3];
    // SAFETY: `values` holds the three doubles asked for.
    if unsafe { libc::getloadavg(values.as_mut_ptr(), 3) } != 3 {
        bail!("the load average is unavailable");
    }
    Ok(values)
}

#[cfg(not(unix))]
pub fn load_average() -> Result<[f64; 3]> {
    bail!("the load average is unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(throughput: f64, p50: f64, p99: f64) -> Curve {
        Curve {
            schema: SCHEMA,
            model: "Mac14,6".to_owned(),
            os: "26.0".to_owned(),
            profile: "release".to_owned(),
            carrier: CARRIER.to_owned(),
            reference_state: DESKTOP_REFERENCE_STATE.to_owned(),
            cpus: 12,
            load_before: [1.0; 3],
            load_after: [1.0; 3],
            rounds: 5,
            points: vec![Point {
                connections: 10,
                throughput_mib_s: throughput,
                open_p50_ms: p50,
                open_p99_ms: p99,
                opens: 1_000,
            }],
        }
    }

    #[test]
    fn a_run_inside_every_band_passes_and_a_faster_one_too() {
        let baseline = curve(1_000.0, 1.0, 2.0);
        let near = compare(&curve(900.0, 1.2, 3.9), &baseline).unwrap();
        assert!(near.iter().all(|judgement| judgement.passed), "{near:?}");
        let faster = compare(&curve(2_000.0, 0.5, 1.0), &baseline).unwrap();
        assert!(
            faster.iter().all(|judgement| judgement.passed),
            "{faster:?}"
        );
    }

    #[test]
    fn each_figure_past_its_band_fails_alone() {
        let baseline = curve(1_000.0, 1.0, 2.0);
        let failed = |run: Curve| {
            compare(&run, &baseline)
                .unwrap()
                .into_iter()
                .filter(|judgement| !judgement.passed)
                .map(|judgement| judgement.metric)
                .collect::<Vec<_>>()
        };
        assert_eq!(failed(curve(800.0, 1.0, 2.0)), ["throughput"]);
        assert_eq!(failed(curve(1_000.0, 1.3, 2.0)), ["open p50"]);
        assert_eq!(failed(curve(1_000.0, 1.0, 4.1)), ["open p99"]);
    }

    #[test]
    fn a_baseline_measured_another_way_is_refused() {
        let baseline = curve(1_000.0, 1.0, 2.0);
        let mut run = curve(1_000.0, 1.0, 2.0);
        run.carrier = "QUIC".to_owned();
        assert!(compare(&run, &baseline).is_err());
        let mut run = curve(1_000.0, 1.0, 2.0);
        run.points[0].connections = 20;
        assert!(compare(&run, &baseline).is_err());
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let sorted = (1..=100).map(Duration::from_millis).collect::<Vec<_>>();
        assert_eq!(percentile(&sorted, 0.50), 50.0);
        assert_eq!(percentile(&sorted, 0.99), 99.0);
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
    }
}
