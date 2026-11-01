//! The terminal client's storybook: named whole screens, each drawn by the
//! client's own views from data, with no daemon, provider or network behind
//! it.
//!
//! A story is one whole screen a person reaches, in one state that changes
//! how the screen is put together; one piece in one variant (a row kind, an
//! ask card, a message inside an otherwise identical screen) is a component
//! example in `tui::vocabulary` instead. Which stories there are, their
//! order and what each shows is `tests/stories.json`, the list the phone's
//! storybook is held to as well: [`all`] draws exactly its terminal
//! stories, in its order, and a test fails when the two differ.
//!
//! A story is a picture, not a live client. Home is the authored fleet the
//! component goldens use, with the keys a person would press to open hosts,
//! a filter or a new agent; a chat is a session replayed from a recorded
//! interpreter fixture up to the moment it shows. The first segment of a
//! name is its group.

use std::path::Path;
use std::sync::LazyLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Committer, Replayed, replay};
use ratatui::Frame;
use ratatui::layout::Rect;
use serde::Deserialize;
use settings::ChatInSetting;
use tui::Theme;
use tui::chat::ChatView;
use tui::chat::review::ReviewPage;
use tui::report::Report;
use tui::sessions::{agent, caught_up};
use tui::vocabulary::{ChatScene, HomeScene};
use ui_state::{Connection, FleetMsg, FleetState, InputOutcome, Msg, PhaseView, SessionState};
use wire::Kind;

/// The storybook both clients are held to.
const LIST: &str = include_str!("../../../tests/stories.json");

#[derive(Deserialize)]
struct List {
    stories: Vec<Listed>,
}

/// One story of the list.
#[derive(Deserialize)]
pub struct Listed {
    pub name: String,
    pub shows: String,
    pub clients: Vec<String>,
}

static LISTED: LazyLock<Vec<Listed>> = LazyLock::new(|| {
    serde_json::from_str::<List>(LIST)
        .unwrap_or_else(|error| panic!("tests/stories.json: {error}"))
        .stories
});

/// The list's terminal stories, in its order.
pub fn listed() -> impl Iterator<Item = &'static Listed> {
    LISTED
        .iter()
        .filter(|story| story.clients.iter().any(|client| client == "terminal"))
}

pub struct Story {
    pub id: &'static str,
    /// What the story shows, in a sentence, as the list says.
    pub shows: &'static str,
    build: fn() -> Scene,
}

impl Story {
    /// The story's group: the first segment of its name.
    pub fn group(&self) -> &'static str {
        self.id.split('.').next().unwrap_or(self.id)
    }

    /// The screen, made afresh.
    pub fn scene(&self) -> Scene {
        (self.build)()
    }
}

/// One whole screen, ready to draw at any size.
pub enum Scene {
    Home(Box<HomeScene>),
    Chat(Box<ChatScene>),
    Review(Box<ReviewPage>),
    /// Report a problem, over the screen it froze: the screen is drawn
    /// first and frozen as drawn, as the client freezes it at the key.
    Report(Box<Scene>),
}

impl Scene {
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        match self {
            Scene::Home(home) => home.draw(frame, area, theme),
            Scene::Chat(chat) => chat.draw(frame, area, theme),
            Scene::Review(page) => page.draw(frame, area, None, theme),
            Scene::Report(under) => {
                under.draw(frame, area, theme);
                let frozen = frame.buffer_mut().clone();
                Report::new(frozen, theme, None).draw(frame, area, theme);
            }
        }
    }
}

/// A story the list's terminal stories name, with what the list says it
/// shows; a name the list lacks is a mistake here.
fn story(id: &'static str, build: fn() -> Scene) -> Story {
    let shows = listed()
        .find(|listed| listed.name == id)
        .unwrap_or_else(|| panic!("{id} is not a terminal story in tests/stories.json"));
    Story {
        id,
        shows: &shows.shows,
        build,
    }
}

/// Every story, in the order the storybook steps through them: the list's.
pub fn all() -> Vec<Story> {
    vec![
        // The agent list.
        story("fleet.first-run", || Scene::Home(Box::new(first_run()))),
        story("fleet.busy", || {
            Scene::Home(Box::new(tui::vocabulary::authored_home()))
        }),
        story("fleet.filter", || filtered("spec")),
        story("fleet.no-matches", || filtered("zebra")),
        story("fleet.stop", stop),
        story("fleet.delete", || {
            let mut home = tui::vocabulary::authored_home();
            home.key(KeyCode::Char('G'));
            home.key(KeyCode::Char('x'));
            Scene::Home(Box::new(home))
        }),
        story("fleet.rename", || {
            let mut home = tui::vocabulary::authored_home();
            home.key(KeyCode::Char('r'));
            for c in "-v2".chars() {
                home.key(KeyCode::Char(c));
            }
            Scene::Home(Box::new(home))
        }),
        // Hosts.
        story("hosts.list", hosts),
        story("hosts.empty", || {
            let mut home = first_run();
            home.key(KeyCode::Char('h'));
            Scene::Home(Box::new(home))
        }),
        // A new agent.
        story("new-agent.claude", || {
            let mut home = tui::vocabulary::authored_home();
            home.key(KeyCode::Char('n'));
            home.offer(wire::Provider::Claude, &claude_offer());
            Scene::Home(Box::new(home))
        }),
        story("new-agent.browsing", || {
            let mut home = tui::vocabulary::authored_home();
            home.key(KeyCode::Char('n'));
            home.offer(wire::Provider::Claude, &claude_offer());
            home.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
            home.key(KeyCode::Char('d'));
            Scene::Home(Box::new(home))
        }),
        story("new-agent.terminal", || {
            let mut home = tui::vocabulary::authored_home();
            home.home.place.chat_in = ChatInSetting::Native;
            home.key(KeyCode::Char('n'));
            Scene::Home(Box::new(home))
        }),
        // Chats.
        story("chat.loading", loading),
        story("chat.working", || {
            chat(Kind::ClaudeSdk, "recorded_multi_turn", Pick::Working)
        }),
        story("chat.conversation", || {
            chat(Kind::ClaudeSdk, "recorded_multi_turn", Pick::Last)
        }),
        story("chat.asking", || {
            chat(Kind::ClaudeSdk, "recorded_question_asked", Pick::Asking)
        }),
        story("chat.plan", || {
            chat(Kind::ClaudeSdk, "recorded_plan_reviewed", Pick::LastAsking)
        }),
        story("chat.queued", || {
            chat(Kind::ClaudeSdk, "recorded_steer_folded", Pick::Queued)
        }),
        story("chat.away", held_while_away),
        story("chat.exited", exited_with_a_draft),
        story("chat.signed-out", || {
            chat(Kind::ClaudeSdk, "recorded_sign_in_problem", Pick::Last)
        }),
        story("chat.overview", || {
            Scene::Chat(Box::new(tui::vocabulary::chat_overview()))
        }),
        story("chat.overview-stop", || {
            Scene::Chat(Box::new(tui::vocabulary::chat_overview_stop()))
        }),
        story("chat.settings", || {
            Scene::Chat(Box::new(tui::vocabulary::chat_settings()))
        }),
        // Review.
        story("review.page", || {
            Scene::Review(Box::new(tui::vocabulary::commented_review()))
        }),
        // Reporting a problem.
        story("report.ready", || {
            Scene::Report(Box::new(Scene::Home(Box::new(
                tui::vocabulary::authored_home(),
            ))))
        }),
    ]
}

/// Home narrowed by `/` to the agents whose names match `word`.
fn filtered(word: &str) -> Scene {
    let mut home = tui::vocabulary::authored_home();
    home.key(KeyCode::Char('/'));
    for c in word.chars() {
        home.key(KeyCode::Char(c));
    }
    Scene::Home(Box::new(home))
}

/// The Stop window over the working agent, which has background jobs and
/// a subagent for the window to name.
fn stop() -> Scene {
    use wire::inventory_event::Of;

    let mut home = tui::vocabulary::authored_home();
    let key = ui_state::agent_key(home.fleet.find(b"quiet-heron").expect("listed"));
    let mut working = home.fleet.agent(&key).expect("listed").clone();
    working.background_jobs = 2;
    working.descendants = 1;
    home.fleet
        .update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(Of::Agent(working)),
        })));
    home.home.select(key);
    home.key(KeyCode::Char('x'));
    Scene::Home(Box::new(home))
}

/// The hosts window over home, with a paired machine reached through the
/// relay and one found nearby that is not paired.
fn hosts() -> Scene {
    use wire::inventory_event::Of;

    let mut home = tui::vocabulary::authored_home();
    for host in [
        wire::HostEntry::trusted_host(
            b"laptop".as_slice(),
            "laptop",
            wire::trusted::Reach::Online(wire::RouteKind::Relay as i32),
        ),
        wire::HostEntry::candidate_host(
            b"den-mini".as_slice(),
            "den-mini",
            vec!["192.168.1.24:7420".into()],
        ),
    ] {
        home.fleet
            .update(FleetMsg::Event(Box::new(wire::InventoryEvent {
                of: Some(Of::Host(host)),
            })));
    }
    home.key(KeyCode::Char('h'));
    Scene::Home(Box::new(home))
}

/// A chat just opened: nothing of it held yet, and long enough since it
/// opened that it says it is loading.
fn loading() -> Scene {
    let opened = 1_790_000_000_000;
    let mut state = SessionState::new(agent(Kind::ClaudeSdk), tui::chat::layout::CAP as usize);
    state.update(Msg::Connection(Connection::Live));
    Scene::Chat(Box::new(ChatScene {
        view: ChatView::new(b"agent".to_vec(), opened, false),
        state,
        now: opened + 1_000,
    }))
}

/// Which moment of a recorded session a story shows.
enum Pick {
    /// The first frame with a turn at work and something already said.
    Working,
    /// The first frame with an ask open.
    Asking,
    /// The last frame with an ask open, past any the turn asked first.
    LastAsking,
    /// The first frame with a prompt queued behind the turn.
    Queued,
    /// Where the recording ends.
    Last,
}

fn chat(kind: Kind, fixture: &str, pick: Pick) -> Scene {
    Scene::Chat(Box::new(recorded(kind, fixture, pick)))
}

/// A recorded session at the frame `pick` names.
fn recorded(kind: Kind, fixture: &str, pick: Pick) -> ChatScene {
    let frames = replayed(kind, fixture);
    let found = match pick {
        Pick::Working => frames
            .iter()
            .find(|(state, _)| state.phase() == PhaseView::Working && state.transcript().len() > 2),
        Pick::Asking => frames
            .iter()
            .find(|(state, _)| ui_view::ask_card(state).is_some()),
        Pick::LastAsking => frames
            .iter()
            .rfind(|(state, _)| ui_view::ask_card(state).is_some()),
        Pick::Queued => frames.iter().find(|(state, _)| !state.queue().is_empty()),
        Pick::Last => frames.last(),
    };
    let (state, now) = found
        .cloned()
        .unwrap_or_else(|| panic!("{fixture}: no frame shows what its story names"));
    ChatScene {
        view: ChatView::new(b"agent".to_vec(), now, false),
        state,
        now,
    }
}

fn held_while_away() -> Scene {
    let mut scene = recorded(Kind::ClaudeSdk, "recorded_multi_turn", Pick::Last);
    scene.state.update(Msg::Host(wire::HostEntry {
        host: Some(wire::Host {
            host_id: b"host".to_vec(),
            name: "studio".into(),
            ..Default::default()
        }),
        ..Default::default()
    }));
    scene.state.update(Msg::Event(wire::SessionEvent {
        of: Some(wire::session_event::Of::Detached(wire::Detached {})),
    }));
    scene.state.update(Msg::Send(wire::Input::new(
        b"held".to_vec(),
        wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "Now say the word back to me.".into(),
                attachments: vec![],
            })),
        }),
    )));
    Scene::Chat(Box::new(scene))
}

fn exited_with_a_draft() -> Scene {
    let mut scene = recorded(Kind::ClaudeSdk, "recorded_failing_command", Pick::Last);
    let mut exited = scene.state.agent().clone();
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    exited.exit_cause = wire::ExitCause::ProviderExited as i32;
    exited.exit_detail = Some("exit code 1".into());
    scene.state.update(Msg::Entry(exited));
    let theme = Theme::dark(tui::ColorMode::TrueColor);
    for c in "Carry on from the failing command.".chars() {
        scene
            .view
            .key(&scene.state, KeyEvent::from(KeyCode::Char(c)), theme);
    }
    Scene::Chat(Box::new(scene))
}

/// The session after each frame of a recorded fixture, with the newest
/// item's time: caught up after the first frame, as a live chat is.
fn replayed(kind: Kind, fixture: &str) -> Vec<(SessionState, i64)> {
    let dir = match kind {
        Kind::ClaudePty => "claude_pty",
        Kind::ClaudeSdk => "claude_sdk",
        _ => "codex",
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(dir)
        .join(format!("{fixture}.json"));
    let frames: Vec<Replayed> = match kind {
        Kind::ClaudePty => replay::<ClaudePty>(&path),
        Kind::ClaudeSdk => replay::<ClaudeSdk>(&path),
        _ => replay::<Codex>(&path),
    }
    .unwrap_or_else(|error| panic!("{dir}/{fixture}: {error}"));
    let mut state = SessionState::new(agent(kind), tui::chat::layout::CAP as usize);
    state.update(Msg::Connection(Connection::Live));
    let mut committer = Committer::default();
    let mut out = Vec::new();
    let mut at = 0;
    for (index, frame) in frames.iter().enumerate() {
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        for event in committer.commit(&frame.step) {
            state.update(Msg::Event(event));
        }
        if index == 0 {
            state.update(caught_up(committer.revision));
        }
        if let Some((hash, catalogue)) = &frame.catalogue {
            state.update(Msg::Catalogue {
                hash: hash.clone(),
                catalogue: catalogue.clone(),
            });
        }
        at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(at)
            .max(at);
        out.push((state.clone(), at));
    }
    out
}

/// Home with the local host and nothing on it.
fn first_run() -> HomeScene {
    use wire::inventory_event::Of;

    let mut fleet = FleetState::new();
    for of in [
        Of::Host(wire::HostEntry::trusted_host(
            b"desk".as_slice(),
            "desk",
            wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
        )),
        Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    ] {
        fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(of),
        })));
    }
    fleet.update(FleetMsg::Connection(Connection::Live));
    HomeScene {
        home: tui::vocabulary::desk_home(),
        fleet,
        lines: Default::default(),
        now: 1_800_000_000_000,
    }
}

/// What a host's Claude offers a new agent, as its catalogue says.
fn claude_offer() -> wire::Catalogue {
    let permission = |value: &str, display_name: &str| wire::OfferedPermission {
        value: value.into(),
        display_name: display_name.into(),
        settable: true,
        ..Default::default()
    };
    wire::Catalogue {
        models: vec![
            wire::OfferedModel {
                value: "opus".into(),
                display_name: "Opus".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into()],
                ..Default::default()
            },
            wire::OfferedModel {
                value: "sonnet".into(),
                display_name: "Sonnet".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into()],
                ..Default::default()
            },
        ],
        permissions: vec![
            wire::OfferedPermission {
                normal: true,
                ..permission("default", "Ask")
            },
            permission("acceptEdits", "Accept edits"),
            permission("plan", "Plan"),
            wire::OfferedPermission {
                never_asks: true,
                ..permission("bypassPermissions", "Never ask")
            },
        ],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui::ColorMode;

    use super::*;

    /// The storybook is the list's terminal stories, by the list's names
    /// and in its order; a difference is named.
    #[test]
    fn the_stories_are_the_lists_terminal_stories_in_its_order() {
        let ours: Vec<&str> = all().iter().map(|story| story.id).collect();
        let listed: Vec<&str> = listed().map(|story| story.name.as_str()).collect();
        let missing: Vec<&&str> = listed.iter().filter(|name| !ours.contains(name)).collect();
        let unlisted: Vec<&&str> = ours.iter().filter(|id| !listed.contains(id)).collect();
        assert!(
            missing.is_empty() && unlisted.is_empty(),
            "tests/stories.json lists terminal stories the storybook lacks: {missing:?}; \
             the storybook has stories the list does not give the terminal: {unlisted:?}"
        );
        if let Some(at) = (0..ours.len()).find(|&at| ours[at] != listed[at]) {
            panic!(
                "story {} is {} in the storybook but {} in tests/stories.json",
                at + 1,
                ours[at],
                listed[at]
            );
        }
    }

    /// Every story draws at every size the storybook offers, in both
    /// shipped palettes; names are unique and each group is contiguous, so
    /// stepping by group visits every story once.
    #[test]
    fn every_story_draws_at_every_size() {
        let stories = all();
        let mut seen = std::collections::BTreeSet::new();
        let mut groups: Vec<&str> = Vec::new();
        for story in &stories {
            assert!(seen.insert(story.id), "{} is named twice", story.id);
            if groups.last() != Some(&story.group()) {
                assert!(
                    !groups.contains(&story.group()),
                    "{}'s group is split",
                    story.id
                );
                groups.push(story.group());
            }
            for (width, height) in [(80, 24), (110, 34), (60, 24), (200, 60)] {
                for theme in [
                    Theme::dark(ColorMode::TrueColor),
                    Theme::light(ColorMode::TrueColor),
                ] {
                    let mut scene = story.scene();
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| scene.draw(frame, frame.area(), theme))
                        .unwrap();
                }
            }
        }
    }
}
