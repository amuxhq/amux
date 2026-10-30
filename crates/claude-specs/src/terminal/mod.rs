//! Driving Claude Code in a terminal for recordings: starting it under a
//! PTY, the semantic input a spec types, and the keymaps that turn that
//! input into bytes for the version observed. amux itself answers Claude's
//! asks through its hooks; recordings keep answering them on Claude's own
//! menus, as a person at the terminal does.

mod input;
pub mod keymap;
mod spawn;

pub use input::*;
pub use spawn::*;
