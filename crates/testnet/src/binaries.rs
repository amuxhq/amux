//! The amux binary and the fake providers the net's agents run.

use std::path::{Path, PathBuf};

use crate::topology::FakeKind;

/// Names a directory holding `amux`, the fakes and the probes, built
/// already.
pub const PREBUILT_ENV: &str = provider_fakes::binaries::ENV;

/// Where the built `amux` and fake provider binaries are. Nothing here
/// builds them: in a test the nextest setup script did, and a driver's
/// recipe built them beside it ([`provider_fakes::binaries::dir`]). Agents
/// run the binary from the tree under test, never an installed one, and a
/// release run measures release agents.
#[derive(Clone, Debug)]
pub struct Binaries {
    dir: PathBuf,
}

impl Binaries {
    /// The binaries of this process: the directory [`PREBUILT_ENV`] names,
    /// else the one a driver runs from.
    pub fn built() -> Binaries {
        Binaries {
            dir: provider_fakes::binaries::dir().to_owned(),
        }
    }

    /// The binaries a journey driver hands down to the net it serves,
    /// checked before it starts one: a binary missing is an error at once,
    /// rather than an agent that fails to launch.
    pub fn for_driver() -> Result<Binaries, String> {
        let binaries = Binaries::built();
        let missing: Vec<String> = std::iter::once(binaries.amux())
            .chain(
                [FakeKind::ClaudePty, FakeKind::ClaudeSdk, FakeKind::Codex]
                    .map(|kind| binaries.fake(kind)),
            )
            .filter(|binary| !binary.is_file())
            .map(|binary| binary.display().to_string())
            .collect();
        if missing.is_empty() {
            Ok(binaries)
        } else {
            Err(format!(
                "{} not built: build amux and provider-fakes in the cargo invocation that builds testnet",
                missing.join(", ")
            ))
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn amux(&self) -> PathBuf {
        self.binary("amux")
    }

    pub fn fake(&self, kind: FakeKind) -> PathBuf {
        self.binary(kind.binary())
    }

    fn binary(&self, name: &str) -> PathBuf {
        self.dir
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    }
}
