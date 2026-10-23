#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn commands() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // These fixtures execute only immediate local scripts. The CLI contracts
    // can be tested on macOS hosts that have not installed GNU coreutils yet.
    executable(
        &dir.path().join("timeout"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    dir
}

fn ios_verify_fixture() -> tempfile::TempDir {
    let dir = commands();
    std::fs::create_dir_all(dir.path().join("journeys")).unwrap();
    std::fs::create_dir_all(dir.path().join("apps/apple")).unwrap();
    std::fs::write(
        dir.path().join("journeys/manifest.json"),
        include_str!("../../../journeys/manifest.json"),
    )
    .unwrap();
    // The two justfiles the runner validates its stages against, declaring
    // exactly the recipes verification runs.
    let root = [
        "fmt-check",
        "lint",
        "test",
        "mobile-check",
        "test-store-ios",
    ];
    let ios = [
        "lint",
        "script-tests",
        "graph-check",
        "rust",
        "simulator",
        "test-build",
        "loopback-smoke",
        "component-snapshots",
        "unit",
        "goldens",
        "goldens-perturb",
        "journey",
        "accessibility",
        "package",
        "scope-audit",
        "perf",
    ];
    let declare = |names: &[&str]| -> String {
        names
            .iter()
            .map(|name| format!("{name}:\n    true\n"))
            .collect()
    };
    std::fs::write(dir.path().join("justfile"), declare(&root)).unwrap();
    std::fs::write(dir.path().join("apps/apple/justfile"), declare(&ios)).unwrap();
    executable(
        &dir.path().join("just"),
        r#"#!/bin/sh
echo "$*" >> calls
if [ "$*" = "ios simulator golden" ]; then
    # Slower than the stage it runs beside, so the run must wait for it.
    sleep 1
    echo "simulator booted" >> calls
fi
[ "$*" != "$FAIL_RECIPE" ] || exit 1
if [ "$*" = "ios journey" ]; then
    for id in reach-host conversation-decision-claude-pty conversation-decision-claude-sdk conversation-decision-codex leave-and-recover decide-plan-claude-sdk decide-plan-claude-pty decide-plan-codex answer-questions tool-server-asks queue-and-steer send-while-away composer-limits manage-agent attachment-or-review keep-authority account-sign-in purchase-restore report accessibility local-network push-wake resume; do
        [ "$id" = "$SKIP_JOURNEY" ] || echo "$id: passed"
    done
fi
"#,
    );
    dir
}

fn ios_verify_command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.arg("ios-verify").current_dir(dir).env(
        "PATH",
        format!("{}:{}", dir.display(), std::env::var("PATH").unwrap()),
    );
    command
}

#[test]
fn ios_verify_cli_runs_full_checks_bare_and_stops_on_failure_or_skipped_journey() {
    let dir = ios_verify_fixture();
    for (fail, skip, success, last) in [
        ("", "", true, "ios perf"),
        ("fmt-check", "", false, "fmt-check"),
        ("mobile-check", "", false, "mobile-check"),
        ("ios simulator golden", "", false, "ios test-build"),
        ("ios accessibility", "", false, "ios accessibility"),
        ("", "reach-host", false, "ios journey"),
        ("", "leave-and-recover", false, "ios journey"),
        ("", "composer-limits", false, "ios journey"),
        ("", "keep-authority", false, "ios journey"),
    ] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let output = ios_verify_command(dir.path())
            .env("FAIL_RECIPE", fail)
            .env("SKIP_JOURNEY", skip)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The simulator boots beside the test build, so which of the two
        // reached the log first is a race; everything else is in order.
        let calls = std::fs::read_to_string(dir.path().join("calls"))
            .unwrap()
            .replace(
                "ios test-build\nios simulator golden\n",
                "ios simulator golden\nios test-build\n",
            );
        let calls = calls.trim_end_matches("simulator booted\n");
        assert!(calls.ends_with(&format!("{last}\n")), "{calls}");
        for forbidden in [
            "--record",
            "--update",
            "--only",
            "--negative-control",
            "--review",
        ] {
            assert!(
                !calls.contains(forbidden),
                "verification must not filter, update or record: {calls}"
            );
        }
        if success {
            assert_eq!(
                calls.replace("simulator booted\n", ""),
                "fmt-check\nlint\ntest\nmobile-check\nios lint\nios script-tests\nios graph-check\nios rust\nios simulator golden\nios test-build\nios component-snapshots --skip-build\nios loopback-smoke\nios unit --skip-build\nios goldens\nios goldens-perturb\ntest-store-ios\nios journey\nios accessibility\nios package\nios scope-audit\nios perf\n"
            );
            let booted = calls.find("simulator booted").unwrap();
            assert!(
                booted < calls.find("ios component-snapshots").unwrap(),
                "a stage that needs the simulator started before it booted: {calls}"
            );
        }
        if !skip.is_empty() {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains(&format!("{skip} did not report a full pass"))
            );
        }
    }
}
