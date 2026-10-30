//! Driving Claude Code in a terminal for recordings: starting it under a
//! PTY, the semantic input a spec types, and the keymap that turns that
//! input into bytes. amux itself answers Claude's
//! asks through its hooks; recordings keep answering them on Claude's own
//! menus, as a person at the terminal does.

mod input;
pub mod keymap;
mod spawn;

pub use input::*;
pub use spawn::*;
