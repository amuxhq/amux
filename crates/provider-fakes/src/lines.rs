//! Newline-delimited JSON over stdio, the transport of the headless Claude
//! and Codex fakes.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::playback::{Channel, Process};

/// A JSON-lines writer that flushes every line, so the host sees each one
/// when the fake means it to.
pub struct Out<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> Out<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn raw(&mut self, line: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(line).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await
    }

    /// Keys come out sorted: serde_json's map order, the order the corpora
    /// are stored in.
    pub async fn send(&mut self, value: &Value) -> std::io::Result<()> {
        self.raw(&serde_json::to_vec(value).expect("JSON values serialise"))
            .await
    }

    /// Sends a frame carrying a form schema in its [`SCHEMA_SLOT`], the
    /// schema written out as the script has it, its keys in their order.
    ///
    /// [`SCHEMA_SLOT`]: crate::script::SCHEMA_SLOT
    pub async fn send_with_schema(
        &mut self,
        value: &Value,
        schema: &crate::script::Schema,
    ) -> std::io::Result<()> {
        self.raw(with_schema(value, schema).as_bytes()).await
    }
}

/// A frame's text with a form schema in its [`SCHEMA_SLOT`], written out
/// as the script has it, its keys in their order.
///
/// [`SCHEMA_SLOT`]: crate::script::SCHEMA_SLOT
pub fn with_schema(value: &Value, schema: &crate::script::Schema) -> String {
    let line = serde_json::to_string(value).expect("JSON values serialise");
    let slot = format!("\"{}\"", crate::script::SCHEMA_SLOT);
    line.replacen(&slot, &schema.compact(), 1)
}

/// Play one recorded stdio process: write each recorded output line, and
/// require each recorded input line from the host, byte for byte. After the
/// last event the host must close stdin without writing more; a recorded
/// exit instead ends the process there with its code, returned.
pub async fn play<R, W>(process: &Process, input: R, output: W) -> Result<Option<i32>, String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut out = Out::new(output);
    let mut input = input.lines();
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Output => out
                .raw(&event.bytes)
                .await
                .map_err(|error| format!("writing event {index}: {error}"))?,
            Channel::Input => {
                let line = input
                    .next_line()
                    .await
                    .map_err(|error| format!("reading event {index}: {error}"))?
                    .ok_or_else(|| format!("input closed before event {index}"))?;
                if line.as_bytes() != event.bytes {
                    return Err(format!(
                        "event {index}: host wrote\n  {line}\nrecording has\n  {}",
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            Channel::Exit => return Ok(Some(crate::playback::exit_code(event))),
            Channel::Transcript | Channel::Hook | Channel::Subagent => {
                return Err(format!(
                    "event {index}: a stdio process has no {:?}",
                    event.channel
                ));
            }
        }
    }
    match input.next_line().await {
        Ok(None) => Ok(None),
        Ok(Some(line)) => Err(format!("host wrote past the recording's end: {line}")),
        Err(error) => Err(format!("reading past the end: {error}")),
    }
}
