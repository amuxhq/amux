//! `os::fs` on Windows: protected DACLs, renames through an open handle
//! with POSIX semantics, and sharing violations waited out.

use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write as _};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_DELETE_PENDING, ERROR_DIR_NOT_EMPTY,
    ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_LOCK_VIOLATION, ERROR_NOT_SUPPORTED,
    ERROR_SHARING_VIOLATION, ERROR_USER_MAPPED_FILE, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, FileDispositionInfo, FileDispositionInfoEx, FileRenameInfo, FileRenameInfoEx,
    FlushFileBuffers, GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW, OPEN_ALWAYS,
    OPEN_EXISTING, SetFileInformationByHandle, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::FILE_SUPPORTS_POSIX_UNLINK_RENAME;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
};

use super::{Create, Flush, Publish, SHARING_PATIENCE, VolumeCheck, VolumeProblem};

/// Windows writes a canonical path verbatim (`\\?\C:\…`).
pub(super) const VERBATIM_PATHS: bool = true;

// Owner-only ----------------------------------------------------------------

pub(super) fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return make_private(dir);
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let descriptor = PrivateDescriptor::new(true)?;
    let attributes = descriptor.attributes();
    let wide = wide(dir)?;
    // SAFETY: `wide` is NUL-terminated and `attributes` points at a
    // descriptor that outlives the call.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) && dir.is_dir() {
        // Created by someone else in between: made private all the same.
        return make_private(dir);
    }
    Err(error)
}

pub(super) fn make_private(path: &Path) -> io::Result<()> {
    let dir = fs::metadata(path)?.is_dir();
    let descriptor = PrivateDescriptor::new(dir)?;
    let wide = wide(path)?;
    // SAFETY: `wide` is NUL-terminated; the ACL lives in `descriptor`,
    // which outlives the call. No owner, group or SACL is changed.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            descriptor.dacl()?,
            ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

pub(super) fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    options
}

pub(super) fn create_private(path: &Path, how: Create) -> io::Result<File> {
    let descriptor = PrivateDescriptor::new(false)?;
    let attributes = descriptor.attributes();
    let wide = wide(path)?;
    let (access, disposition) = match how {
        Create::New => (FILE_GENERIC_READ | FILE_GENERIC_WRITE, CREATE_NEW),
        Create::Truncate => (FILE_GENERIC_READ | FILE_GENERIC_WRITE, CREATE_ALWAYS),
        // As std appends: FILE_APPEND_DATA without FILE_WRITE_DATA, so every
        // write lands at the end, whoever else writes.
        Create::Append => (
            FILE_GENERIC_READ | (FILE_GENERIC_WRITE & !FILE_WRITE_DATA) | FILE_APPEND_DATA,
            OPEN_ALWAYS,
        ),
    };
    // SAFETY: `wide` is NUL-terminated and `attributes` points at a
    // descriptor that outlives the call. A file this creates takes the
    // descriptor; one that exists keeps its own, set below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // CREATE_ALWAYS and OPEN_ALWAYS succeed on an existing file and say so.
    let existed = how != Create::New
        && io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32);
    // SAFETY: a valid handle this function owns.
    let file = unsafe { File::from_raw_handle(handle) };
    if existed {
        // SAFETY: the handle is open with WRITE_DAC; the ACL lives in
        // `descriptor`, which outlives the call.
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                descriptor.dacl()?,
                ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
    }
    Ok(file)
}

pub(super) fn current_user_sid() -> io::Result<String> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(sid) = SID.get() {
        return Ok(sid.clone());
    }
    let sid = token_user_sid()?;
    Ok(SID.get_or_init(|| sid).clone())
}

fn token_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle needs no closing; `token` receives a handle
    // owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a handle this function owns from here.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0u32;
    // SAFETY: a size query: no buffer, `needed` receives the size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    // u64 words keep the TOKEN_USER the buffer starts with aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` holds `needed` bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled the buffer with a TOKEN_USER,
    // whose SID lies inside the buffer.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `user.User.Sid` is a valid SID; `text` receives a LocalAlloc'd
    // string freed below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is NUL-terminated.
    let sid = unsafe { from_wide_nul(text) };
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(text.cast()) };
    sid.into_string()
        .map_err(|_| io::Error::other("a SID that is not text"))
}

/// The security descriptor for something owner-only: a protected DACL
/// giving full control to the current user, SYSTEM and Administrators,
/// inherited by what a folder holds.
struct PrivateDescriptor(PSECURITY_DESCRIPTOR);

impl PrivateDescriptor {
    fn new(dir: bool) -> io::Result<Self> {
        let sid = current_user_sid()?;
        let inherit = if dir { "OICI" } else { "" };
        let sddl = format!("D:P(A;{inherit};FA;;;{sid})(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)");
        let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a
        // LocalAlloc'd descriptor freed on drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }

    fn dacl(&self) -> io::Result<*const ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: `self.0` is a valid descriptor; the ACL pointer points
        // into it and lives as long as `self`.
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(dacl)
    }
}

impl Drop for PrivateDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

// Durable whole-file writes --------------------------------------------------

/// The temporary file, owner-only from its creation, opened for writing
/// and for the rename that puts it in place, and shared with nobody.
pub(super) fn create_temp(temp: &Path) -> io::Result<File> {
    let descriptor = PrivateDescriptor::new(false)?;
    let attributes = descriptor.attributes();
    let wide = wide(temp)?;
    // SAFETY: `wide` is NUL-terminated and `attributes` outlives the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | DELETE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle this function owns.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Writes and flushes the temporary file, then renames it into place
/// through its own handle, retrying sharing violations, and closes it.
///
/// If any of it fails, the temporary file is deleted through the same
/// handle before it closes. Shared with nobody while open, it can't be
/// held by anyone else (a scanner that opens a file as it closes) when
/// the deletion is asked for, so it never stays behind.
pub(super) fn finish_durably(
    mut file: File,
    _temp: &Path,
    path: &Path,
    bytes: &[u8],
    publish: Publish,
    flush: Flush,
) -> io::Result<()> {
    let mut put = || {
        file.write_all(bytes)?;
        if flush == Flush::Yes {
            file.sync_all()?;
        }
        let target = wide(path)?;
        super::patiently(SHARING_PATIENCE, || {
            rename_by_handle(&file, &target, publish == Publish::Replace)
        })
    };
    let put = put();
    if put.is_err() {
        // A failure here leaves the file for the caller's own removal.
        let _ = delete_on_close(&file);
    }
    put
}

/// Marks the file `file` is open on (with DELETE access) for deletion: at
/// once with POSIX semantics where the volume has them, else as the last
/// handle closes.
pub(crate) fn delete_on_close(file: &File) -> io::Result<()> {
    let posix = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the handle is open; the struct outlives the call, which reads
    // `size_of` bytes of it.
    let done = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const posix).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0;
    if done {
        return Ok(());
    }
    let plain = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above.
    let done = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const plain).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } != 0;
    if done {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Renames the file `file` is open on to `target` (NUL-terminated): with
/// POSIX semantics where the volume has them, so a target open with delete
/// sharing is replaced at once; otherwise a plain rename.
fn rename_by_handle(file: &File, target: &[u16], replace: bool) -> io::Result<()> {
    let posix = FILE_RENAME_FLAG_POSIX_SEMANTICS
        | if replace {
            FILE_RENAME_FLAG_REPLACE_IF_EXISTS
        } else {
            0
        };
    match set_rename_info(file, target, FileRenameInfoEx, posix) {
        Err(error)
            if [
                ERROR_INVALID_PARAMETER,
                ERROR_NOT_SUPPORTED,
                ERROR_INVALID_FUNCTION,
            ]
            .iter()
            .any(|&code| error.raw_os_error() == Some(code as i32)) =>
        {
            // FAT, exFAT and Windows before 10 1809 refuse POSIX semantics;
            // FileRenameInfo's first field is then ReplaceIfExists.
            set_rename_info(file, target, FileRenameInfo, u32::from(replace))
        }
        done => done,
    }
}

fn set_rename_info(file: &File, target: &[u16], class: i32, flags: u32) -> io::Result<()> {
    let name = &target[..target.len() - 1];
    let name_bytes = size_of_val(name);
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset + name_bytes + size_of::<u16>();
    // u64 words keep the FILE_RENAME_INFO the buffer starts with aligned.
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buffer` is zeroed, aligned and `size` bytes long: the fixed
    // fields, then the name and its NUL.
    unsafe {
        (*info).Anonymous.Flags = flags;
        (*info).RootDirectory = ptr::null_mut();
        (*info).FileNameLength = name_bytes as u32;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    // SAFETY: `file` is open with DELETE access; `info` describes `size` bytes.
    if unsafe { SetFileInformationByHandle(file.as_raw_handle(), class, info.cast(), size as u32) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    let wide = wide(dir)?;
    // SAFETY: `wide` is NUL-terminated; the handle is owned below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle this function owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    // SAFETY: a valid handle opened for writing.
    if unsafe { FlushFileBuffers(handle.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Removal ----------------------------------------------------------------------

/// Removes a file, read-only or not: Windows refuses to delete a
/// read-only file, which git's objects are. A link to a folder (a
/// directory symlink or a junction) is a folder to Windows: it goes as an
/// empty folder does, the link alone, nothing it points at. An ACL that
/// refuses the deletion is told apart (see [`refusal_told_apart`]).
pub(super) fn remove_file(path: &Path) -> io::Result<()> {
    refusal_told_apart(path, || remove_file_now(path))
}

/// Removes a folder and what it holds, an ACL's refusal told apart.
pub(super) fn remove_dir_all(path: &Path) -> io::Result<()> {
    refusal_told_apart(path, || fs::remove_dir_all(path))
}

// The thread's last NTSTATUS, which windows-sys doesn't carry. raw-dylib
// needs no import library.
#[link(name = "ntdll.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn RtlGetLastNtStatus() -> i32;
    fn RtlSetLastWin32ErrorAndNtStatusFromNtStatus(status: i32) -> u32;
}

/// Runs `remove`, and turns an ACCESS_DENIED that NT gave as
/// STATUS_ACCESS_DENIED, an ACL refusing the deletion, into an error
/// [`is_held`] doesn't wait on: waiting can't fix it, and a sweep would
/// put the path off for ever. Windows reports a deletion already pending
/// and an image still mapped as ACCESS_DENIED too (STATUS_DELETE_PENDING,
/// STATUS_CANNOT_DELETE), and those stay held. The thread's last status
/// is cleared first, so a stale one is never read: a failure that set none
/// stays held, as before.
fn refusal_told_apart(path: &Path, remove: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    const STATUS_SUCCESS: i32 = 0;
    const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
    // SAFETY: sets this thread's last error and status, nothing else.
    unsafe { RtlSetLastWin32ErrorAndNtStatusFromNtStatus(STATUS_SUCCESS) };
    match remove() {
        Err(error)
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                // SAFETY: reads this thread's last status, nothing else.
                && unsafe { RtlGetLastNtStatus() } == STATUS_ACCESS_DENIED =>
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: its deletion is refused: {error}", path.display()),
            ))
        }
        done => done,
    }
}

fn remove_file_now(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt as _;
    if fs::symlink_metadata(path)?.file_type().is_symlink_dir() {
        return fs::remove_dir(path);
    }
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let meta = fs::symlink_metadata(path)?;
            let mut permissions = meta.permissions();
            if !permissions.readonly() {
                return Err(error);
            }
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
            fs::remove_file(path)
        }
        done => done,
    }
}

/// Whether `error` is something else holding the file, which waiting can
/// fix: a sharing or lock violation, a mapped file, a deletion pending, a
/// folder still emptying. ACCESS_DENIED is among them, as that is what a
/// file whose deletion is pending reports to an open or a rename; a
/// removal tells an ACL's refusal apart first ([`refusal_told_apart`]).
pub(super) fn is_held(error: &io::Error) -> bool {
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    [
        ERROR_SHARING_VIOLATION,
        ERROR_LOCK_VIOLATION,
        ERROR_ACCESS_DENIED,
        ERROR_DELETE_PENDING,
        ERROR_DIR_NOT_EMPTY,
        ERROR_USER_MAPPED_FILE,
    ]
    .iter()
    .any(|&held| code == held as i32)
}

// Executables ------------------------------------------------------------------

pub(super) fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn is_executable(path: &Path, meta: &Metadata) -> bool {
    meta.is_file() && crate::paths::windows::runs_by_extension(path)
}

// The root's volume ----------------------------------------------------------------

pub(super) fn volume_supports_root(path: &Path) -> io::Result<VolumeCheck> {
    let mut existing = std::path::absolute(path)?;
    while !existing.exists() {
        if !existing.pop() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no part of {} exists", path.display()),
            ));
        }
    }
    let root = volume_root(&existing)?;
    let root_wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    let mut problems = Vec::new();
    // SAFETY: `root_wide` is NUL-terminated.
    let kind = match unsafe { GetDriveTypeW(root_wide.as_ptr()) } {
        DRIVE_FIXED => None,
        DRIVE_REMOTE => Some("network share"),
        DRIVE_REMOVABLE => Some("removable drive"),
        DRIVE_CDROM => Some("optical drive"),
        DRIVE_RAMDISK => Some("RAM disk"),
        _ => Some("drive of unknown kind"),
    };
    if let Some(kind) = kind {
        problems.push(VolumeProblem::NotLocal { kind });
    }
    let mut flags = 0u32;
    let mut name = [0u16; 261];
    // SAFETY: `root_wide` is NUL-terminated; `name` holds 261 characters.
    if unsafe {
        GetVolumeInformationW(
            root_wide.as_ptr(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let file_system = String::from_utf16_lossy(&name[..length]);
    if !file_system.eq_ignore_ascii_case("NTFS") && !file_system.eq_ignore_ascii_case("ReFS") {
        problems.push(VolumeProblem::FileSystem { name: file_system });
    }
    if flags & FILE_SUPPORTS_POSIX_UNLINK_RENAME == 0 {
        problems.push(VolumeProblem::NoPosixRename);
    }
    Ok(if problems.is_empty() {
        VolumeCheck::Suitable
    } else {
        VolumeCheck::Unsuitable(problems)
    })
}

/// The root of the volume `path` is on, as `C:\` or `\\server\share\`.
fn volume_root(path: &Path) -> io::Result<PathBuf> {
    let wide = wide(path)?;
    let mut root = vec![0u16; wide.len().max(261)];
    // SAFETY: `wide` is NUL-terminated; `root` holds `root.len()` characters.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    let root = PathBuf::from(OsString::from_wide(&root[..length]));
    Ok(super::plain(root))
}

// Paths as Windows takes them ------------------------------------------------------

/// `path` made absolute, NUL-terminated, and verbatim when it is too long
/// for a plain path, as std does, so a deep root works whether or not the
/// process is long-path aware.
pub(crate) fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let mut wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path holds a NUL",
        ));
    }
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    if wide.len() >= 248 && !wide.starts_with(&verbatim) {
        let unc: Vec<u16> = r"\\".encode_utf16().collect();
        wide = if wide.starts_with(&unc) {
            r"\\?\UNC\"
                .encode_utf16()
                .chain(wide[2..].iter().copied())
                .collect()
        } else {
            verbatim.into_iter().chain(wide).collect()
        };
    }
    wide.push(0);
    Ok(wide)
}

/// The NUL-terminated wide string at `text`.
///
/// # Safety
/// `text` must point at a NUL-terminated UTF-16 string.
unsafe fn from_wide_nul(text: *const u16) -> OsString {
    let mut length = 0;
    // SAFETY: the caller promises a terminating NUL.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` characters precede the NUL.
    OsString::from_wide(unsafe { std::slice::from_raw_parts(text, length) })
}

#[cfg(test)]
pub(super) mod testing {
    //! Reading back what this module sets.

    use super::*;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };

    /// The DACL of `path` as SDDL (`D:PAI(A;OICI;FA;;;S-1-5-…)…`).
    pub(crate) fn dacl_sddl(path: &Path) -> io::Result<String> {
        let wide = wide(path)?;
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `descriptor` receives a
        // LocalAlloc'd descriptor freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let mut text: *mut u16 = ptr::null_mut();
        // SAFETY: `descriptor` is valid; `text` receives a LocalAlloc'd string.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                ptr::null_mut(),
            )
        };
        let result = if converted == 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: NUL-terminated, freed just below.
            let sddl = unsafe { from_wide_nul(text) };
            unsafe { LocalFree(text.cast()) };
            Ok(sddl.to_string_lossy().into_owned())
        };
        // SAFETY: allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(descriptor) };
        result
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
