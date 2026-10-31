//! The driver the terminal specifications record and replay through.
//!
//! It runs Claude in a terminal and reads its hooks and transcript, the way
//! a program embedding Claude would. It exists so a specification can make
//! a claim about provider traffic; the product hosts Claude in its agent
//! process.

pub mod pty;
