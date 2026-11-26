use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::{io, ptr};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, INHERIT_ONLY_ACE, NO_INHERITANCE,
    PSECURITY_DESCRIPTOR, PSID, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ};

/// SYSTEM and the Administrators group, beside the user, are who a private
/// DACL names.
const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";
/// The machine's Users group.
const USERS: &str = "S-1-5-32-545";

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

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

/// The DACL of `path` and the descriptor that owns it, freed on drop.
struct Dacl {
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *mut ACL,
}

impl Dacl {
    fn of(path: &Path) -> io::Result<Self> {
        let name = wide(path);
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; on success `descriptor` owns
        // what `dacl` points into.
        check(unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        })?;
        Ok(Self { descriptor, dacl })
    }
}

impl Drop for Dacl {
    fn drop(&mut self) {
        // SAFETY: allocated by GetNamedSecurityInfoW, freed once.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The SID at `sid` as text.
///
/// # Safety
/// `sid` must point at a valid SID.
unsafe fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: the caller promises a valid SID; `text` is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW writes a NUL-terminated string.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` characters precede the NUL.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: allocated by ConvertSidToStringSidW, freed once.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

pub(super) fn owner_only(path: &Path) -> io::Result<bool> {
    let user = crate::fs::current_user_sid()?;
    let dacl = Dacl::of(path)?;
    if dacl.dacl.is_null() {
        // No DACL: everyone may do anything.
        return Ok(false);
    }
    // SAFETY: a valid ACL, owned by `dacl`.
    let count = unsafe { (*dacl.dacl).AceCount };
    for n in 0..u32::from(count) {
        let mut entry: *mut std::ffi::c_void = ptr::null_mut();
        // SAFETY: `n` is below the entry count; `entry` points into the ACL.
        if unsafe { GetAce(dacl.dacl, n, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with its header.
        let header = unsafe { *entry.cast::<ACE_HEADER>() };
        if header.AceFlags & INHERIT_ONLY_ACE as u8 != 0 || header.AceType == ACCESS_DENIED_ACE_TYPE
        {
            // Not this object's, or one that takes away.
            continue;
        }
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            return Ok(false);
        }
        // ACCESS_ALLOWED_ACE: the header, the access mask, then the SID.
        // SAFETY: an allowed entry holds its SID at that offset.
        let sid = unsafe { sid_text(entry.cast::<u8>().add(8).cast()) }?;
        if sid != user && sid != SYSTEM && sid != ADMINISTRATORS {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn open_to_others(path: &Path) -> io::Result<()> {
    let dir = std::fs::metadata(path)?.is_dir();
    let dacl = Dacl::of(path)?;
    let users: Vec<u16> = USERS.encode_utf16().chain([0]).collect();
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: `users` is NUL-terminated; `sid` is freed below.
    if unsafe { ConvertStringSidToSidW(users.as_ptr(), &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: if dir {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.cast(),
        },
    };
    let mut opened: *mut ACL = ptr::null_mut();
    // SAFETY: one entry, the DACL read above, and an out pointer freed below.
    let made = check(unsafe { SetEntriesInAclW(1, &entry, dacl.dacl, &mut opened) });
    // SAFETY: allocated by ConvertStringSidToSidW, freed once; the new ACL
    // holds its own copy.
    unsafe { LocalFree(sid) };
    made?;
    let name = wide(path);
    // SAFETY: `name` is NUL-terminated and `opened` is the ACL just made.
    // A folder passes the new entry on to what it holds.
    let set = check(unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            opened,
            ptr::null(),
        )
    });
    // SAFETY: allocated by SetEntriesInAclW, freed once.
    unsafe { LocalFree(opened.cast()) };
    set
}
