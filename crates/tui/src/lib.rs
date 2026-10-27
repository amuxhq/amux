//! The amux terminal client: the fleet and the chat.
//!
//! A library the CLI invokes (bare `amux` opens it), never a second
//! executable. It reads the local runtime only through ui-runtime's
//! drivers and composes ui-view's values — fleet rows, chat rows, the ask
//! card, the overview, composer tokens — inside its own layout. It
//! owns panes, the anchor, the expansion set, drafts and focus, and
//! nothing it holds outlives the process.

pub mod app;
pub mod chat;
pub mod clipboard;
pub mod editor;
pub mod fleet;
pub mod highlight;
pub mod home;
mod hosts;
pub(crate) mod markdown;
pub(crate) mod panel;
pub mod report;
pub mod run;
#[cfg(any(test, feature = "fixtures"))]
pub mod sessions;
pub mod setup;
pub mod terminal;
pub(crate) mod text;
pub mod theme;
#[cfg(feature = "fixtures")]
pub mod vocabulary;
pub mod wheel;
mod words;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

pub use app::{App, Flow, Tone, TuiConfig};
pub use run::{AttachFn, AttachReturn, run};
pub use terminal::{
    RESTORE_BYTES, TerminalGuard, install_panic_hook, query_terminal_colors, write_enter_chrome,
    write_osc52, write_restore,
};
pub use theme::{
    ColorMode, ColorPreference, TerminalColors, Theme, ThemeError, ThemeFile, ThemeName, Token,
    Tokens, Variant, detect_color_mode, nearest_ansi, parse_theme_file, theme_from_file,
};
