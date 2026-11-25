//! The operating-system primitives several amux crates share.
//!
//! Each module is one API with an implementation per OS in submodules, so
//! callers never write `cfg`. A primitive an OS doesn't support returns
//! [`std::io::ErrorKind::Unsupported`], never a fixed success, unless its
//! doc comment says otherwise. docs/ARCHITECTURE.md's crate map says who
//! may depend on this crate.

pub mod fs;
pub mod lock;
pub mod paths;
pub mod process;
#[cfg(feature = "async")]
pub mod signal;

/// A process id, as every OS amux runs on numbers them.
pub type Pid = u32;
