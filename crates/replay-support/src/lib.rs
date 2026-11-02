//! Replays a dump bundle through its four stages (facts through the
//! interpreter, the daemon core from its checkpoint, records through the
//! session model, state through the views) with the models' properties
//! judged at every step, and prints a bundle as text.

pub mod bundle;
pub mod replay;
pub mod show;
pub mod synthetic;
