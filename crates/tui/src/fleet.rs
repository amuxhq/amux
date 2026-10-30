//! The fleet: home, where a chat always returns, and what its keys ask the
//! event loop to do.

use std::collections::HashMap;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame as Paint;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ui_state::{AgentKey, FleetState};
use ui_view::SessionLine;
use wire::Attachment;

use crate::home::{self, Home};
use crate::theme::Theme;

/// What a fleet key asks the event loop to do.
#[derive(Clone, Debug, PartialEq)]
pub enum FleetEffect {
    Open(AgentKey),
    /// The agent's own interface, raw.
    Attach(AgentKey),
    /// Create an agent with its first prompt, and open its chat or stay.
    Start {
        setup: Box<crate::setup::Setup>,
        text: String,
        attachments: Vec<Attachment>,
        open: bool,
    },
    Rename {
        agent: AgentKey,
        name: String,
    },
    Stop(AgentKey),
    /// Takes back a stop held while the agent's host was out of reach.
    CancelStop(AgentKey),
    Delete(AgentKey),
    /// Leave amux.
    Quit,
    /// Show the key help.
    Help,
    /// Freeze the screen and report a problem with it.
    Report,
}

#[derive(Debug, Default)]
pub struct FleetView {
    /// Whether raw attach is offered.
    pub attach: bool,
    /// This build's version, compared with the daemon's.
    pub version: String,
    /// The host whose daemon this client talks to.
    pub local_host: Vec<u8>,
    /// Where a new agent works, as the person would write it.
    pub working_dir: String,
    pub home: Home,
    /// Where the person chats, which decides how a new agent starts.
    pub chat_in: settings::ChatInSetting,
    /// What each agent starts with.
    pub defaults: settings::NewAgentSettings,
}

impl FleetView {
    pub fn select(&mut self, agent: AgentKey) {
        self.home.select(agent);
    }

    /// The new agent's setup while its screen is open.
    pub fn open_setup(&mut self) -> Option<&mut crate::setup::Setup> {
        let place = home::Place {
            local_host: &self.local_host,
            version: &self.version,
            working_dir: &self.working_dir,
            attach: self.attach,
            chat_in: self.chat_in,
            defaults: &self.defaults,
        };
        self.home.open_setup(&place)
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self) -> bool {
        self.home.field_text()
    }

    pub fn kill_field(&mut self) -> bool {
        self.home.kill_field()
    }

    /// A bracketed paste types into whichever field has the keys.
    pub fn paste(&mut self, text: &str) {
        self.home.paste(text);
    }

    pub fn key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        self.home.key(fleet, key, self.attach)
    }

    pub fn mouse(&mut self, fleet: &FleetState, event: MouseEvent) -> Vec<FleetEffect> {
        self.home.mouse(fleet, event, self.attach)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        fleet: &FleetState,
        lines: &HashMap<AgentKey, SessionLine>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) {
        let place = home::Place {
            local_host: &self.local_host,
            version: &self.version,
            working_dir: &self.working_dir,
            attach: self.attach,
            chat_in: self.chat_in,
            defaults: &self.defaults,
        };
        self.home
            .draw(paint, area, fleet, lines, footer, now_ms, theme, &place);
    }
}
