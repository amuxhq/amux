//! The names the core gives the things the driver holds for it.
//!
//! The core cannot hold a socket, a response stream, an armed timer or a
//! running job, so it names each and the driver keeps the name beside the
//! thing. Every name is minted by the core and is a plain value, so it
//! survives a checkpoint.

/// A call the driver holds open: a client's or peer's request awaiting its
/// answer, or a request the core made to a peer awaiting the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(pub u64);

/// A timer the core asked the driver to arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(pub u64);

/// A job the driver runs off the core thread; its outcome returns as
/// [`Input::JobDone`](crate::Input::JobDone) under this id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// A host, by the id its identity key was minted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostId(pub [u8; 16]);

/// One link to a peer: the driver's connection, named by the core when the
/// driver reports it up. Two links to one peer have different ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LinkId(pub u64);

/// A channel the driver opened over a link at the core's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(pub u64);

/// An agent of this profile, by its directory's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub [u8; 16]);

/// One pairing conversation, from its first message to its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PairingId(pub u64);

/// An account this installation is signed in to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub [u8; 16]);
