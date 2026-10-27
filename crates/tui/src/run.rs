//! The event loop: wait for a key, a driver's change signal, a finished
//! task or the next tick, then draw once. It redraws on change and never
//! on a clock, except while something on screen moves with time.
//!
//! Raw attach suspends the chrome: the terminal is restored, the caller's
//! passthrough runs in process, and the chrome comes back on return. When
//! the agent exits or the person detaches there, the TUI exits to the
//! shell; only the passthrough's fleet chord returns to the fleet.

use std::io;
use std::sync::Arc;

use anyhow::Result;
use client::{Client, SystemClock};
use crossterm::event::EventStream;
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use ratatui::Terminal;
use ratatui::backend::{Backend as _, ClearType, CrosstermBackend};
use tokio::sync::watch;
use ui_runtime::Fleet;
use wire::Agent;

use crate::app::{App, Flow, Tone, TuiConfig};
use crate::terminal::TerminalGuard;

/// What the passthrough decided when it gave the terminal back.
#[derive(Debug)]
pub enum AttachReturn {
    /// Back to the fleet, with something to say.
    Fleet(Option<String>),
    /// Leave the TUI for the shell.
    Exit,
}

/// The embedding CLI's raw attach: the terminal is the caller's until the
/// future resolves.
pub type AttachFn = Box<dyn FnMut(Agent) -> BoxFuture<'static, Result<AttachReturn>> + Send>;

enum Leave {
    Quit,
    Attach(Box<Agent>),
}

/// Runs the terminal client until the person quits.
pub async fn run(
    client: Arc<dyn Client>,
    mut config: TuiConfig,
    mut attach: Option<AttachFn>,
) -> Result<()> {
    config.attach = attach.is_some();
    let fleet = Fleet::connect(client.clone(), SystemClock).await?;
    let mut app = App::new(client, fleet, config);
    loop {
        match session(&mut app).await? {
            Leave::Quit => {
                app.keep_drafts().await;
                return Ok(());
            }
            Leave::Attach(agent) => {
                let Some(attach) = attach.as_mut() else {
                    continue;
                };
                match attach(*agent).await {
                    Ok(AttachReturn::Exit) => return Ok(()),
                    Ok(AttachReturn::Fleet(notice)) => {
                        app.close_chat();
                        if let Some(notice) = notice {
                            app.notice(notice, Tone::Info);
                        }
                    }
                    Err(error) => app.notice(format!("attach failed: {error:#}"), Tone::Warn),
                }
            }
        }
    }
}

/// The next input event if one is already waiting, without waiting for
/// one. A zero timeout still polls the stream once, with this task's own
/// waker, so nothing is lost when none is ready.
pub async fn ready(events: &mut EventStream) -> Option<io::Result<crossterm::event::Event>> {
    tokio::time::timeout(std::time::Duration::ZERO, events.next())
        .await
        .ok()
        .flatten()
}

/// One stretch on the alternate screen, until a quit or an attach.
async fn session(app: &mut App) -> Result<Leave> {
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    // A fresh terminal's buffers are blank, so a blank screen matches them.
    // `Terminal::clear` would also ask the terminal where its cursor is and
    // fail if no reply comes in time; after a raw attach, the input reader
    // still holds the wake-up from the last session's dropped event stream,
    // which ends that wait at once.
    terminal.backend_mut().clear_region(ClearType::All)?;
    let mut events = EventStream::new();
    let mut fleet_changed = app.fleet.changed();
    // The receiver sees only changes after it: a first chat whose row
    // arrived before this session began opens from what is already there.
    app.fleet_changed();
    let mut chat: Option<(Vec<u8>, watch::Receiver<()>)> = None;
    let leave = loop {
        app.housekeep();
        let open = app.chat.as_ref().map(|chat| chat.view.agent_id.clone());
        if chat.as_ref().map(|(id, _)| id) != open.as_ref() {
            chat = app
                .chat
                .as_ref()
                .map(|open| (open.view.agent_id.clone(), open.session.changed()));
        }
        crate::terminal::write_begin_frame(terminal.backend_mut())?;
        terminal.draw(|frame| app.draw(frame))?;
        crate::terminal::write_end_frame(terminal.backend_mut())?;
        let tick = app.next_tick();
        let flow = tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => {
                    let mut flow = app.input(event);
                    // Whatever input is already waiting (a trackpad's burst
                    // of wheel events) applies before the one redraw.
                    while matches!(flow, Flow::Continue)
                        && let Some(event) = ready(&mut events).await
                    {
                        flow = app.input(event?);
                    }
                    flow
                }
                Some(Err(error)) => return Err(error.into()),
                None => Flow::Quit,
            },
            changed = fleet_changed.changed() => {
                if changed.is_err() {
                    Flow::Quit
                } else {
                    app.fleet_changed();
                    Flow::Continue
                }
            }
            _ = async {
                match chat.as_mut() {
                    Some((_, changed)) => { let _ = changed.changed().await; }
                    None => std::future::pending::<()>().await,
                }
            } => Flow::Continue,
            Some(event) = app.receiver.recv() => app.event(event),
            _ = async {
                match tick {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                app.tick();
                Flow::Continue
            }
        };
        match flow {
            Flow::Continue => {}
            Flow::Quit => break Leave::Quit,
            Flow::Attach(agent) => break Leave::Attach(agent),
        }
    };
    drop(events);
    drop(terminal);
    guard.restore();
    Ok(leave)
}
