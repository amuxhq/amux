use std::io;

use super::Window;
use crate::Pid;

pub(super) fn visible_windows() -> io::Result<Vec<Window>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no amux process has windows on Unix",
    ))
}

pub(super) fn show_a_window(_title: &str, _while_shown: impl FnOnce()) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no amux process has windows on Unix",
    ))
}

pub(super) fn job_members() -> io::Result<Vec<Pid>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix has no jobs",
    ))
}
