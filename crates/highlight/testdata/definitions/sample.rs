//! A store of named blobs.

use std::collections::HashMap;

/// The most blobs a store holds.
pub const MAX_BLOBS: usize = 1024;

static GREETING: &str = "hello";

/// A store of blobs, by name.
#[derive(Debug, Default)]
pub struct Store {
    blobs: HashMap<String, Vec<u8>>,
}

pub enum Error {
    Full,
    Missing(String),
}

pub trait Backend {
    fn read(&self, name: &str) -> Option<Vec<u8>>;

    fn exists(&self, name: &str) -> bool {
        self.read(name).is_some()
    }
}

impl Store {
    const LIMIT: usize = MAX_BLOBS;

    pub fn open() -> Store {
        let check = |n: usize| n < Self::LIMIT;
        assert!(check(0));
        Store::default()
    }

    pub fn put(&mut self, name: &str, blob: Vec<u8>) -> Result<(), Error> {
        if self.blobs.len() >= Self::LIMIT {
            return Err(Error::Full);
        }
        self.blobs.insert(name.to_owned(), blob);
        Ok(())
    }
}

impl<'a> Backend for &'a Store {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        self.blobs.get(name).cloned()
    }
}

type Blobs = HashMap<String, Vec<u8>>;

macro_rules! blob {
    ($text:expr) => {
        $text.as_bytes().to_vec()
    };
}

mod tests {
    use super::*;

    #[test]
    fn puts_a_blob() {
        let mut store = Store::open();
        store.put("a", blob!("x")).ok();
    }
}
