//! `os::wake` on Windows: no descriptor, since no app's run loop there
//! watches one.

pub(super) struct Descriptor;

impl Descriptor {
    pub(super) fn new() -> std::io::Result<Descriptor> {
        Ok(Descriptor)
    }

    pub(super) fn raw(&self) -> i32 {
        -1
    }

    pub(super) fn ring(&self) {}

    pub(super) fn drain(&self) {}
}
