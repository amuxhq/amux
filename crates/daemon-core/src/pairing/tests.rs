use store::Sqlite;

use super::*;
use crate::payload::{ClientRequest, JobResult};
use crate::trust::TrustStore;
use crate::{Input, Job, StartOptions, StepBudget};

const OFFERER: u8 = 1;
const JOINER: u8 = 2;
const SESSION: PairingId = PairingId(7);
const MINUTE: u64 = 60_000;

fn identity(host: u8) -> wire::PairingIdentity {
    wire::PairingIdentity {
        host_id: vec![host; 16],
        pubkey: vec![host; 32],
        name: format!("host {host}"),
        expires_at_ms: 0,
    }
}

/// One host's core, with what it has said since last asked.
struct Host {
    daemon: Daemon,
    out: Vec<Output>,
}

impl Host {
    fn new(host: u8) -> Host {
        let store = Sqlite::open_in_memory(vec![host; 16]).expect("a store opens");
        let daemon = Daemon::new(
            store,
            StartOptions {
                trust: "/profile/trust.json".into(),
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        );
        let mut host = Host {
            daemon,
            out: Vec::new(),
        };
        host.step(Input::Tick { now_ms: 1_000_000 });
        host.step(Input::Entropy([host_seed(host.id()); 32]));
        host.step(Input::TrustRead {
            store: TrustStore::default(),
        });
        host.out.clear();
        host
    }

    fn id(&self) -> u8 {
        self.daemon.store.own_host()[0]
    }

    /// Steps `input`, then finishes every trust job it asked for.
    fn step(&mut self, input: Input) {
        let mut out = Vec::new();
        self.daemon.step(input, &mut out);
        self.out.extend(out.iter().cloned());
        for output in out {
            if let Output::Job {
                id,
                op: Job::Fsync { .. } | Job::TeardownHost { .. },
            } = output
            {
                self.step(Input::JobDone {
                    job: id,
                    result: JobResult::Done,
                });
            }
        }
    }

    fn call(&mut self, call: u64, request: ClientRequest) {
        self.step(Input::ClientCall {
            call: CallId(call),
            request,
        });
    }

    fn take(&mut self) -> Vec<Output> {
        std::mem::take(&mut self.out)
    }

    fn open_window(&mut self, method: WindowMethod, ttl_ms: u64) -> Vec<u8> {
        self.call(1, ClientRequest::StartPairing { method, ttl_ms });
        let out = self.take();
        match out.iter().find_map(|output| match output {
            Output::Answer {
                response: Response::PairingWindow(secret),
                ..
            } => Some(secret.clone()),
            _ => None,
        }) {
            Some(WindowSecret::Pin(pin)) => pin.into_bytes(),
            Some(WindowSecret::Qr(secret)) => secret,
            None => panic!("expected a window, got {out:?}"),
        }
    }

    fn respond(&mut self) {
        self.step(Input::PairingOpened {
            session: SESSION,
            opened: Opened::Responder {
                local: identity(self.id()),
                reachability: None,
            },
        });
    }

    fn initiate(&mut self, secret: &[u8]) {
        self.step(Input::PairingOpened {
            session: SESSION,
            opened: Opened::Initiator(Box::new(Initiate {
                local: identity(self.id()),
                secret: secret.to_vec(),
                expect: None,
                found: false,
                reachability: Reachability::Cloud,
                method: Method::Pin,
            })),
        });
    }

    fn tick(&mut self, now_ms: u64) {
        self.step(Input::Tick { now_ms });
    }

    fn trusts(&self, host: u8) -> bool {
        self.daemon
            .trust()
            .entry(Uuid::from_bytes([host; 16]))
            .is_some()
    }
}

fn host_seed(host: u8) -> u8 {
    host.wrapping_mul(31)
}

/// Carries each side's messages to the other until neither says more.
fn converse(offerer: &mut Host, joiner: &mut Host) -> (Vec<Output>, Vec<Output>) {
    let (mut said_by_offerer, mut said_by_joiner) = (Vec::new(), Vec::new());
    loop {
        let from_offerer = offerer.take();
        let from_joiner = joiner.take();
        if from_offerer.is_empty() && from_joiner.is_empty() {
            return (said_by_offerer, said_by_joiner);
        }
        for output in &from_offerer {
            if let Output::Pair { message, .. } = output {
                joiner.step(Input::PairingMessage {
                    session: SESSION,
                    message: message.clone(),
                });
            }
        }
        for output in &from_joiner {
            if let Output::Pair { message, .. } = output {
                offerer.step(Input::PairingMessage {
                    session: SESSION,
                    message: message.clone(),
                });
            }
        }
        said_by_offerer.extend(from_offerer);
        said_by_joiner.extend(from_joiner);
    }
}

fn pending_token(out: &[Output]) -> Option<Vec<u8>> {
    out.iter().find_map(|output| match output {
        Output::PairPending { token, .. } => Some(token.clone()),
        _ => None,
    })
}

fn failure(out: &[Output]) -> Option<PairFailure> {
    out.iter().find_map(|output| match output {
        Output::PairEnded {
            failure: Some(failure),
            ..
        } => Some(failure.clone()),
        _ => None,
    })
}

fn call_ended(out: &[Output], call: u64) -> Option<Status> {
    out.iter().find_map(|output| match output {
        Output::EndCall {
            call: ended,
            status,
        } if *ended == CallId(call) => Some(status.clone()),
        _ => None,
    })
}

fn events(out: &[Output]) -> Vec<PairingEvent> {
    out.iter()
        .filter_map(|output| match output {
            Output::Shell(ShellWork::Pairing(event)) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_demo_pin_survives_successes_and_failures() {
    let mut offerer = Host::new(OFFERER);
    offerer.open_window(WindowMethod::Demo("123456".into()), 5 * MINUTE);
    for _ in 0..ATTEMPT_LIMIT + 2 {
        let mut joiner = Host::new(JOINER);
        offerer.respond();
        joiner.initiate(b"000000");
        converse(&mut offerer, &mut joiner);
    }
    assert!(
        offerer.daemon.pairing().active(),
        "a demo PIN never locks out"
    );
    for _ in 0..2 {
        let mut joiner = Host::new(JOINER);
        offerer.respond();
        joiner.initiate(b"123456");
        let (_, said) = converse(&mut offerer, &mut joiner);
        joiner.call(
            9,
            ClientRequest::ConfirmPair {
                token: pending_token(&said).unwrap(),
            },
        );
        converse(&mut offerer, &mut joiner);
        assert!(joiner.trusts(OFFERER));
    }
    assert!(
        offerer.daemon.pairing().active(),
        "a demo PIN is never used up"
    );
    offerer.call(2, ClientRequest::CancelPairing);
    assert!(!offerer.daemon.pairing().active());
}

#[test]
fn a_demo_pin_must_be_six_digits() {
    let mut offerer = Host::new(OFFERER);
    offerer.call(
        1,
        ClientRequest::StartPairing {
            method: WindowMethod::Demo("12345".into()),
            ttl_ms: MINUTE,
        },
    );
    assert_eq!(
        call_ended(&offerer.take(), 1),
        Some(Status::Failed(CallError::Window(
            WindowRefusal::InvalidPinFormat
        )))
    );
}

#[test]
fn a_pin_is_six_decimal_digits() {
    let mut offerer = Host::new(OFFERER);
    let pin = offerer.open_window(WindowMethod::Pin, MINUTE);
    assert_eq!(pin.len(), 6);
    assert!(pin.iter().all(u8::is_ascii_digit));
}

/// A seed read from a checkpoint predicts every draw after it until fresh
/// entropy is mixed in: the window opened after a second seed is not the
/// one the first seed alone predicts.
#[test]
fn a_draw_after_fresh_entropy_is_not_the_one_the_old_seed_predicts() {
    let predicted = Host::new(OFFERER).open_window(WindowMethod::Pin, MINUTE);
    assert_eq!(
        Host::new(OFFERER).open_window(WindowMethod::Pin, MINUTE),
        predicted,
        "one seed and no fresh entropy draw the same PIN"
    );
    let mut fresh = Host::new(OFFERER);
    fresh.step(Input::Entropy([0xA5; 32]));
    assert_ne!(fresh.open_window(WindowMethod::Pin, MINUTE), predicted);
}

#[test]
fn too_many_wrong_secrets_lock_the_window_out_until_a_new_one_opens() {
    let mut offerer = Host::new(OFFERER);
    offerer.open_window(WindowMethod::Qr, 5 * MINUTE);
    for _ in 0..ATTEMPT_LIMIT {
        let mut joiner = Host::new(JOINER);
        offerer.respond();
        joiner.initiate(b"not it");
        converse(&mut offerer, &mut joiner);
    }
    assert!(!offerer.daemon.pairing().active());
    assert!(offerer.daemon.pairing().locked_out());
    offerer.respond();
    assert_eq!(
        failure(&offerer.take()),
        Some(PairFailure::new(
            FailureCode::PermissionDenied,
            "PAIRING_LOCKED"
        ))
    );
    offerer.open_window(WindowMethod::Pin, MINUTE);
    assert!(!offerer.daemon.pairing().locked_out());
}

#[test]
fn attempts_in_flight_count_against_the_cap_until_they_end() {
    let mut offerer = Host::new(OFFERER);
    offerer.open_window(WindowMethod::Pin, 5 * MINUTE);
    for session in 0..u64::from(ATTEMPT_LIMIT) {
        offerer.step(Input::PairingOpened {
            session: PairingId(100 + session),
            opened: Opened::Responder {
                local: identity(OFFERER),
                reachability: None,
            },
        });
    }
    offerer.take();
    offerer.respond();
    assert_eq!(failure(&offerer.take()), Some(PairFailure::invalid_pin()));
    offerer.step(Input::PairingClosed {
        session: PairingId(100),
        status: None,
    });
    offerer.take();
    offerer.respond();
    assert_eq!(
        failure(&offerer.take()),
        None,
        "a closed attempt frees its slot"
    );
}

#[test]
fn a_conversation_past_its_deadline_ends_and_frees_its_attempt() {
    let mut offerer = Host::new(OFFERER);
    offerer.open_window(WindowMethod::Pin, 10 * MINUTE);
    offerer.respond();
    let armed = offerer.take();
    assert!(armed.contains(&Output::ArmTimer {
        id: TimerId::Pairing(SESSION),
        at_ms: 1_000_000 + RESPONDER_PATIENCE_MS,
    }));
    offerer.tick(1_000_000 + RESPONDER_PATIENCE_MS);
    offerer.step(Input::Timer {
        id: TimerId::Pairing(SESSION),
    });
    let out = offerer.take();
    assert_eq!(
        failure(&out),
        Some(PairFailure::new(
            FailureCode::DeadlineExceeded,
            "PAIRING_TIMEOUT"
        ))
    );
    assert_eq!(
        events(&out)[0].outcome,
        Outcome::Failed {
            reason: "PAIRING_TIMEOUT".into(),
            counted: Some(Failure::TimedOut),
        }
    );
    let window = offerer.daemon.pairing.window.as_ref().unwrap();
    assert!(window.in_flight.is_empty() && window.failed == 0);
}

/// The window closing while the joiner's key is written undoes the
/// pairing it was for.
#[test]
fn a_commit_that_lands_after_the_window_closed_is_undone() {
    let (mut offerer, mut joiner) = (Host::new(OFFERER), Host::new(JOINER));
    let secret = offerer.open_window(WindowMethod::Pin, 5 * MINUTE);
    offerer.respond();
    joiner.initiate(&secret);
    let (_, said) = converse(&mut offerer, &mut joiner);
    joiner.call(
        9,
        ClientRequest::ConfirmPair {
            token: pending_token(&said).unwrap(),
        },
    );
    // Hold the offerer's trust write: carry the joiner's sealed identity
    // by hand and keep the job.
    let sealed = joiner
        .take()
        .into_iter()
        .find_map(|output| match output {
            Output::Pair { message, .. } => Some(message),
            _ => None,
        })
        .unwrap();
    let mut out = Vec::new();
    offerer.daemon.step(
        Input::PairingMessage {
            session: SESSION,
            message: sealed,
        },
        &mut out,
    );
    let write = out
        .iter()
        .find_map(|output| match output {
            Output::Job { id, .. } => Some(*id),
            _ => None,
        })
        .expect("the joiner's key is being written");
    offerer.call(2, ClientRequest::CancelPairing);
    offerer.take();
    offerer.step(Input::JobDone {
        job: write,
        result: JobResult::Done,
    });
    let out = offerer.take();
    assert_eq!(failure(&out), Some(PairFailure::invalid_pin()));
    assert!(!offerer.trusts(JOINER), "the commit is undone");
}

#[test]
fn an_unknown_token_is_refused_like_a_wrong_secret() {
    let mut joiner = Host::new(JOINER);
    joiner.call(9, ClientRequest::ConfirmPair { token: vec![1; 16] });
    assert_eq!(
        call_ended(&joiner.take(), 9),
        Some(Status::Failed(CallError::Pairing(
            PairFailure::invalid_pin()
        )))
    );
}

#[test]
fn a_pending_pairing_is_forgotten_when_the_peers_window_would_close() {
    let (mut offerer, mut joiner) = (Host::new(OFFERER), Host::new(JOINER));
    let secret = offerer.open_window(WindowMethod::Pin, 2 * MINUTE);
    offerer.respond();
    joiner.initiate(&secret);
    let (_, said) = converse(&mut offerer, &mut joiner);
    let token = pending_token(&said).unwrap();
    assert!(said.contains(&Output::ArmTimer {
        id: TimerId::Pairing(SESSION),
        at_ms: 1_000_000 + 2 * MINUTE,
    }));
    joiner.tick(1_000_000 + 2 * MINUTE);
    joiner.step(Input::Timer {
        id: TimerId::Pairing(SESSION),
    });
    joiner.take();
    joiner.call(9, ClientRequest::ConfirmPair { token });
    assert_eq!(
        call_ended(&joiner.take(), 9),
        Some(Status::Failed(CallError::Pairing(
            PairFailure::invalid_pin()
        )))
    );
}

/// SSH carried the identities, so the commit is the conversation.
#[test]
fn an_ssh_pairing_commits_through_the_same_trust_writes() {
    let mut joiner = Host::new(JOINER);
    let reachability = Reachability::Ssh("desk".into());
    joiner.call(
        3,
        ClientRequest::PairOverSsh {
            local: identity(JOINER),
            peer: identity(OFFERER),
            reachability: Some(reachability.clone()),
        },
    );
    let out = joiner.take();
    assert!(out.iter().any(|output| matches!(
        output,
        Output::Job {
            op: Job::Fsync { .. },
            ..
        }
    )));
    assert_eq!(
        joiner
            .daemon
            .trust()
            .entry(Uuid::from_bytes([OFFERER; 16]))
            .unwrap()
            .ssh,
        Some("desk".into())
    );
    assert_eq!(
        events(&out),
        vec![PairingEvent {
            role: Role::Joiner,
            method: Method::Ssh,
            outcome: Outcome::Succeeded {
                host: HostId([OFFERER; 16]),
            },
        }]
    );

    joiner.call(
        4,
        ClientRequest::PairOverSsh {
            local: identity(JOINER),
            peer: identity(JOINER),
            reachability: None,
        },
    );
    assert_eq!(
        call_ended(&joiner.take(), 4),
        Some(Status::Failed(CallError::Pairing(PairFailure::new(
            FailureCode::InvalidArgument,
            "SELF_PAIRING"
        ))))
    );
}
