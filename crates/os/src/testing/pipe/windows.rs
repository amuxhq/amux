use std::fs::File;
use std::os::windows::io::OwnedHandle;

pub(super) fn file(end: impl Into<OwnedHandle>) -> File {
    File::from(end.into())
}
