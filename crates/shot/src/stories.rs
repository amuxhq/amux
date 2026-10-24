//! The terminal client's storybook: named whole screens, each drawn by the
//! client's own views from data, with no daemon, provider or network behind
//! it.
//!
//! A story is a picture, not a live client. Home is the authored fleet the
//! component goldens use, with the keys a person would press to open hosts,
//! a filter or a new agent; a chat is a session replayed from a recorded
//! interpreter fixture up to the moment it shows. Names are shared with the
//! phone's storybook where the concept is the same (`fleet.busy`,
//! `chat.plan`, `review.page`), and the first segment of a name is its
//! group. The list is in screen order: home, hosts, new agent, chat, asks,
//! review.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Replayed, replay};
use ratatui::Frame;
use ratatui::layout::Rect;
use tui::Theme;
use tui::chat::ChatView;
use tui::chat::review::ReviewPage;
use tui::sessions::{Committer, agent, caught_up};
use tui::setup::ChatIn;
use tui::vocabulary::{ChatScene, HomeScene};
use ui_state::{Connection, FleetMsg, FleetState, InputOutcome, Msg, PhaseView, SessionState};
use wire::Kind;

pub struct Story {
    pub id: &'static str,
    /// What the story shows, in a sentence.
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
}

impl Scene {
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        match self {
            Scene::Home(home) => home.draw(frame, area, theme),
            Scene::Chat(chat) => chat.draw(frame, area, theme),
            Scene::Review(page) => page.draw(frame, area, None, theme),
        }
    }
}

macro_rules! story {
    ($id:literal, $shows:literal, $build:expr) => {
        Story {
            id: $id,
            shows: $shows,
            build: $build,
        }
    };
}

/// Every story, in the order the storybook steps through them.
pub fn all() -> Vec<Story> {
    vec![
        // Home.
        story!(
            "fleet.first-run",
            "Home on a machine with no agents yet: what to do first.",
            || { Scene::Home(Box::new(first_run())) }
        ),
        story!(
            "fleet.busy",
            "Home over a day's fleet: two hosts, one away, a dozen agents in every standing, with families, branches and the exited fold open.",
            || Scene::Home(Box::new(tui::vocabulary::authored_home()))
        ),
        story!(
            "fleet.filter",
            "Home narrowed by `/` to the agents whose names match \"spec\".",
            || {
                let mut home = tui::vocabulary::authored_home();
                home.key(KeyCode::Char('/'));
                for c in "spec".chars() {
                    home.key(KeyCode::Char(c));
                }
                Scene::Home(Box::new(home))
            }
        ),
        // Hosts.
        story!(
            "hosts.list",
            "The hosts screen `h` opens over home: each host, how it is reached, and the one away.",
            || {
                let mut home = tui::vocabulary::authored_home();
                home.key(KeyCode::Char('h'));
                Scene::Home(Box::new(home))
            }
        ),
        // A new agent.
        story!(
            "new-agent.claude",
            "The new-agent composer `n` opens on home, with its edge of name, agent, model, effort, folder and host.",
            || {
                let mut home = tui::vocabulary::authored_home();
                home.key(KeyCode::Char('n'));
                home.offer(wire::Provider::Claude, &claude_offer());
                Scene::Home(Box::new(home))
            }
        ),
        story!(
            "new-agent.terminal",
            "The New Agent window `n` opens when the person chats in each agent's own terminal.",
            || {
                let mut home = tui::vocabulary::authored_home();
                home.chat_in = ChatIn::Terminal;
                home.key(KeyCode::Char('n'));
                Scene::Home(Box::new(home))
            }
        ),
        // Chats.
        story!(
            "chat.working",
            "A Claude turn at work: prose so far and the step it is on.",
            || { chat(Kind::ClaudeSdk, "recorded_multi_turn", Pick::Working) }
        ),
        story!(
            "chat.conversation",
            "A finished multi-turn Claude conversation.",
            || { chat(Kind::ClaudeSdk, "recorded_multi_turn", Pick::Last) }
        ),
        story!(
            "chat.tools",
            "Terminal Claude's reads, edits and commands as rows.",
            || { chat(Kind::ClaudePty, "recorded_tools", Pick::Last) }
        ),
        story!(
            "chat.failure",
            "A command that failed, and the turn that followed it.",
            || { chat(Kind::ClaudeSdk, "recorded_failing_command", Pick::Last) }
        ),
        story!("chat.interrupted", "A turn stopped part way.", || {
            chat(Kind::ClaudeSdk, "recorded_interrupted", Pick::Last)
        }),
        story!("chat.task-list", "A turn that kept a task list.", || {
            chat(Kind::ClaudeSdk, "recorded_task_list", Pick::Last)
        }),
        story!(
            "chat.subagent",
            "A turn that handed work to a subagent.",
            || { chat(Kind::ClaudeSdk, "recorded_subagent_task", Pick::Last) }
        ),
        story!(
            "chat.background",
            "A command left running in the background.",
            || { chat(Kind::ClaudeSdk, "recorded_background_shell", Pick::Last) }
        ),
        story!(
            "chat.markdown",
            "A reply in every markdown construct an agent writes: headings, nested and numbered lists, a table, fenced code, a quote, links, a rule and emphasis.",
            markdown
        ),
        story!(
            "chat.queued",
            "A prompt queued behind a turn still running its command.",
            || { chat(Kind::ClaudeSdk, "recorded_steer_folded", Pick::Queued) }
        ),
        story!(
            "chat.held",
            "The host is away: the chat says so and is not current, and a prompt sent meanwhile waits to go when the host is back.",
            held_while_away
        ),
        story!(
            "chat.exited",
            "An agent whose provider crashed, with a draft in the composer that Enter sends as it resumes.",
            exited_with_a_draft
        ),
        story!(
            "chat.exited-resumed",
            "An agent that exited mid-task and was resumed, both marked in the transcript.",
            || { chat(Kind::Codex, "interrupt_exited", Pick::Last) }
        ),
        story!(
            "chat.compacted",
            "A conversation compacted, with how far it shrank.",
            || { chat(Kind::ClaudeSdk, "recorded_compacted", Pick::Last) }
        ),
        story!(
            "chat.edits",
            "A Codex turn whose three patches fold into one row.",
            || { chat(Kind::Codex, "recorded_file_changes", Pick::Last) }
        ),
        story!(
            "chat.api-error",
            "A turn the provider's API refused, its error marked in the transcript.",
            || { chat(Kind::ClaudePty, "recorded_api_error", Pick::Last) }
        ),
        story!(
            "chat.tool-servers",
            "Tool servers that need signing in and one that failed to start, called out above the composer.",
            || { chat(Kind::ClaudeSdk, "recorded_failed_tool_server", Pick::Last) }
        ),
        story!(
            "chat.signed-out.claude",
            "Claude refused its credential: the card in place of the composer says how to sign in on its host.",
            || { chat(Kind::ClaudeSdk, "recorded_sign_in_problem", Pick::Last) }
        ),
        story!(
            "chat.signed-out.codex",
            "Codex signed out: the card in place of the composer says to run codex login on its host.",
            || { chat(Kind::Codex, "recorded_signed_out", Pick::Last) }
        ),
        story!("chat.codex", "A Codex turn from prompt to reply.", || {
            chat(Kind::Codex, "recorded_turn_round_trip", Pick::Last)
        }),
        story!(
            "chat.codex-exploring",
            "Codex exploring a repository: its reads and searches folded.",
            || { chat(Kind::Codex, "recorded_exploring", Pick::Last) }
        ),
        story!(
            "chat.plan",
            "A plan Claude proposes, waiting to be approved.",
            || { chat(Kind::ClaudeSdk, "recorded_plan_reviewed", Pick::LastAsking) }
        ),
        story!(
            "chat.changes.uncommitted",
            "The overview open beside a chat: background jobs and the uncommitted files by folder.",
            || { surroundings(0) }
        ),
        story!(
            "chat.changes.branch",
            "The overview counting everything on the branch.",
            || { surroundings(1) }
        ),
        story!(
            "chat.usage.claude",
            "A Claude chat near a usage limit, the overview listing every window.",
            || { surroundings(2) }
        ),
        story!(
            "chat.usage.codex",
            "A Codex chat near a usage limit, with the credits left.",
            || { surroundings(3) }
        ),
        // Asks.
        story!("ask.permission", "Claude asking to run a command.", || {
            chat(
                Kind::ClaudeSdk,
                "recorded_permission_callback",
                Pick::Asking,
            )
        }),
        story!(
            "ask.permission.terminal",
            "Terminal Claude's own permission menu, as a question.",
            || {
                chat(
                    Kind::ClaudePty,
                    "recorded_permission_allow_once",
                    Pick::Asking,
                )
            }
        ),
        story!(
            "ask.approval.codex",
            "Codex asking to approve a command, with its scopes.",
            || { chat(Kind::Codex, "recorded_approval_scopes", Pick::Asking) }
        ),
        story!(
            "ask.question",
            "Claude asking a question with options.",
            || { chat(Kind::ClaudeSdk, "recorded_question_asked", Pick::Asking) }
        ),
        story!(
            "ask.question.every-shape",
            "Several questions at once: single, multiple and with previews.",
            || {
                chat(
                    Kind::ClaudeSdk,
                    "recorded_question_every_shape",
                    Pick::Asking,
                )
            }
        ),
        story!("ask.question.codex", "Codex asking questions.", || {
            chat(Kind::Codex, "recorded_live_questions", Pick::Asking)
        }),
        story!("ask.form", "A tool server's form, through Codex.", || {
            chat(Kind::Codex, "recorded_tool_server_form", Pick::Asking)
        }),
        story!(
            "ask.access",
            "Codex asking for access outside its folder.",
            || { chat(Kind::Codex, "recorded_access_grant", Pick::Asking) }
        ),
        story!(
            "ask.trust-folder",
            "Terminal Claude's first question in a new folder: whether to trust it.",
            || { chat(Kind::ClaudePty, "trust_dialog", Pick::Asking) }
        ),
        story!(
            "ask.terminal-only",
            "A dialog only terminal Claude's own screen can answer.",
            || { chat(Kind::ClaudePty, "unanswerable", Pick::Asking) }
        ),
        // Review.
        story!(
            "review.page",
            "The review page over a two-file diff, one comment made and a second being written.",
            || { Scene::Review(Box::new(tui::vocabulary::commented_review())) }
        ),
    ]
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

/// What an agent writes when it explains: every markdown construct the
/// chat draws, in one reply.
const MARKDOWN: &str = r#"## What changed

The relay now **retries** after a dropped link, with *jittered* backoff, and the
retry budget lives in `RelayConfig::retries`.

1. The link notices the drop.
2. It waits, then dials again:
   - first after 200 ms,
   - then doubling up to 5 s.
3. After the budget it gives up and says so.

| Case | Before | After |
|---|---|---|
| Wi-Fi blip | dropped | resumes |
| Laptop asleep | dropped | resumes on wake |
| Relay restart | 30 s gap | under 2 s |

```rust
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 << attempt.min(5))
}
```

> The phone keeps its own timer; this only changes the host's side.

---

See [the relay design](https://example.com/docs/relay) and
https://example.com/a/very/long/link/that/runs/past/the/edge/of/a/narrow/terminal/window
for the measurements."#;

fn markdown() -> Scene {
    use prost::Message as _;
    use wire::claude_sdk_item::Kind as K;

    let now = 1_790_000_000_000;
    let item = |order: u64, kind: K, text: &str| wire::Item {
        key: format!("k{order}"),
        order,
        revision: order,
        text: text.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeSdkItem { kind: Some(kind) }.encode_to_vec(),
        at_ms: now - 60_000 + order as i64 * 1_000,
        ..Default::default()
    };
    let mut state = SessionState::new(agent(Kind::ClaudeSdk), tui::chat::layout::CAP as usize);
    state.update(Msg::Connection(Connection::Live));
    let event = |of| Msg::Event(wire::SessionEvent { of: Some(of) });
    state.update(event(wire::session_event::Of::Snapshot(wire::Snapshot {
        kind: Kind::ClaudeSdk as i32,
        phase: wire::Phase::Idle as i32,
        ..Default::default()
    })));
    for item in [
        item(
            1,
            K::Prompt(wire::Prompt {}),
            "Explain what the relay change does.",
        ),
        item(2, K::Message(wire::Text { complete: true }), MARKDOWN),
    ] {
        state.update(event(wire::session_event::Of::Item(item)));
    }
    state.update(caught_up(2));
    Scene::Chat(Box::new(ChatScene {
        view: ChatView::new(b"agent".to_vec(), now, false),
        state,
        now,
    }))
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

fn surroundings(index: usize) -> Scene {
    let (_, _, scene) = tui::vocabulary::surroundings()
        .into_iter()
        .nth(index)
        .expect("the overview stories are the vocabulary's");
    Scene::Chat(Box::new(scene))
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
        home: tui::home::Home::default(),
        fleet,
        lines: Default::default(),
        now: 1_800_000_000_000,
        chat_in: ChatIn::Amux,
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
