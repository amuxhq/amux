//! Claude Code version probing.

use std::path::Path;
use std::time::Duration;

use semver::Version;
use tokio::process::Command;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The oldest Claude amux runs in a terminal: the first with every hook
/// the agent registers (PostModelSwitch, the newest of them, came in
/// 2.1.251). An older Claude starts without complaint and never reports
/// the events amux follows a session by, so it is refused at launch.
pub const MIN_TERMINAL: Version = Version::new(2, 1, 251);

/// The version of the Claude at `binary`, or why its `--version` could not
/// be read: the agent only reports that, and starts Claude anyway.
pub async fn probe_version(binary: &Path) -> Result<Version, String> {
    let mut command = Command::new(binary);
    command.arg("--version");
    probe_command(command).await
}

async fn probe_command(mut command: Command) -> Result<Version, String> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| "Claude version probe timed out".to_owned())?
        .map_err(|error| format!("could not run Claude version probe: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Claude version probe failed with {}",
            output.status
        ));
    }
    let raw = String::from_utf8(output.stdout)
        .map_err(|error| format!("Claude version output was not UTF-8: {error}"))?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Claude version output was empty".to_owned());
    }
    parse(raw).map_err(|error| format!("invalid Claude version `{raw}`: {error}"))
}

/// The version at the head of Claude's `--version` banner.
fn parse(banner: &str) -> Result<Version, semver::Error> {
    banner.split_whitespace().next().unwrap_or(banner).parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semantic_version_from_cli_banner() {
        assert_eq!(
            parse("2.1.251 (Claude Code)").unwrap(),
            Version::new(2, 1, 251)
        );
        assert!(parse("not a version").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_reports_process_output_and_failures() {
        for (script, expected) in [
            ("printf '2.1.251 (Claude Code)\\n'", "2.1.251"),
            ("printf '2.1.251'; exit 17", "status"),
            ("printf ''", "empty"),
            ("printf 'invalid'", "invalid"),
            ("printf '\\377'", "utf8"),
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let output = probe_command(command).await;
            match expected {
                "status" => assert!(output.unwrap_err().contains("17")),
                "empty" => assert!(output.unwrap_err().contains("empty")),
                "invalid" => assert!(output.unwrap_err().contains("invalid")),
                "utf8" => assert!(output.unwrap_err().contains("UTF-8")),
                version => assert_eq!(output.unwrap().to_string(), version),
            }
        }
    }
}
