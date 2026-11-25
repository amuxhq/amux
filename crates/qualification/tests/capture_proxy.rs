//! The probe standing in for `claude` on the PATH, as the live lane puts it,
//! reaches the real Claude through a wrapper earlier on the PATH that runs
//! the next `claude` it finds. Were the probe still on the PATH the real
//! provider inherits, the wrapper would find the probe again, and so on
//! without end; the wrapper here gives up after a few rounds instead.

use std::path::Path;
use std::process::{Command, Stdio};

use provider_fakes::dispatch::{self, Plan};

#[test]
fn a_wrapper_ahead_of_the_probe_reaches_the_real_claude() {
    let root = tempfile::tempdir().unwrap();
    let [shim, wrapper, real, capture] =
        ["shim", "wrapper", "real", "capture"].map(|name| root.path().join(name));
    for folder in [&shim, &wrapper, &real, &capture] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let stand_in = dispatch::program(provider_fakes::binaries::dir(), dispatch::BINARY);
    dispatch::link(
        Path::new(source_tree::bin!("claude-probe")),
        &dispatch::program(&shim, "claude"),
    )
    .unwrap();
    // Runs the next `claude` on the PATH past its own folder, as a usage
    // meter or version manager does.
    dispatch::install(
        &stand_in,
        &dispatch::program(&wrapper, "claude"),
        &Plan::Next {
            name: "claude".to_owned(),
            rounds_env: "WRAPPER_ROUNDS".to_owned(),
            max_rounds: 3,
        },
    )
    .unwrap();
    dispatch::install(
        &stand_in,
        &dispatch::program(&real, "claude"),
        &Plan::Answer {
            lines: 0,
            stdout: "reached\n".to_owned(),
            stderr: String::new(),
            code: 0,
        },
    )
    .unwrap();
    let path = std::env::join_paths([&shim, &wrapper, &real]).unwrap();

    let output = Command::new(dispatch::program(&shim, "claude"))
        .arg("--version")
        .env("PATH", path)
        .env("CLAUDE_CAPTURE_PROXY", "1")
        .env("CLAUDE_CAPTURE_DIR", &capture)
        .env("CLAUDE_REAL_PATH", dispatch::program(&wrapper, "claude"))
        .env_remove("WRAPPER_ROUNDS")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "reached",
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
