//! The profile's trust file as the driver reads it at start, and the
//! trust store the handshakes read. The core owns the store and is its
//! only writer: every change is a core step written by a job, and the
//! driver publishes each store the core serves into the shared copy here.

use std::fs;
use std::path::{Path, PathBuf};

pub use daemon_core::trust::{
    Reachability, SshTarget, TrustChange, TrustEntry, TrustError, TrustStore,
};

use crate::identity::{
    ED25519_PUBKEY_LEN, IdentityError, create_private_file, ensure_private_file_mode,
};

const TRUST_FILE: &str = "trust.json";

/// The trust store as the core last published it, for the handshakes and
/// the shell's reads. Only the driver writes it.
pub type SharedTrustStore = std::sync::Arc<std::sync::RwLock<TrustStore>>;

/// The profile's trust file.
pub(crate) fn trust_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TRUST_FILE)
}

/// The trust file as it is, created empty when there is none.
#[doc(hidden)]
pub fn load_or_create_in(data_dir: &Path) -> Result<TrustStore, IdentityError> {
    let path = trust_path(data_dir);
    match load_trust_store_from_path(&path) {
        Ok(store) => Ok(store),
        Err(IdentityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let store = TrustStore::default();
            let bytes = store.to_file().map_err(trust_error)?;
            match create_private_file(&path, &bytes) {
                Ok(()) => Ok(store),
                Err(IdentityError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    load_trust_store_from_path(&path)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn load_trust_store_from_path(path: &Path) -> Result<TrustStore, IdentityError> {
    ensure_private_file_mode(path)?;
    let bytes = fs::read(path)?;
    TrustStore::from_file(&bytes).map_err(|error| match error {
        TrustError::Corrupt(reason) => IdentityError::TrustFile {
            path: path.to_path_buf(),
            reason,
        },
        other => trust_error(other),
    })
}

fn trust_error(error: TrustError) -> IdentityError {
    match error {
        TrustError::BadPubkey { actual } => IdentityError::InvalidLength {
            field: "TrustEntry.pubkey",
            expected: ED25519_PUBKEY_LEN,
            actual,
        },
        other => IdentityError::Trust(other),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn trust_store_is_created_as_empty_json_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();

        let store = load_or_create_in(dir.path()).unwrap();
        assert!(store.is_empty());
        assert!(trust_path(dir.path()).exists());

        let loaded = load_or_create_in(dir.path()).unwrap();
        assert!(loaded.is_empty());

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(trust_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
