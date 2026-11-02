//! Replays what a problem left behind through its stages with the models'
//! properties and the interpreters' and cores' checks judged at every
//! step: a report's records (each agent's record as a made-up stream, the
//! daemon core from its checkpoint, each chat's record through the session
//! model), an installation's own rings, and a relay's or supervisor's
//! window. Prints a bundle as text, says whether a client session's record
//! replays as its whole history does, and fetches a sent report.

pub mod bundle;
pub mod fetch;
pub mod record;
pub mod replay;
pub mod repro;
pub mod show;
pub mod synthetic;
pub mod window;
