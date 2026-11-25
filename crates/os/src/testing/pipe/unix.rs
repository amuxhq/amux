use std::fs::File;
use std::os::fd::OwnedFd;

pub(super) fn file(end: impl Into<OwnedFd>) -> File {
    File::from(end.into())
}
