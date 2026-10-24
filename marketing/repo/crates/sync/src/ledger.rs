use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

pub struct Ledger {
    entries: Mutex<Vec<Entry>>,
    cursor: AtomicU64,
}

impl Ledger {
    pub async fn apply(&self, batch: Vec<Entry>) {
        let newest = batch.iter().map(|entry| entry.seq).max();
        self.entries.lock().await.extend(batch);
        if let Some(seq) = newest {
            self.cursor.store(seq, Ordering::SeqCst);
        }
    }
}
