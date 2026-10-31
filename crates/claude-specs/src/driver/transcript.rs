//! Following the transcript file terminal Claude writes.
//!
//! The rows themselves are [`claude_protocol::transcript::Row`].

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use claude_protocol::stream::RawFrame;
use claude_protocol::transcript::Row;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::mpsc;

/// A tailer owns one transcript's row stream; the driver follows a
/// replacement transcript with a tailer of its own.
pub struct TranscriptTailer {
    rows: Mutex<Option<mpsc::Receiver<Row>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TranscriptTailer {
    pub fn follow(path: PathBuf) -> Self {
        let (row_tx, row_rx) = mpsc::channel(256);
        let task = tokio::spawn(async move {
            tail(&path, &row_tx).await;
        });
        Self {
            rows: Mutex::new(Some(row_rx)),
            task,
        }
    }

    pub fn rows(&self) -> mpsc::Receiver<Row> {
        self.rows
            .lock()
            .expect("transcript rows mutex poisoned")
            .take()
            .expect("transcript row stream already taken")
    }
}

impl Drop for TranscriptTailer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How long the tail waits for the file to appear or grow.
const POLL: Duration = Duration::from_millis(100);

async fn tail(path: &PathBuf, rows: &mpsc::Sender<Row>) {
    let file = loop {
        match tokio::fs::File::open(path).await {
            Ok(file) => break file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::time::sleep(POLL).await;
            }
            Err(_) => return,
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut eof_observed = false;
    loop {
        line.clear();
        let Ok(line_start) = reader.stream_position().await else {
            return;
        };
        match reader.read_line(&mut line).await {
            Ok(0) => {
                if !eof_observed {
                    eof_observed = true;
                    tokio::time::sleep(POLL).await;
                    continue;
                }
                // Not Claude's: tells the reader the file has been read to
                // its end once.
                let ready = Row::Unknown(RawFrame::new(
                    serde_json::json!({"type":"amux.transcript_ready"}),
                ));
                if rows.send(ready).await.is_err() {
                    return;
                }
                break;
            }
            Ok(_) if !line.ends_with('\n') => {
                if reader
                    .seek(std::io::SeekFrom::Start(line_start))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
            Ok(_) => {
                eof_observed = false;
                if send_line(&line, rows).await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
    loop {
        line.clear();
        let Ok(line_start) = reader.stream_position().await else {
            return;
        };
        match reader.read_line(&mut line).await {
            Ok(0) => {
                tokio::time::sleep(POLL).await;
                let Ok(position) = reader.stream_position().await else {
                    return;
                };
                let Ok(metadata) = tokio::fs::metadata(path).await else {
                    continue;
                };
                if metadata.len() < position
                    && reader.seek(std::io::SeekFrom::Start(0)).await.is_err()
                {
                    return;
                }
            }
            Ok(_) if !line.ends_with('\n') => {
                if reader
                    .seek(std::io::SeekFrom::Start(line_start))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
            Ok(_) => {
                if send_line(&line, rows).await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Sends the line's row, if it is one. Err when the reader has gone.
async fn send_line(line: &str, rows: &mpsc::Sender<Row>) -> Result<(), ()> {
    if let Ok(row) = claude_protocol::transcript::decode(line.trim().as_bytes()) {
        rows.send(row).await.map_err(drop)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row as JSON; these rows are too sparse to decode as Claude's.
    fn json(row: Row) -> serde_json::Value {
        serde_json::to_value(row).unwrap()
    }

    #[tokio::test]
    async fn tailer_reads_existing_rows_then_says_it_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        tokio::fs::write(&path, "{\"type\":\"user\",\"uuid\":\"one\"}\n")
            .await
            .unwrap();
        let tailer = TranscriptTailer::follow(path);
        let mut rows = tailer.rows();
        assert_eq!(json(rows.recv().await.unwrap())["uuid"], "one");
        assert_eq!(
            json(rows.recv().await.unwrap())["type"],
            "amux.transcript_ready"
        );
    }

    #[tokio::test]
    async fn tailer_waits_for_a_transcript_created_after_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("later.jsonl");
        let tailer = TranscriptTailer::follow(path.clone());
        let mut rows = tailer.rows();
        drop(tokio::fs::File::create(&path).await.unwrap());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rows.recv())
                .await
                .is_err(),
            "a newly created transcript is not ready before its initial write"
        );
        tokio::fs::write(&path, "{\"type\":\"system\",\"subtype\":\"ready\"}\n")
            .await
            .unwrap();
        let row = tokio::time::timeout(Duration::from_secs(2), rows.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(json(row)["subtype"], "ready");
    }

    #[tokio::test]
    async fn tailer_rewinds_an_incomplete_jsonl_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fragmented.jsonl");
        tokio::fs::write(&path, "{\"type\":\"system\",\"subtype\":\"initial\"}\n")
            .await
            .unwrap();
        let tailer = TranscriptTailer::follow(path.clone());
        let mut rows = tailer.rows();
        assert_eq!(json(rows.recv().await.unwrap())["subtype"], "initial");
        assert_eq!(
            json(rows.recv().await.unwrap())["type"],
            "amux.transcript_ready"
        );

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        use tokio::io::AsyncWriteExt;
        file.write_all(b"{\"type\":\"user\",\"uuid\":\"frag")
            .await
            .unwrap();
        file.flush().await.unwrap();
        // A window: a row that must not be consumed leaves no mark to wait
        // on, so nothing may arrive for the whole of it.
        assert!(
            tokio::time::timeout(Duration::from_millis(300), rows.recv())
                .await
                .is_err(),
            "an unterminated JSONL row must not be consumed"
        );

        file.write_all(b"mented\"}\n").await.unwrap();
        file.flush().await.unwrap();
        let row = tokio::time::timeout(Duration::from_secs(2), rows.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(json(row)["uuid"], "fragmented");
    }
}
