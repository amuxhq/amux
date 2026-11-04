//! The analytics notice, which a person sees once before amux sends
//! anything. The first command that would have something sent prints it to
//! standard error and records it as shown, so the installation's events,
//! held until then, start going.

use std::path::Path;
use std::time::Duration;

use settings::InstallationConfig;

/// How long the command waits on a running daemon to take the notice as
/// shown before giving up: the next command prints it again.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// Prints the notice when this installation would send usage counts and no
/// client has shown it yet, and records it as shown. Never stops the
/// command: a config that does not read is the command's to report.
pub fn once(config_path: Option<&Path>) {
    let path = config_path
        .map(Path::to_owned)
        .unwrap_or_else(InstallationConfig::default_path);
    let config = match InstallationConfig::from_file(&path) {
        Ok(config) => config,
        Err(settings::ConfigError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            InstallationConfig::default()
        }
        Err(_) => return,
    };
    if !config.telemetry
        || analytics::Endpoint::resolve(node::release::is_published()).is_none()
        || node::notice_shown_in(&config.root)
    {
        return;
    }
    eprintln!("{} {}", analytics::NOTICE, analytics::NOTICE_URL);
    if let Err(error) = mark(&config) {
        tracing::debug!(%error, "recording the analytics notice as shown");
    }
}

/// A running daemon keeps the onboarding state and is told; with none
/// running the file is written.
fn mark(config: &InstallationConfig) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let Some(door) = crate::connect::front_door_now(config).await else {
            return Ok(node::mark_notice_shown_in(&config.root)?);
        };
        let answer = wire::AnswerOnboardingRequest {
            profile_id: Vec::new(),
            answer: Some(wire::answer_onboarding_request::Answer::NoticeShown(
                wire::Empty {},
            )),
            surface: wire::Surface::Terminal as i32,
        };
        tokio::time::timeout(
            ANSWER_WITHIN,
            crate::connect::installation(door).answer_onboarding(answer),
        )
        .await??;
        Ok(())
    })
}
