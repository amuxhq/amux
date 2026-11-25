//! An anonymous pipe whose two ends are files, for a test that hands them
//! to a process it starts as the supervisor hands `amux daemon` its
//! activation pipe: [`crate::process::set_inheritable`] and
//! [`crate::process::raw_for_child`] take files, and std's pipe ends turn
//! into one only through a type each OS names differently.

use std::fs::File;
use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// A pipe: `(read, write)`, what is written to the second end read from
/// the first. Neither end is inheritable until the caller marks it so.
pub fn pipe() -> io::Result<(File, File)> {
    let (read, write) = io::pipe()?;
    Ok((imp::file(read), imp::file(write)))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    #[test]
    fn what_is_written_to_one_end_is_read_from_the_other() {
        let (mut read, mut write) = super::pipe().unwrap();
        write.write_all(b"prepared\n").unwrap();
        drop(write);
        let mut said = String::new();
        read.read_to_string(&mut said).unwrap();
        assert_eq!(said, "prepared\n");
    }
}
