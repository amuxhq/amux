use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::{FileTypeExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    ERROR_PRIVILEGE_NOT_HELD, ERROR_SHARING_VIOLATION, ERROR_SUCCESS, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW,
    NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::{Deny, Linked};

// File and directory access rights (winnt.h). The directory meaning of a
// bit is in brackets.
const FILE_READ_DATA: u32 = 0x1; // (list the directory)
const FILE_WRITE_DATA: u32 = 0x2; // (add a file)
const FILE_APPEND_DATA: u32 = 0x4; // (add a subdirectory)
const FILE_READ_EA: u32 = 0x8;
const FILE_WRITE_EA: u32 = 0x10;
const FILE_EXECUTE: u32 = 0x20; // (traverse)
const FILE_DELETE_CHILD: u32 = 0x40;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const DELETE: u32 = 0x1_0000;

/// What each [`Deny`] denies. None takes reading the attributes, which a
/// stat needs and `chmod 000` leaves, or the owner's reading and changing
/// of the DACL, which putting it back needs (and which the owner is granted
/// whatever an entry says).
fn mask(what: Deny) -> u32 {
    let read = FILE_READ_DATA | FILE_READ_EA;
    let write = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | FILE_DELETE_CHILD;
    match what {
        Deny::Read => read,
        Deny::Write => write,
        Deny::All => read | write | FILE_EXECUTE | DELETE,
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn check(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

/// The security descriptor `path` had before the fault, which owns the
/// DACL put back.
#[derive(Debug)]
pub(super) struct Restore {
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *mut ACL,
}

// SAFETY: the descriptor is memory LocalAlloc'd for this value alone.
unsafe impl Send for Restore {}
// SAFETY: as above; nothing reads it but `apply` and drop, which take self.
unsafe impl Sync for Restore {}

impl Restore {
    pub(super) fn apply(self, path: &Path) -> io::Result<()> {
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and `dacl` lies in the
        // descriptor this value owns, which outlives the call.
        check(unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                self.dacl,
                std::ptr::null(),
            )
        })
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: the descriptor GetNamedSecurityInfoW allocated, freed once.
        unsafe { LocalFree(self.descriptor) };
    }
}

pub(super) fn deny(path: &Path, what: Deny) -> io::Result<Restore> {
    let name = wide(path);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; the out pointers are valid, and on
    // success `descriptor` owns what `dacl` points into.
    check(unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    })?;
    let restore = Restore { descriptor, dacl };

    // Everyone, so the entry binds this user whatever its own entries grant.
    let everyone: Vec<u16> = "S-1-1-0".encode_utf16().chain([0]).collect();
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: `everyone` is NUL-terminated; `sid` is freed below.
    if unsafe { ConvertStringSidToSidW(everyone.as_ptr(), &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask(what),
        grfAccessMode: DENY_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.cast(),
        },
    };
    let mut denied: *mut ACL = std::ptr::null_mut();
    // SAFETY: one entry, the DACL read above, and an out pointer freed
    // below. SetEntriesInAclW puts a deny entry ahead of the allows.
    let made = check(unsafe { SetEntriesInAclW(1, &entry, restore.dacl, &mut denied) });
    // SAFETY: the SID ConvertStringSidToSidW allocated, freed once; the
    // new ACL holds its own copy.
    unsafe { LocalFree(sid) };
    made?;
    // SAFETY: `name` is NUL-terminated and `denied` is the ACL just made.
    let set = check(unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            denied,
            std::ptr::null(),
        )
    });
    // SAFETY: the ACL SetEntriesInAclW allocated, freed once.
    unsafe { LocalFree(denied.cast()) };
    set.map(|()| restore)
}

pub(super) fn hold_exclusively(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).share_mode(0).open(path)
}

pub(super) fn is_sharing_violation(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
}

const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// A junction: an empty directory carrying a mount-point reparse point
/// whose substitute name is the target's NT path (`\??\C:\…`).
pub(super) fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    let target = target.as_os_str().to_string_lossy();
    // `std::path::absolute` may give the verbatim form; the NT form is
    // `\??\` and the plain path either way.
    let plain = target.strip_prefix(r"\\?\").unwrap_or(&target);
    let substitute: Vec<u16> = format!(r"\??\{plain}").encode_utf16().collect();
    let print: Vec<u16> = plain.encode_utf16().collect();
    let bytes = |name: &[u16]| (name.len() * 2) as u16;

    // REPARSE_DATA_BUFFER's MountPointReparseBuffer: four lengths and
    // offsets, then both names, each NUL-terminated.
    let mut names = Vec::new();
    for unit in substitute.iter().chain([&0]).chain(&print).chain([&0]) {
        names.extend_from_slice(&unit.to_le_bytes());
    }
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&((8 + names.len()) as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&bytes(&substitute).to_le_bytes());
    buffer.extend_from_slice(&(bytes(&substitute) + 2).to_le_bytes());
    buffer.extend_from_slice(&bytes(&print).to_le_bytes());
    buffer.extend_from_slice(&names);

    std::fs::create_dir(link)?;
    let set = (|| {
        let dir = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(link)?;
        let mut returned = 0u32;
        // SAFETY: `buffer` is a whole reparse data buffer of the length
        // passed, and `dir` an open handle to the directory.
        let ok = unsafe {
            DeviceIoControl(
                dir.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast::<c_void>(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    if set.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    set
}

/// A junction or a directory symbolic link is removed as a directory, a
/// file symbolic link as a file; neither touches what it names.
pub(super) fn remove_link(link: &Path) -> io::Result<()> {
    // std calls both kinds of directory link a symbolic link, never a
    // directory.
    if std::fs::symlink_metadata(link)?
        .file_type()
        .is_symlink_dir()
    {
        std::fs::remove_dir(link)
    } else {
        std::fs::remove_file(link)
    }
}

pub(super) fn symlink(target: &Path, link: &Path) -> io::Result<Linked> {
    let made = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match made {
        Ok(()) => Ok(Linked::Made),
        Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) => {
            Ok(Linked::Skipped(
                "making a symbolic link takes SeCreateSymbolicLinkPrivilege or Developer Mode, \
                 which this user lacks"
                    .to_owned(),
            ))
        }
        Err(error) => Err(error),
    }
}
