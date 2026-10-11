//! One request to a served net's door and its one reply, over a connection
//! of its own.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

/// Sends `request` and answers the reply's value, or its error in words.
pub(crate) fn call(
    control: SocketAddr,
    request: &Value,
    patience: Duration,
) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(&control, Duration::from_secs(5)).map_err(|error| {
        format!("the net's door at {control} does not answer ({error}); is the net up? (ctl status, ctl up)")
    })?;
    stream
        .set_read_timeout(Some(patience))
        .map_err(|error| error.to_string())?;
    let mut line = request.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|error| format!("writing to the door: {error}"))?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => format!(
                "no answer within {}s; the net may still be working on it",
                patience.as_secs()
            ),
            _ => format!("reading the door's reply: {error}"),
        })?;
    let reply: Value = serde_json::from_str(reply.trim())
        .map_err(|error| format!("the door's reply is not JSON ({error}): {reply}"))?;
    if let Some(value) = reply.get("ok") {
        return Ok(value.clone());
    }
    let error = &reply["error"];
    Err(format!(
        "{} ({})",
        error["message"].as_str().unwrap_or("the door refused"),
        error["kind"].as_str().unwrap_or("error")
    ))
}
