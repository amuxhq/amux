//! A wake an app's run loop watches: a descriptor that is readable while a
//! ring waits in it, until it is drained.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// A descriptor an app's run loop watches (iOS's, Android's): one end of a
/// socket pair, readable after [`ring`](Self::ring) until
/// [`drain`](Self::drain). Neither call blocks; a ring into a full buffer
/// is a ring already. Windows: no descriptor ([`raw`](Self::raw) is -1),
/// and ringing and draining do nothing, since no app's run loop there
/// watches one (documented exception).
pub struct Descriptor {
    inner: imp::Descriptor,
}

impl Descriptor {
    pub fn new() -> std::io::Result<Descriptor> {
        Ok(Descriptor {
            inner: imp::Descriptor::new()?,
        })
    }

    /// The descriptor to watch for reading, or -1 where there is none.
    pub fn raw(&self) -> i32 {
        self.inner.raw()
    }

    /// Makes it readable.
    pub fn ring(&self) {
        self.inner.ring();
    }

    /// Reads it empty, so it is readable again only after the next ring.
    pub fn drain(&self) {
        self.inner.drain();
    }
}
