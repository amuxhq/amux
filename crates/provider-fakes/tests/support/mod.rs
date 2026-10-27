// Each test binary compiles this module and uses only part of it.
#![allow(dead_code)]

//! A host for the stdio fakes, shared by their tests.

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use provider_fakes::shape::{Classifier, Corpus};
use provider_fakes::{Kind, Script};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

pub const DEADLINE: Duration = Duration::from_secs(20);

/// The recorded corpus a kind's composed frames are held to.
pub fn corpus(kind: Kind) -> &'static Corpus {
    static SDK: OnceLock<Corpus> = OnceLock::new();
    static CODEX: OnceLock<Corpus> = OnceLock::new();
    static PTY: OnceLock<Corpus> = OnceLock::new();
    let (cell, root) = match kind {
        Kind::ClaudeSdk => (&SDK, "../claude-specs/fixtures/sdk"),
        Kind::Codex => (&CODEX, "../codex-specs/fixtures/runtime"),
        Kind::ClaudePty => (&PTY, "../claude-specs/fixtures/pty"),
    };
    cell.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(root);
        Corpus::load(kind, &[&root]).unwrap()
    })
}

/// Write a script where a fake can read it.
pub fn script_file(dir: &Path, script: Value) -> std::path::PathBuf {
    // Parsed from text: a form schema keeps the text it was written as.
    let script: Script = serde_json::from_str(&script.to_string()).unwrap();
    let path = dir.join("script.json");
    std::fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
    path
}

/// A host speaking JSON lines to a fake over its stdio, checking every
/// frame the fake writes against the corpus.
pub struct Host {
    kind: Kind,
    /// Groups the corpus does not show yet, with the probe that does.
    exempt: &'static [&'static str],
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    classifier: Classifier,
    pub frames: Vec<Value>,
    /// Holds the script until the fake exits.
    _dir: tempfile::TempDir,
}

impl Host {
    pub async fn spawn(
        kind: Kind,
        binary: &str,
        args: &[&str],
        script: Value,
        exempt: &'static [&'static str],
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = script_file(dir.path(), script);
        let mut child = tokio::process::Command::new(binary)
            .args(args)
            .env(provider_fakes::SCRIPT_ENV, &path)
            // Keeps a fake Claude's transcript out of the person's own
            // `~/.claude`, where it would otherwise write it as Claude does.
            .env("CLAUDE_CONFIG_DIR", dir.path().join("claude"))
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            kind,
            exempt,
            child,
            stdin,
            stdout,
            classifier: Classifier::default(),
            frames: Vec::new(),
            _dir: dir,
        }
    }

    pub async fn send(&mut self, frame: Value) {
        self.classifier.host(self.kind, &frame);
        let mut line = serde_json::to_vec(&frame).unwrap();
        line.push(b'\n');
        self.stdin.as_mut().unwrap().write_all(&line).await.unwrap();
    }

    pub async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(DEADLINE, self.stdout.next_line())
            .await
            .expect("the fake wrote nothing in time")
            .unwrap()
            .expect("the fake closed stdout");
        let frame: Value = serde_json::from_str(&line).unwrap();
        let group = self.classifier.provider(self.kind, &frame);
        if let Err(drift) = corpus(self.kind).check(self.kind, &group, &frame)
            && !self.exempt.contains(&group.as_str())
        {
            panic!("{drift}");
        }
        self.frames.push(frame.clone());
        frame
    }

    /// Read until a frame matches, returning it.
    pub async fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let frame = self.next().await;
            if matches(&frame) {
                return frame;
            }
        }
    }

    /// A compact trace of the frames read so far, for order assertions.
    pub fn trace(&self, describe: fn(&Value) -> String) -> Vec<String> {
        self.frames.iter().map(describe).collect()
    }

    /// The fake's exit code, waited for while the host still holds its
    /// input open, as a live host does.
    pub async fn exited_within(mut self, within: Duration) -> i32 {
        let status = tokio::time::timeout(within, self.child.wait())
            .await
            .expect("the fake exited in time with its input still open")
            .unwrap();
        drop(self.stdin.take());
        status.code().unwrap_or(-1)
    }

    pub async fn close(mut self) -> i32 {
        drop(self.stdin.take());
        while let Ok(Ok(Some(line))) = tokio::time::timeout(DEADLINE, self.stdout.next_line()).await
        {
            let frame: Value = serde_json::from_str(&line).unwrap();
            self.frames.push(frame);
        }
        tokio::time::timeout(DEADLINE, self.child.wait())
            .await
            .unwrap()
            .unwrap()
            .code()
            .unwrap_or(-1)
    }
}

/// A fake serving `app-server --listen unix://…`, for its clients to share.
#[cfg(unix)]
pub struct Server {
    child: Child,
    pub socket: std::path::PathBuf,
    /// Holds the script and the socket until the fake exits.
    _dir: tempfile::TempDir,
}

#[cfg(unix)]
impl Server {
    pub fn start(binary: &str, script: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = script_file(dir.path(), script);
        let socket = dir.path().join("codex.sock");
        let child = tokio::process::Command::new(binary)
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", socket.display()))
            .env(provider_fakes::SCRIPT_ENV, &path)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            child,
            socket,
            _dir: dir,
        }
    }

    /// A new client, checking every frame it reads against the corpus.
    pub async fn connect(&self, kind: Kind, exempt: &'static [&'static str]) -> Client {
        let socket = provider_fakes::clients::connect(&self.socket, DEADLINE)
            .await
            .unwrap();
        Client {
            kind,
            exempt,
            socket,
            classifier: Classifier::default(),
            frames: Vec::new(),
        }
    }

    /// Whether the fake is still serving.
    pub fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

/// One client of a [`Server`], speaking WebSocket text frames.
#[cfg(unix)]
pub struct Client {
    kind: Kind,
    exempt: &'static [&'static str],
    socket: provider_fakes::clients::Connection,
    classifier: Classifier,
    pub frames: Vec<Value>,
}

#[cfg(unix)]
impl Client {
    pub async fn send(&mut self, frame: Value) {
        use futures_util::SinkExt;
        self.classifier.host(self.kind, &frame);
        self.socket
            .send(tokio_tungstenite::tungstenite::Message::text(
                frame.to_string(),
            ))
            .await
            .unwrap();
    }

    pub async fn next(&mut self) -> Value {
        self.next_within(DEADLINE)
            .await
            .expect("the fake wrote nothing in time")
    }

    /// The next frame, or None when nothing arrives within `wait`.
    pub async fn next_within(&mut self, wait: Duration) -> Option<Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let deadline = tokio::time::Instant::now() + wait;
        let text = loop {
            let message = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .ok()?
                .expect("the fake hung up")
                .unwrap();
            if let Message::Text(text) = message {
                break text;
            }
        };
        let frame: Value = serde_json::from_str(text.as_str()).unwrap();
        let group = self.classifier.provider(self.kind, &frame);
        if let Err(drift) = corpus(self.kind).check(self.kind, &group, &frame)
            && !self.exempt.contains(&group.as_str())
        {
            panic!("{drift}");
        }
        self.frames.push(frame.clone());
        Some(frame)
    }

    pub async fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let frame = self.next().await;
            if matches(&frame) {
                return frame;
            }
        }
    }

    /// Leaves as a client does, closing the connection.
    pub async fn leave(mut self) {
        let _ = self.socket.close(None).await;
    }
}
