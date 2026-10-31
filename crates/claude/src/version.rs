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

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("could not run Claude version probe: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude version probe timed out")]
    Timeout,
    #[error("Claude version probe failed with {0}")]
    Status(std::process::ExitStatus),
    #[error("Claude version output was not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("Claude version output was empty")]
    Empty,
    #[error("invalid Claude version `{value}`: {source}")]
    Invalid {
        value: String,
        source: semver::Error,
    },
}

pub async fn probe_version(binary: &Path) -> Result<Version, VersionError> {
    let mut command = Command::new(binary);
    command.arg("--version");
    probe_command(command).await
}

async fn probe_command(mut command: Command) -> Result<Version, VersionError> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| VersionError::Timeout)??;
    if !output.status.success() {
        return Err(VersionError::Status(output.status));
    }
    let raw = String::from_utf8(output.stdout)?.trim().to_string();
    if raw.is_empty() {
        return Err(VersionError::Empty);
    }
    parse(&raw).map_err(|source| VersionError::Invalid { value: raw, source })
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
                "status" => assert!(
                    matches!(output, Err(VersionError::Status(status)) if status.code() == Some(17))
                ),
                "empty" => assert!(matches!(output, Err(VersionError::Empty))),
                "invalid" => assert!(matches!(output, Err(VersionError::Invalid { .. }))),
                "utf8" => assert!(matches!(output, Err(VersionError::Utf8(_)))),
                version => assert_eq!(output.unwrap().to_string(), version),
            }
        }
    }
}
