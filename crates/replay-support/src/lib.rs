//! Replays a dump bundle through its four stages (facts through the
//! interpreter, the daemon core from its checkpoint, records through the
//! session model, state through the views) with the models' properties
//! judged at every step, and prints a bundle as text. It also says whether
//! a client session's record replays as its whole history does.

pub mod bundle;
pub mod record;
pub mod replay;
pub mod show;
