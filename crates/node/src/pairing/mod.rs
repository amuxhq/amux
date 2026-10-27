//! Pairing as a person runs it: the SSH identity exchange and the QR
//! code's payload. The pairing window and the secret exchange are the
//! profile's core's (`daemon_core::pairing`).

pub(crate) mod qr;
pub(crate) mod ssh;

use std::time::Duration;

/// How long a pairing window stays open unless asked otherwise.
pub(crate) const PAIR_MODE_TTL: Duration = Duration::from_secs(5 * 60);
