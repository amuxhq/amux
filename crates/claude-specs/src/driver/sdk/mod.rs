//! A stream-JSON client for Claude Code, shaped after the published SDK.

pub mod abort;
pub(crate) mod dispatch;
mod error;
pub mod options;
mod process;
pub(crate) mod query;
pub mod session;

pub use abort::AbortHandle;
pub use claude_protocol::stream::*;
pub use error::Error;
pub use options::QueryOptions;
pub use query::{ProcessExit, Termination, UserMessage};
pub use session::{
    Control, EventStream, PermissionSuggestion, RequestId, SdkEvent, Session, from_io,
};

/// Spawn Claude Code and return an initialized SDK session.
pub async fn spawn(options: QueryOptions) -> Result<Session, Error> {
    let (options, command) = prepare_query(options)?;
    let process = process::spawn_command(command)?;
    let warm =
        query::Query::warm_from_process(options, process, std::time::Duration::from_secs(60))
            .await?;
    Ok(session::from_query(warm.into_query()))
}

fn prepare_query(
    mut options: QueryOptions,
) -> Result<(QueryOptions, tokio::process::Command), Error> {
    options.validate()?;
    let session_id = query::query_session_id(&options);
    let command = process::query_command(&session_id, &options)?;
    // Forks without a requested target must use the same generated identity
    // in the CLI arguments and every subsequent streamed prompt.
    options.session_id = Some(session_id);
    Ok((options, command))
}
