//! Onboarding's screens, the settings screen, home's offer to finish
//! setting up and the new-agent picker's folders: what each key asks the
//! app to do, and what each screen says.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ui_view::SetupStep;
use wire::answer_onboarding_request::Answer;
use wire::{OnboardingStepId as Id, StepState};

use super::*;
use crate::onboarding::{Onboarding, SetupEffect};
use crate::settings_screen::{SettingsEffect, SettingsScreen};
use crate::setup_scenes::{self as scenes, PHONE, SetupScene};

fn screen(scene: &mut SetupScene, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| scene.draw(frame, frame.area(), theme()))
        .unwrap();
    text(terminal.backend().buffer())
}

fn settings_text(screen: &mut SettingsScreen, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| screen.draw(frame, frame.area(), None, theme()))
        .unwrap();
    text(terminal.backend().buffer())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(scene: &mut SetupScene, code: KeyCode) -> Vec<SetupEffect> {
    let fleet = scene.fleet.clone();
    scene.onboarding.key(key(code), &fleet)
}

fn shown(step: SetupStep) -> SetupEffect {
    SetupEffect::Answer(Answer::Shown(step.id() as i32))
}

fn skip(step: SetupStep) -> SetupEffect {
    SetupEffect::Answer(Answer::Skip(step.id() as i32))
}

/// The lines holding a drawn QR code: block characters and nothing else.
fn code_rows(screen: &str) -> usize {
    screen
        .lines()
        .filter(|line| {
            let blocks = line.chars().filter(|c| "█▀▄".contains(*c)).count();
            blocks > 10
        })
        .count()
}

#[test]
fn a_first_run_opens_the_pairing_window_and_shows_the_notice_once() {
    let fleet = scenes::fleet(false);
    let (_, effects) = Onboarding::open(
        SetupStep::PairPhone,
        false,
        scenes::steps([StepState::Needed; 6], false),
        scenes::settings(),
        scenes::notice(),
        "Mac",
        b"desk",
        &fleet,
    );
    assert_eq!(
        effects,
        [
            SetupEffect::Answer(Answer::NoticeShown(wire::Empty {})),
            shown(SetupStep::PairPhone),
            SetupEffect::OpenPairing,
        ]
    );
    let mut scene = scenes::pair();
    let drawn = screen(&mut scene, 110, 34);
    assert!(drawn.contains("This machine is called desk."), "{drawn}");
    assert!(
        drawn.contains("1. Scan to get amux on your iPhone"),
        "{drawn}"
    );
    assert!(drawn.contains("2. Enter 418 207 in the app"), "{drawn}");
    assert!(drawn.contains("amux sends usage counts"), "{drawn}");
    // The next screen has no notice: it was shown.
    let mut paired = scenes::paired();
    assert!(!screen(&mut paired, 110, 34).contains("amux sends usage counts"));
    // Shown already by another client, it is not shown here at all.
    let (onboarding, effects) = Onboarding::open(
        SetupStep::PairPhone,
        false,
        scenes::steps([StepState::Needed; 6], true),
        scenes::settings(),
        scenes::notice(),
        "Mac",
        b"desk",
        &fleet,
    );
    assert!(!effects.contains(&SetupEffect::Answer(Answer::NoticeShown(wire::Empty {}))));
    let mut scene = SetupScene { onboarding, fleet };
    assert!(!screen(&mut scene, 110, 34).contains("amux sends usage counts"));
}

/// A window tall enough shows the whole pairing code; a usual one, the
/// code of the page alone, which still sends the phone to the App Store;
/// a small one says how much room the code needs.
#[test]
fn the_qr_code_fits_the_window_or_it_says_how_to_make_room() {
    let window = scenes::window();
    let whole = crate::qr::Code::encode(&window.qr_link).unwrap().cells();
    let page = crate::qr::Code::encode("https://amux.sh/p")
        .unwrap()
        .cells();
    let mut scene = scenes::pair();
    let tall = screen(&mut scene, 140, 60);
    assert_eq!(code_rows(&tall), whole.1, "{tall}");
    let usual = screen(&mut scene, 80, 24);
    assert_eq!(code_rows(&usual), page.1, "{usual}");
    assert!(!usual.contains("Make this window"));
    let small = screen(&mut scene, 60, 24);
    assert_eq!(code_rows(&small), 0, "{small}");
    assert!(
        small.contains("Make this window at least 67 columns wide and 22 rows"),
        "{small}"
    );
    // The PIN is there at every size.
    for drawn in [&tall, &usual, &small] {
        assert!(drawn.contains("418 207"), "{drawn}");
    }
}

#[test]
fn the_window_is_renewed_before_it_ends_and_not_while_a_phone_joins() {
    let mut scene = scenes::pair();
    let opened = 1_800_000_000_000;
    let ends = opened + i64::try_from(crate::onboarding::WINDOW_SECONDS * 1000).unwrap();
    assert_eq!(scene.onboarding.tick(opened + 60_000), []);
    let due = scene.onboarding.renew_at().expect("a window to renew");
    assert!(due < ends && due > opened, "{due}");
    assert_eq!(scene.onboarding.tick(due), [SetupEffect::OpenPairing]);
    // The old window closing as the new one opens is not the end.
    let fleet = scene.fleet.clone();
    let closed = wire::PairingStatus::default();
    assert_eq!(scene.onboarding.pairing_status(closed, &fleet), []);
    assert_eq!(scene.onboarding.step, SetupStep::PairPhone);
    scene.onboarding.pairing_opened(scenes::window(), due);
    let mut joining = scenes::pairing();
    assert_eq!(
        joining.onboarding.renew_at(),
        None,
        "a joiner is confirming"
    );
    assert!(screen(&mut joining, 110, 34).contains("Your iPhone is pairing…"));
}

#[test]
fn a_pairing_made_before_the_window_opened_is_not_this_one() {
    let mut scene = scenes::pair();
    let fleet = scenes::fleet(true);
    let earlier = wire::PairingStatus {
        active: false,
        locked_out: false,
        joining: false,
        paired: Some(wire::Peer {
            host_id: b"phone".to_vec(),
            name: PHONE.into(),
            ..Default::default()
        }),
    };
    assert_eq!(scene.onboarding.pairing_status(earlier.clone(), &fleet), []);
    assert_eq!(scene.onboarding.step, SetupStep::PairPhone);
    let open = wire::PairingStatus {
        active: true,
        ..Default::default()
    };
    assert_eq!(scene.onboarding.pairing_status(open, &fleet), []);
    let effects = scene.onboarding.pairing_status(earlier, &fleet);
    assert_eq!(effects[0], SetupEffect::ClosePairing, "{effects:?}");
    assert_eq!(scene.onboarding.step, SetupStep::KeepReachable);
}

#[test]
fn the_machine_is_renamed_from_the_pairing_screen() {
    let mut scene = scenes::pair_rename();
    let drawn = screen(&mut scene, 110, 34);
    assert!(drawn.contains("This machine is called ▎ studio"), "{drawn}");
    assert!(drawn.contains("enter save"), "{drawn}");
    assert_eq!(
        press(&mut scene, KeyCode::Enter),
        [SetupEffect::Rename("studio".into())]
    );
    assert!(screen(&mut scene, 110, 34).contains("This machine is called studio."));
}

#[test]
fn paired_it_asks_to_keep_the_machine_connected_and_applies_both_choices() {
    let mut scene = scenes::paired();
    assert_eq!(scene.onboarding.step, SetupStep::KeepReachable);
    let drawn = screen(&mut scene, 110, 34);
    assert!(drawn.contains(&format!("Paired with {PHONE} ✓")), "{drawn}");
    assert!(drawn.contains("[x] Start amux when you log in"), "{drawn}");
    assert!(
        drawn.contains("[x] Keep this Mac awake while it's plugged in"),
        "{drawn}"
    );
    let confirm = |start_at_login, keep_awake| SetupEffect::Confirm {
        settings: Box::new(wire::UpdateSettingsRequest {
            start_at_login,
            keep_awake: Some(keep_awake),
            ..Default::default()
        }),
        step: Some(SetupStep::KeepReachable),
    };
    // Space on the first changes starting at login.
    press(&mut scene, KeyCode::Char(' '));
    assert_eq!(
        press(&mut scene, KeyCode::Enter),
        [confirm(Some(false), true)]
    );
    // Without a supervisor there is no starting at login to ask about.
    let mut scene = scenes::paired();
    let mut settings = scenes::settings();
    settings.start_at_login = None;
    scene.onboarding.settings_changed(settings);
    let drawn = screen(&mut scene, 110, 34);
    assert!(!drawn.contains("Start amux when you log in"), "{drawn}");
}

#[test]
fn later_skips_each_step_in_turn_and_the_last_goes_home() {
    let mut scene = scenes::pair();
    let effects = press(&mut scene, KeyCode::Esc);
    assert_eq!(
        effects,
        [
            skip(SetupStep::PairPhone),
            SetupEffect::ClosePairing,
            shown(SetupStep::KeepReachable),
        ]
    );
    let effects = press(&mut scene, KeyCode::Esc);
    assert_eq!(
        effects,
        [skip(SetupStep::KeepReachable), shown(SetupStep::SignIn)]
    );
    let mut last = scenes::first_agent();
    assert_eq!(
        press(&mut last, KeyCode::Esc),
        [skip(SetupStep::FirstAgent), SetupEffect::Home(None)]
    );
}

#[test]
fn signing_in_offers_the_phone_just_paired_the_account_when_ticked() {
    let mut scene = scenes::sign_in();
    assert_eq!(scene.onboarding.step, SetupStep::SignIn);
    let drawn = screen(&mut scene, 110, 34);
    assert!(
        drawn.contains(&format!("[x] Also sign in {PHONE}")),
        "{drawn}"
    );
    assert_eq!(press(&mut scene, KeyCode::Enter), [SetupEffect::SignIn]);
    let mut waiting = scenes::sign_in_waiting();
    let drawn = screen(&mut waiting, 110, 34);
    assert!(drawn.contains("It should show WDJB-MJHT."), "{drawn}");
    assert!(
        drawn.contains("https://amux.sh/device?user_code=WDJB-MJHT"),
        "the link shows when the browser did not open: {drawn}"
    );
    let fleet = waiting.fleet.clone();
    let effects = waiting.onboarding.signed_in(&fleet);
    assert_eq!(
        effects,
        [
            SetupEffect::OfferSignIn(b"phone".to_vec()),
            shown(SetupStep::Projects),
            SetupEffect::Suggest,
        ]
    );
    // Unticked, the phone is not offered the account.
    let mut scene = scenes::sign_in();
    press(&mut scene, KeyCode::Char(' '));
    press(&mut scene, KeyCode::Enter);
    let fleet = scene.fleet.clone();
    let effects = scene.onboarding.signed_in(&fleet);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, SetupEffect::OfferSignIn(_))),
        "{effects:?}"
    );
}

#[test]
fn a_phone_signed_in_first_offers_its_account_and_the_offer_page_opens() {
    let mut scene = scenes::sign_in_offer();
    let drawn = screen(&mut scene, 110, 34);
    assert!(
        drawn.contains(&format!("Sign in as jordan@example.com (from {PHONE})?")),
        "{drawn}"
    );
    // Counted once as shown, however often the offers move.
    let offers = vec![wire::SignInOffer {
        host_id: b"phone".to_vec(),
        host_name: PHONE.into(),
        email: "jordan@example.com".into(),
        account_name: "Jordan".into(),
    }];
    assert_eq!(scene.onboarding.offers_changed(offers), []);
    assert_eq!(
        press(&mut scene, KeyCode::Enter),
        [SetupEffect::AnswerOffer {
            host_id: b"phone".to_vec(),
            accept: true,
        }]
    );
    let fleet = scene.fleet.clone();
    let effects = scene.onboarding.offer_answered(true, &fleet);
    assert_eq!(effects[0], SetupEffect::OfferPage);
    assert_eq!(scene.onboarding.step, SetupStep::Projects);
    // Not now: the browser sign-in is offered in its place.
    let mut scene = scenes::sign_in_offer();
    assert_eq!(
        press(&mut scene, KeyCode::Char('n')),
        [SetupEffect::AnswerOffer {
            host_id: b"phone".to_vec(),
            accept: false,
        }]
    );
    assert!(screen(&mut scene, 110, 34).contains("enter sign in   esc later"));
}

#[test]
fn the_shown_offer_is_counted_for_the_side_it_is_on() {
    let fleet = scenes::fleet(true);
    let steps = scenes::steps(
        [
            StepState::Done,
            StepState::Done,
            StepState::Needed,
            StepState::Needed,
            StepState::Needed,
            StepState::Needed,
        ],
        true,
    );
    let (mut onboarding, effects) = Onboarding::open(
        SetupStep::SignIn,
        false,
        steps,
        scenes::settings(),
        scenes::notice(),
        "Mac",
        b"desk",
        &fleet,
    );
    assert_eq!(effects, [shown(SetupStep::SignIn)], "nothing to offer yet");
    let effects = onboarding.offers_changed(vec![wire::SignInOffer {
        host_id: b"phone".to_vec(),
        ..Default::default()
    }]);
    assert_eq!(
        effects,
        [SetupEffect::Answer(Answer::SignInOfferShown(
            wire::OfferRole::Receiver as i32
        ))]
    );
}

#[test]
fn projects_keep_the_ticked_folders_and_the_ones_typed_with_those_chosen_before() {
    let mut scene = scenes::projects();
    let drawn = screen(&mut scene, 110, 34);
    assert!(drawn.contains("[ ] "), "one is unticked: {drawn}");
    assert!(drawn.contains("+ Add a folder…"), "{drawn}");
    let mut chosen = scenes::settings();
    chosen.projects = vec!["/srv/kept".into()];
    scene.onboarding.settings_changed(chosen);
    for _ in 0..4 {
        press(&mut scene, KeyCode::Down);
    }
    press(&mut scene, KeyCode::Enter);
    for c in "/srv/typed".chars() {
        press(&mut scene, KeyCode::Char(c));
    }
    press(&mut scene, KeyCode::Enter);
    let effects = press(&mut scene, KeyCode::Enter);
    let SetupEffect::Confirm { settings, step } = &effects[0] else {
        panic!("{effects:?}");
    };
    assert_eq!(*step, Some(SetupStep::Projects));
    assert_eq!(
        settings.projects.as_ref().unwrap().paths,
        [
            "/srv/kept",
            "/Users/jordan/src/amux",
            "/Users/jordan/src/amuxcloud",
            "/Users/jordan/notes",
            "/srv/typed",
        ]
    );
    // A folder the daemon refuses keeps the screen, saying why.
    scene
        .onboarding
        .confirm_failed("/srv/typed does not exist".into());
    assert!(screen(&mut scene, 110, 34).contains("/srv/typed does not exist"));
    let fleet = scene.fleet.clone();
    let effects = scene.onboarding.confirmed(SetupStep::Projects, &fleet);
    assert_eq!(effects, [shown(SetupStep::Improve)]);
}

#[test]
fn improve_sets_usage_counts_and_reports_as_ticked() {
    let mut scene = scenes::improve();
    let drawn = screen(&mut scene, 110, 34);
    assert!(
        drawn.contains("Never your prompts, code, or file names."),
        "{drawn}"
    );
    press(&mut scene, KeyCode::Down);
    press(&mut scene, KeyCode::Char(' '));
    assert_eq!(
        press(&mut scene, KeyCode::Enter),
        [SetupEffect::Confirm {
            settings: Box::new(wire::UpdateSettingsRequest {
                telemetry: Some(true),
                auto_reports: Some(true),
                ..Default::default()
            }),
            step: Some(SetupStep::Improve),
        }]
    );
}

#[test]
fn the_agent_the_phone_starts_lands_on_home() {
    use wire::inventory_event::Of;
    let mut scene = scenes::first_agent();
    let drawn = screen(&mut scene, 110, 34);
    assert!(
        drawn.contains("On your phone, tap + and pick a project."),
        "{drawn}"
    );
    let fleet = scene.fleet.clone();
    assert_eq!(scene.onboarding.fleet_changed(&fleet), []);
    let agent = wire::Agent {
        agent_id: b"from-phone".to_vec(),
        host_id: b"desk".to_vec(),
        kind: Kind::ClaudeSdk as i32,
        name: "tidy-otter".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        ..Default::default()
    };
    scene
        .fleet
        .update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(Of::Agent(agent.clone())),
        })));
    let fleet = scene.fleet.clone();
    assert_eq!(
        scene.onboarding.fleet_changed(&fleet),
        [SetupEffect::Home(Some(ui_state::agent_key(&agent)))]
    );
}

#[test]
fn a_step_done_elsewhere_moves_the_screen_on() {
    let mut scene = scenes::sign_in();
    let fleet = scene.fleet.clone();
    let mut steps = scenes::steps([StepState::Needed; 6], true);
    steps.steps[2].state = StepState::Done as i32;
    let effects = scene.onboarding.steps_changed(steps, &fleet);
    assert_eq!(effects, [shown(SetupStep::Projects), SetupEffect::Suggest]);
}

#[test]
fn finishing_walks_the_skipped_steps_too() {
    let fleet = scenes::fleet(true);
    let steps = scenes::steps(
        [
            StepState::Done,
            StepState::NotNeeded,
            StepState::Skipped,
            StepState::Done,
            StepState::Skipped,
            StepState::Done,
        ],
        true,
    );
    let (mut onboarding, _) = Onboarding::open(
        SetupStep::SignIn,
        true,
        steps,
        scenes::settings(),
        scenes::notice(),
        "Mac",
        b"desk",
        &fleet,
    );
    let effects = onboarding.key(key(KeyCode::Esc), &fleet);
    assert_eq!(
        effects,
        [skip(SetupStep::SignIn), shown(SetupStep::Improve)],
        "a skipped step is walked when finishing"
    );
    assert_eq!(
        onboarding.key(key(KeyCode::Esc), &fleet),
        [skip(SetupStep::Improve), SetupEffect::Home(None)]
    );
}

#[test]
fn every_setting_changes_at_once() {
    let mut page = scenes::settings_page(true).screen;
    let drawn = settings_text(&mut page, 110, 40);
    for words in [
        "This machine's name",
        "desk",
        "Start amux when you log in",
        "Keep this Mac awake while it's plugged in",
        "+ Add a folder…",
        "amux's chat",
        "Send usage counts",
        "Send reports automatically",
        "Signed in as jordan@example.com",
        "Pro until",
    ] {
        assert!(drawn.contains(words), "{words}: {drawn}");
    }
    let update = |request: wire::UpdateSettingsRequest| SettingsEffect::Update(Box::new(request));
    page.key(key(KeyCode::Down));
    assert_eq!(
        page.key(key(KeyCode::Enter)),
        [update(wire::UpdateSettingsRequest {
            start_at_login: Some(false),
            ..Default::default()
        })]
    );
    page.key(key(KeyCode::Down));
    assert_eq!(
        page.key(key(KeyCode::Char(' '))),
        [update(wire::UpdateSettingsRequest {
            keep_awake: Some(false),
            ..Default::default()
        })]
    );
    // The first project, removed.
    page.key(key(KeyCode::Down));
    assert_eq!(
        page.key(key(KeyCode::Char('x'))),
        [update(wire::UpdateSettingsRequest {
            projects: Some(wire::ProjectList {
                paths: vec!["/Users/jordan/src/amuxcloud".into()],
            }),
            ..Default::default()
        })]
    );
    // A folder added to the list.
    page.key(key(KeyCode::Down));
    page.key(key(KeyCode::Down));
    page.key(key(KeyCode::Enter));
    for c in "/srv/new".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        page.key(key(KeyCode::Enter)),
        [update(wire::UpdateSettingsRequest {
            projects: Some(wire::ProjectList {
                paths: vec![
                    "/Users/jordan/src/amux".into(),
                    "/Users/jordan/src/amuxcloud".into(),
                    "/srv/new".into(),
                ],
            }),
            ..Default::default()
        })]
    );
    page.key(key(KeyCode::Down));
    assert_eq!(
        page.key(key(KeyCode::Enter)),
        [update(wire::UpdateSettingsRequest {
            chat_in: Some(wire::ChatIn::Native as i32),
            ..Default::default()
        })]
    );
    page.key(key(KeyCode::Char('G')));
    assert_eq!(page.key(key(KeyCode::Enter)), [SettingsEffect::SignOut]);
    // The name, renamed in its field.
    page.key(key(KeyCode::Char('g')));
    page.key(key(KeyCode::Enter));
    for _ in 0.."desk".len() {
        page.key(key(KeyCode::Backspace));
    }
    for c in "studio".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        page.key(key(KeyCode::Enter)),
        [update(wire::UpdateSettingsRequest {
            host_name: Some("studio".into()),
            ..Default::default()
        })]
    );
    assert_eq!(page.key(key(KeyCode::Esc)), [SettingsEffect::Close]);
}

#[test]
fn signed_out_the_settings_sign_in_in_the_browser_and_show_the_code() {
    let mut page = scenes::settings_page(false).screen;
    let drawn = settings_text(&mut page, 110, 40);
    assert!(drawn.contains("Not signed in"), "{drawn}");
    assert!(drawn.contains("enter sign in"), "{drawn}");
    assert_eq!(page.key(key(KeyCode::Enter)), [SettingsEffect::SignIn]);
    page.sign_in_code(
        wire::SignInCode {
            verification_uri_complete: "https://amux.sh/device?user_code=AB-CD".into(),
            user_code: "AB-CD".into(),
            ..Default::default()
        },
        true,
    );
    let drawn = settings_text(&mut page, 110, 40);
    assert!(drawn.contains("It should show AB-CD."), "{drawn}");
    assert!(!drawn.contains("did not open"), "{drawn}");
    // Leaving abandons the sign-in.
    assert_eq!(
        page.key(key(KeyCode::Esc)),
        [SettingsEffect::CancelSignIn, SettingsEffect::Close]
    );
}

#[test]
fn home_offers_to_finish_setting_up_and_takes_the_settings_key() {
    let mut home = new_home();
    let fleet = scenes::fleet(false);
    home.finish = 2;
    assert_eq!(
        home.key(&fleet, key(KeyCode::Char(','))),
        [FleetEffect::Settings]
    );
    home.key(&fleet, key(KeyCode::Down));
    assert_eq!(
        home.key(&fleet, key(KeyCode::Enter)),
        [FleetEffect::FinishSetup]
    );
    assert_eq!(
        home.key(&fleet, key(KeyCode::Char('x'))),
        [FleetEffect::DismissSetup]
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            home.draw(
                frame,
                frame.area(),
                &fleet,
                &HashMap::new(),
                None,
                0,
                theme(),
            )
        })
        .unwrap();
    let drawn = text(terminal.backend().buffer());
    assert!(drawn.contains("Finish setting up (2)"), "{drawn}");
    assert!(drawn.contains("enter finish   x dismiss"), "{drawn}");
}

#[test]
fn the_picker_offers_the_hosts_projects_then_its_recent_folders() {
    let mut setup = crate::setup::Setup::defaults(
        settings::ChatInSetting::Amux,
        "/src/here",
        b"desk",
        &settings::NewAgentSettings::default(),
    );
    assert!(setup.wants_folders());
    let entry = |path: &str| wire::ProjectEntry {
        path: path.into(),
        ..Default::default()
    };
    let answer = wire::ListRepositoriesResponse {
        recent: vec![entry("/src/recent"), entry("/src/app")],
        projects: vec![entry("/src/app"), entry("/src/lib")],
    };
    setup.take_folders(b"desk", &answer);
    assert!(!setup.wants_folders());
    assert_eq!(
        setup.folder_choices(),
        ["/src/here", "/src/app", "/src/lib", "/src/recent"],
        "the folder the client started in, then the projects, then recent ones"
    );
    // A folder the host lists keeps its place among the others.
    setup.pick(crate::setup::Item::Folder, "/src/lib");
    assert_eq!(
        setup.folder_choices(),
        ["/src/app", "/src/lib", "/src/recent"]
    );
    // On another host the folder is the first that host offers.
    setup.pick(crate::setup::Item::Host, "6c6170746f70");
    assert!(setup.wants_folders());
    setup.take_folders(b"laptop", &answer);
    assert_eq!(setup.folder, "/src/app");
}

#[test]
fn every_step_has_a_title() {
    for id in [
        Id::PairPhone,
        Id::KeepReachable,
        Id::SignIn,
        Id::Projects,
        Id::Improve,
        Id::FirstAgent,
    ] {
        let step = SetupStep::of(id).unwrap();
        assert!(!crate::onboarding::title(step).is_empty());
    }
}
