//! amux in a terminal the test holds, answering the queries a real
//! terminal answers.

use std::path::Path;
use std::time::Duration;

use pty_host::{PtyProcess, PtySize, PtySpawn};

use super::PATIENCE;
use super::desk::Desk;

/// amux running in a terminal the test holds: everything
/// it wrote, and the screen a terminal would show.
pub struct Term {
    process: PtyProcess,
    output: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    written: Vec<u8>,
    screen: vt100::Parser,
}

impl Term {
    /// `amux attach <agent> --in native` from the desk's own binary: the
    /// agent's own interface, whatever the desk's config prefers.
    pub fn attach(desk: &Desk, agent: &str, rows: u16, cols: u16) -> Term {
        Term::run(
            desk,
            &desk.amux_path(),
            &["attach", agent, "--in", "native"],
            rows,
            cols,
        )
    }

    /// `binary args…` against the desk's install, in a terminal of its own.
    pub fn run(desk: &Desk, binary: &Path, args: &[&str], rows: u16, cols: u16) -> Term {
        let path = format!(
            "{}:{}",
            desk.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let process = pty_host::spawn(PtySpawn {
            command: binary.to_owned(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            cwd: desk.work.clone(),
            env: vec![
                ("AMUX_CONFIG".into(), desk.config.clone().into()),
                ("PATH".into(), path.into()),
                ("TERM".into(), "xterm-256color".into()),
                (super::NO_DISCOVERY.0.into(), super::NO_DISCOVERY.1.into()),
            ],
            env_remove: vec!["AMUX_LOG".into()],
            size: PtySize { rows, cols },
        })
        .expect("amux starts in a terminal");
        let output = process.handle.output();
        Term {
            process,
            output,
            written: Vec::new(),
            screen: vt100::Parser::new(rows, cols, 0),
        }
    }

    /// Answers the queries a terminal answers: the cursor position, which
    /// the fleet's terminal library asks when it starts, and the device
    /// attributes terminal Claude asks when it starts. Answers are typed
    /// into amux, as a terminal types them.
    async fn answer(&self, bytes: &[u8]) {
        if bytes.windows(4).any(|window| window == b"\x1b[6n") {
            let (row, col) = self.screen.screen().cursor_position();
            self.type_keys(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes())
                .await;
        }
        if bytes.windows(3).any(|window| window == b"\x1b[c") {
            self.type_keys(b"\x1b[?1;2c").await;
        }
    }

    pub fn said(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }

    pub fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    /// Reads until `done` holds of what the terminal shows.
    pub async fn until(&mut self, what: &str, done: impl Fn(&Term) -> bool) {
        self.until_within(what, PATIENCE, done).await;
    }

    /// Reads until `done` holds, failing once `patience` has passed.
    pub async fn until_within(
        &mut self,
        what: &str,
        patience: Duration,
        done: impl Fn(&Term) -> bool,
    ) {
        let waited = tokio::time::timeout(patience, async {
            while !done(self) {
                match self.output.recv().await {
                    Some(bytes) => {
                        self.written.extend_from_slice(&bytes);
                        self.screen.process(&bytes);
                        self.answer(&bytes).await;
                    }
                    None => return false,
                }
            }
            true
        })
        .await;
        assert!(
            waited == Ok(true),
            "waiting for {what}; the terminal shows:\n{}\nand was written:\n{}",
            self.contents(),
            self.said()
        );
    }

    /// Reads until the terminal has been written `text`.
    pub async fn drawn(&mut self, text: &str) {
        self.until(text, |term| term.said().contains(text)).await;
    }

    /// Reads until the screen shows `text` now.
    pub async fn shows(&mut self, text: &str) {
        self.until(text, |term| term.contents().contains(text))
            .await;
    }

    pub async fn type_keys(&self, keys: &[u8]) {
        self.process
            .handle
            .write(keys)
            .await
            .expect("typing reaches amux");
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.screen.set_size(rows, cols);
        self.process
            .handle
            .resize(PtySize { rows, cols })
            .expect("the terminal resizes");
    }

    /// Waits for amux to exit, reading what it says on the way out.
    pub async fn exits(mut self) -> String {
        let status = tokio::time::timeout(PATIENCE, async {
            loop {
                tokio::select! {
                    Some(bytes) = self.output.recv() => self.written.extend_from_slice(&bytes),
                    status = self.process.exit.wait() => break status,
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("amux did not exit; it said:\n{}", self.said()));
        while let Ok(Some(bytes)) =
            tokio::time::timeout(Duration::from_millis(200), self.output.recv()).await
        {
            self.written.extend_from_slice(&bytes);
        }
        assert!(status.success(), "amux failed:\n{}", self.said());
        self.said()
    }
}

/// Ends what runs in the terminal with the test, by return or by panic.
/// The process group is killed and the terminal read to its end on a
/// thread of its own: a program cannot finish exiting while its terminal
/// holds output nobody reads, and the test's runtime waits for the
/// terminal's reader before the test can end.
impl Drop for Term {
    fn drop(&mut self) {
        let _ = self
            .process
            .handle
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
        let (_, closed) = tokio::sync::mpsc::channel(1);
        let mut output = std::mem::replace(&mut self.output, closed);
        std::thread::spawn(move || while output.blocking_recv().is_some() {});
    }
}
