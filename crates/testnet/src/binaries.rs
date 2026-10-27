//! The amux binary and the fake providers the net's agents run.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::topology::FakeKind;

/// Names a directory holding `amux`, the fakes and the probes, built
/// already; a net then builds nothing.
pub const PREBUILT_ENV: &str = "TESTNET_BINARIES";

/// Where the built `amux` and fake provider binaries are: the target
/// directory this process runs from.
#[derive(Clone, Debug)]
pub struct Binaries {
    dir: PathBuf,
}

impl Binaries {
    /// Builds `amux` and the fake providers once per process with the same
    /// cargo and profile that built this one, and finds them beside it.
    /// Agents run the binary from the tree under test, never an installed
    /// one, and a release run measures release agents.
    ///
    /// With [`PREBUILT_ENV`] naming a directory, takes the binaries from
    /// there and builds nothing: a driver that built them once, with the
    /// features it chose, keeps every net it starts on that one build.
    pub fn built() -> &'static Binaries {
        static BUILT: OnceLock<Binaries> = OnceLock::new();
        BUILT.get_or_init(|| {
            if let Some(dir) = std::env::var_os(PREBUILT_ENV) {
                return Binaries { dir: dir.into() };
            }
            let dir = target_dir();
            let mut args = vec![
                "build",
                "--locked",
                "-p",
                "amux",
                "-p",
                "provider-fakes",
                "--bins",
            ];
            if dir.file_name().and_then(|name| name.to_str()) == Some("release") {
                args.push("--release");
            }
            let status = provider_fakes::cargo::command()
                .args(args)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .stdout(std::process::Stdio::null())
                .status()
                .expect("cargo runs");
            assert!(
                status.success(),
                "building amux and the fake providers failed"
            );
            Binaries { dir }
        })
    }

    /// The binaries a journey driver hands down to the net it serves,
    /// found before it starts one: in the directory [`PREBUILT_ENV`] names,
    /// else beside the driver, where its recipe built `amux` and the fakes
    /// in the same cargo invocation as the driver, so their features match.
    /// A served net left to find them itself would build them alone,
    /// unifying features otherwise and rebuilding much of the workspace
    /// before it is ready; a binary missing here is an error at once.
    pub fn for_driver() -> Result<Binaries, String> {
        let dir = match std::env::var_os(PREBUILT_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => std::env::current_exe()
                .map_err(|error| format!("this driver's path: {error}"))?
                .parent()
                .ok_or("this driver has no directory")?
                .to_owned(),
        };
        let binaries = Binaries { dir };
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

/// `target/<profile>`, from this executable: a test binary lives one level
/// further down, in `deps`.
fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("this executable's path");
    let dir = exe.parent().expect("a target directory");
    match dir.file_name().and_then(|name| name.to_str()) {
        Some("deps") => dir.parent().expect("a target directory").to_owned(),
        _ => dir.to_owned(),
    }
}
