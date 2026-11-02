//! Replays a dump bundle's records through their stages (the daemon core
//! from its checkpoint, each chat's record through the session model) with
//! the models' properties judged at every step, and prints a bundle as
//! text. It also says whether a client session's record replays as its
//! whole history does.

pub mod bundle;
pub mod record;
pub mod replay;
pub mod show;
pub mod synthetic;
