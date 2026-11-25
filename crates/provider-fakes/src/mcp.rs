//! The tool servers a launch configures, called as a provider calls them.
//!
//! A scripted call named `mcp__<server>__<tool>` for a server the launch
//! configured goes to that server: the fake starts the server's command the
//! first time, does the MCP handshake over its stdio and sends `tools/call`,
//! and the call's outcome is the server's answer rather than the script's.
//! Claude names its servers in `--mcp-config` (inline JSON or a file);
//! Codex in `--config mcp_servers.<name>.command=…` and `….args=…`
//! overrides. A call for any other server keeps its scripted outcome.

use std::collections::BTreeMap;
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

#[derive(Clone, Debug, Default, PartialEq)]
struct Launch {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

struct Running {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

/// The configured servers, each started on its first call and kept for the
/// session, as a provider keeps them.
#[derive(Default)]
pub struct ToolServers {
    configured: BTreeMap<String, Launch>,
    running: BTreeMap<String, Running>,
}

impl ToolServers {
    /// From Claude's `--mcp-config` values, in order; a later one names a
    /// server again over an earlier.
    pub fn from_claude(configs: &[Value]) -> Self {
        let mut configured = BTreeMap::new();
        for config in configs {
            let Some(servers) = config.get("mcpServers").and_then(Value::as_object) else {
                continue;
            };
            for (name, server) in servers {
                let Some(command) = server.get("command").and_then(Value::as_str) else {
                    continue;
                };
                configured.insert(
                    name.clone(),
                    Launch {
                        command: command.to_owned(),
                        args: strings(server.get("args")),
                        env: server
                            .get("env")
                            .and_then(Value::as_object)
                            .map(|env| {
                                env.iter()
                                    .filter_map(|(key, value)| {
                                        Some((key.clone(), value.as_str()?.to_owned()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                );
            }
        }
        Self {
            configured,
            running: BTreeMap::new(),
        }
    }

    /// From Codex's `--config key=value` (or `-c`) overrides. Values are
    /// TOML; the string and string-array values a host passes read as JSON.
    pub fn from_codex(args: &[String]) -> Self {
        let mut configured: BTreeMap<String, Launch> = BTreeMap::new();
        let mut index = 0;
        while index < args.len() {
            let pair = match args[index].as_str() {
                "--config" | "-c" => {
                    index += 1;
                    args.get(index).cloned()
                }
                arg => arg.strip_prefix("--config=").map(str::to_owned),
            };
            index += 1;
            let Some((key, value)) = pair.as_deref().and_then(|pair| pair.split_once('=')) else {
                continue;
            };
            let Some(rest) = key.trim().strip_prefix("mcp_servers.") else {
                continue;
            };
            let Some((name, field)) = rest.rsplit_once('.') else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(value.trim()) else {
                continue;
            };
            let launch = configured.entry(name.to_owned()).or_default();
            match field {
                "command" => launch.command = value.as_str().unwrap_or_default().to_owned(),
                "args" => launch.args = strings(Some(&value)),
                _ => {}
            }
        }
        configured.retain(|_, launch| !launch.command.is_empty());
        Self {
            configured,
            running: BTreeMap::new(),
        }
    }

    /// Claude's name for a server's tool, split: `mcp__<server>__<tool>`.
    pub fn split(name: &str) -> Option<(&str, &str)> {
        name.strip_prefix("mcp__")?.split_once("__")
    }

    pub fn configures(&self, server: &str) -> bool {
        self.configured.contains_key(server)
    }

    /// The server's answer to one call: its text, or the text of an error
    /// it returned or failed with. None when the launch did not configure
    /// the server.
    pub async fn call(
        &mut self,
        server: &str,
        tool: &str,
        arguments: &Value,
    ) -> Option<Result<String, String>> {
        let launch = self.configured.get(server)?.clone();
        if !self.running.contains_key(server) {
            match start(&launch).await {
                Ok(running) => {
                    self.running.insert(server.to_owned(), running);
                }
                Err(error) => return Some(Err(format!("{server}: {error}"))),
            }
        }
        let running = self.running.get_mut(server)?;
        let answer = running
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        Some(match answer {
            Ok(result) => {
                let text = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if result.get("isError") == Some(&Value::Bool(true)) {
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            Err(error) => Err(error),
        })
    }
}

async fn start(launch: &Launch) -> Result<Running, String> {
    let mut child = tokio::process::Command::new(&launch.command)
        .args(&launch.args)
        .envs(&launch.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // A terminal fake's stderr is the terminal.
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("starting {}: {error}", launch.command))?;
    let mut running = Running {
        stdin: child.stdin.take().expect("stdin is piped"),
        stdout: BufReader::new(child.stdout.take().expect("stdout is piped")).lines(),
        _child: child,
        next_id: 0,
    };
    running
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "provider-fake", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
    running
        .write(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await?;
    Ok(running)
}

impl Running {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "the tool server closed its output".to_owned())?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the tool server failed")
                    .to_owned());
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn write(&mut self, message: Value) -> Result<(), String> {
        let mut bytes = message.to_string().into_bytes();
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        self.stdin.flush().await.map_err(|error| error.to_string())
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_overrides_name_a_server() {
        let servers = ToolServers::from_codex(&[
            "--config".into(),
            r#"mcp_servers.amux.command="/bin/amux""#.into(),
            "--config".into(),
            r#"mcp_servers.amux.args=["mcp", "/a/b"]"#.into(),
            "app-server".into(),
        ]);
        assert_eq!(
            servers.configured["amux"],
            Launch {
                command: "/bin/amux".into(),
                args: vec!["mcp".into(), "/a/b".into()],
                env: BTreeMap::new(),
            }
        );
    }

    #[test]
    fn claude_names_split_into_server_and_tool() {
        assert_eq!(
            ToolServers::split("mcp__amux__message_agent"),
            Some(("amux", "message_agent"))
        );
        assert_eq!(ToolServers::split("Bash"), None);
        let servers = ToolServers::from_claude(&[json!({
            "mcpServers": { "amux": { "command": "/bin/amux", "args": ["mcp", "/d"] } }
        })]);
        assert!(servers.configures("amux"));
    }
}
