//! Onboarding and the settings screen as the storybook draws them: each
//! screen built from authored data through the screens' own calls, with
//! the keys a person would press, and no daemon behind it.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ui_state::{Connection, FleetMsg, FleetState};
use ui_view::SetupStep;
use wire::inventory_event::Of;
use wire::{OnboardingStepId as Id, StepState};

use crate::Theme;
use crate::onboarding::Onboarding;
use crate::settings_screen::SettingsScreen;

/// When every scene is drawn, in Unix milliseconds.
const NOW: i64 = 1_800_000_000_000;

/// The phone of the scenes, as it names itself.
pub const PHONE: &str = "Jordan's iPhone";

/// One onboarding screen over the authored desk.
pub struct SetupScene {
    pub onboarding: Onboarding,
    pub fleet: FleetState,
}

impl SetupScene {
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        self.onboarding.draw(frame, area, None, theme);
    }

    /// A key pressed on the screen, as the person would press it.
    pub fn key(&mut self, key: impl Into<KeyEvent>) {
        let _ = self.onboarding.key(key.into(), &self.fleet);
    }
}

/// The settings screen.
pub struct SettingsScene {
    pub screen: SettingsScreen,
}

impl SettingsScene {
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        self.screen.draw(frame, area, None, theme);
    }

    pub fn key(&mut self, key: impl Into<KeyEvent>) {
        let _ = self.screen.key(key.into());
    }
}

/// The desk's settings on a first run: a supervisor, so starting at login
/// is a choice; usage counts on; no repositories yet.
pub fn settings() -> wire::Settings {
    wire::Settings {
        host_name: "desk".to_owned(),
        start_at_login: Some(false),
        keep_awake: false,
        repositories: Vec::new(),
        chat_in: wire::ChatIn::Amux as i32,
        claude_kind: wire::Kind::Unspecified as i32,
        telemetry: true,
        auto_reports: false,
    }
}

/// The steps with `states` in their order.
pub fn steps(states: [StepState; 6]) -> wire::Onboarding {
    let ids = [
        Id::PairPhone,
        Id::KeepReachable,
        Id::SignIn,
        Id::Repositories,
        Id::Improve,
        Id::FirstAgent,
    ];
    wire::Onboarding {
        profile_id: b"profile".to_vec(),
        steps: ids
            .iter()
            .zip(states)
            .map(|(id, state)| wire::OnboardingStep {
                id: *id as i32,
                state: state as i32,
            })
            .collect(),
        dismissed: false,
    }
}

/// The desk alone, and the phone when it is paired: signed out, so the
/// account can be offered to it.
pub fn fleet(with_phone: bool) -> FleetState {
    let mut fleet = FleetState::new();
    let mut event = |of| {
        fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(of),
        })))
    };
    event(Of::Host(wire::HostEntry::trusted_host(
        b"desk".as_slice(),
        "desk",
        wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
    )));
    if with_phone {
        let mut phone = wire::HostEntry::trusted_host(
            b"phone".as_slice(),
            PHONE,
            wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
        );
        if let Some(host) = phone.host.as_mut() {
            host.platform = wire::Platform::Ios as i32;
            host.account_signed_in = Some(false);
        }
        event(Of::Host(phone));
    }
    event(Of::CaughtUp(wire::CaughtUp { revision: 0 }));
    fleet.update(FleetMsg::Connection(Connection::Live));
    fleet
}

/// A pairing window as the desk opens one: the PIN and the link its QR
/// code shows, an invitation of the usual length. The invitation names a
/// home address and a Tailscale one, the most a desk's names, so the link
/// is as long as a real one gets: 97 characters.
pub fn window() -> wire::PairingWindow {
    let invitation =
        "AYw-GitNX0prjH2eDxorPE0M_wdYjAPbQhHIYy2ABulHD74qogjWN2v0H7ICXpkbeRz8wKgBGGRYDgM";
    wire::PairingWindow {
        identity: Some(wire::PairingIdentity {
            name: "desk".to_owned(),
            ..Default::default()
        }),
        ttl_seconds: crate::onboarding::WINDOW_SECONDS,
        addrs: vec![
            "192.168.1.24:7420".to_owned(),
            "100.88.14.3:7420".to_owned(),
        ],
        pin: Some("418207".to_owned()),
        qr_link: format!("https://amux.sh/p#{invitation}"),
    }
}

fn opened(step: SetupStep, steps: wire::Onboarding, fleet: FleetState) -> SetupScene {
    let (onboarding, _) = Onboarding::open(step, false, steps, settings(), "Mac", b"desk", &fleet);
    SetupScene { onboarding, fleet }
}

/// The first screen of a first run: the pairing window open, its QR code
/// and PIN.
pub fn pair() -> SetupScene {
    let mut scene = opened(
        SetupStep::PairPhone,
        steps([StepState::Needed; 6]),
        fleet(false),
    );
    scene.onboarding.pairing_opened(window(), NOW);
    scene
}

/// The machine's name being changed on the pairing screen.
pub fn pair_rename() -> SetupScene {
    let mut scene = pair();
    scene.key(KeyCode::Char('r'));
    for _ in 0..4 {
        scene.key(KeyCode::Backspace);
    }
    for c in "studio".chars() {
        scene.key(KeyCode::Char(c));
    }
    scene
}

/// The phone proved the PIN and is confirming.
pub fn pairing() -> SetupScene {
    let mut scene = pair();
    let fleet = scene.fleet.clone();
    let _ = scene.onboarding.pairing_status(
        wire::PairingStatus {
            active: true,
            locked_out: false,
            joining: true,
            paired: None,
        },
        &fleet,
    );
    scene
}

fn paired_peer() -> wire::Peer {
    wire::Peer {
        host_id: b"phone".to_vec(),
        name: PHONE.to_owned(),
        ..Default::default()
    }
}

/// Paired: "Keep it connected", both choices on.
pub fn paired() -> SetupScene {
    let mut scene = pairing();
    scene.fleet = fleet(true);
    let fleet = scene.fleet.clone();
    let _ = scene.onboarding.pairing_status(
        wire::PairingStatus {
            active: false,
            locked_out: false,
            joining: false,
            paired: Some(paired_peer()),
        },
        &fleet,
    );
    scene
}

/// Signing in, with the phone just paired ticked to be signed in too.
pub fn sign_in() -> SetupScene {
    let mut scene = paired();
    scene.key(KeyCode::Enter);
    let fleet = scene.fleet.clone();
    let _ = scene.onboarding.confirmed(SetupStep::KeepReachable, &fleet);
    scene
}

/// The browser opened at the approval page; it did not here, so the link
/// shows, with the code to check.
pub fn sign_in_waiting() -> SetupScene {
    let mut scene = sign_in();
    scene.key(KeyCode::Enter);
    scene.onboarding.sign_in_code(
        wire::SignInCode {
            verification_uri: "https://amux.sh/device".to_owned(),
            verification_uri_complete: "https://amux.sh/device?user_code=WDJB-MJHT".to_owned(),
            user_code: "WDJB-MJHT".to_owned(),
            expires_at_ms: NOW + 600_000,
        },
        false,
    );
    scene
}

/// The phone signed in first and offers its account.
pub fn sign_in_offer() -> SetupScene {
    let mut scene = opened(
        SetupStep::SignIn,
        steps([
            StepState::Done,
            StepState::Done,
            StepState::Needed,
            StepState::Needed,
            StepState::Needed,
            StepState::Needed,
        ]),
        fleet(true),
    );
    let _ = scene.onboarding.offers_changed(vec![wire::SignInOffer {
        host_id: b"phone".to_vec(),
        host_name: PHONE.to_owned(),
        email: "jordan@example.com".to_owned(),
        account_name: "Jordan".to_owned(),
    }]);
    scene
}

fn later_steps() -> wire::Onboarding {
    steps([
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Needed,
        StepState::Needed,
        StepState::Needed,
    ])
}

fn entry(path: &str) -> wire::RepositoryEntry {
    wire::RepositoryEntry {
        path: path.to_owned(),
        name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        last_used_ms: Some(NOW),
    }
}

/// The folders Claude Code and Codex ran in lately, all ticked.
pub fn repositories() -> SetupScene {
    let mut scene = opened(SetupStep::Repositories, later_steps(), fleet(true));
    scene.onboarding.suggestions(vec![
        entry("/Users/jordan/src/amux"),
        entry("/Users/jordan/src/amuxcloud"),
        entry("/Users/jordan/work/payments-api"),
        entry("/Users/jordan/notes"),
    ]);
    scene.key(KeyCode::Down);
    scene.key(KeyCode::Down);
    scene.key(KeyCode::Char(' '));
    scene
}

/// A folder of the person's own being typed.
pub fn repositories_add() -> SetupScene {
    let mut scene = repositories();
    for _ in 0..4 {
        scene.key(KeyCode::Down);
    }
    scene.key(KeyCode::Enter);
    for c in "~/src/garden".chars() {
        scene.key(KeyCode::Char(c));
    }
    scene
}

/// Usage counts and automatic reports.
pub fn improve() -> SetupScene {
    let steps = steps([
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Needed,
        StepState::Needed,
    ]);
    opened(SetupStep::Improve, steps, fleet(true))
}

/// Waiting for the phone to start an agent.
pub fn first_agent() -> SetupScene {
    let steps = steps([
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Done,
        StepState::Needed,
    ]);
    opened(SetupStep::FirstAgent, steps, fleet(true))
}

/// A profile signed in to Jordan's account on a Pro trial.
pub fn profile(signed_in: bool) -> wire::Profile {
    use wire::cloud_standing::Standing;
    wire::Profile {
        id: b"profile".to_vec(),
        label: "default".to_owned(),
        email: "jordan@example.com".to_owned(),
        account_name: "Jordan".to_owned(),
        cloud: Some(wire::CloudStanding {
            standing: Some(if signed_in {
                Standing::SignedIn(wire::RelayLink {
                    state: Some(wire::relay_link::State::Connecting(wire::Empty {})),
                })
            } else {
                Standing::Unbound(wire::Empty {})
            }),
        }),
        access: signed_in.then_some(wire::Access {
            pro: true,
            // 18 Jan 2027.
            until_ms: Some(1_800_230_400_000),
            grant: Some(wire::access::Grant::Granted(wire::Empty {})),
        }),
        ..Default::default()
    }
}

/// The settings screen of a desk that finished setting up.
pub fn settings_page(signed_in: bool) -> SettingsScene {
    let settings = wire::Settings {
        start_at_login: Some(true),
        keep_awake: true,
        repositories: vec![
            "/Users/jordan/src/amux".to_owned(),
            "/Users/jordan/src/amuxcloud".to_owned(),
        ],
        auto_reports: true,
        ..settings()
    };
    let mut scene = SettingsScene {
        screen: SettingsScreen::new(settings, Some(profile(signed_in)), "Mac"),
    };
    if !signed_in {
        for _ in 0..9 {
            scene.key(KeyCode::Down);
        }
    }
    scene
}
