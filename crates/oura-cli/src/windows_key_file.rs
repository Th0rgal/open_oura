//! Auth-key files on Windows that only the current user can open.
//!
//! `std::fs::write` gives a new file the ACL inherited from its directory, which
//! can let other local accounts read the key. This is the Windows counterpart of
//! the Unix `0600` path: the file is created with a protected DACL whose single
//! ACE grants the current user full access, so the key is never readable by
//! anyone else, even briefly.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use anyhow::{anyhow, bail, Context, Result};
use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_SUCCESS, FALSE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
    DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, MoveFileExW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    FILE_SHARE_NONE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `ACCESS_ALLOWED_ACE_TYPE` from winnt.h.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Write `contents` to `path` so that only the current user can open it.
///
/// The bytes go to a new sibling file created with the restricted DACL, which
/// then replaces `path`. The DACL is checked on the still-empty file, so a volume
/// that cannot store it (FAT, exFAT) fails before any key bytes are written. An
/// existing key keeps its old contents if anything fails, and an existing file
/// with a broader ACL is replaced rather than reused.
pub fn write_user_only(path: &Path, contents: &[u8]) -> Result<()> {
    let user = CurrentUser::get()?;
    let sddl = user_only_sddl(&user)?;
    let temp = temp_path(path);
    let result = create_new(&temp, &sddl)
        .and_then(|file| {
            check_user_only(&file, &user)?;
            write_and_flush(file, contents)
        })
        .and_then(|()| replace(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Fails unless `path` has a DACL with exactly one ACE, allowing the current user.
#[cfg(test)]
fn check_user_only_file(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .open(path)?;
    check_user_only(&file, &CurrentUser::get()?)
}

/// Create `path` with the DACL in `sddl`, then write `contents`.
#[cfg(test)]
fn write_new(path: &Path, contents: &[u8], sddl: &str) -> Result<()> {
    write_and_flush(create_new(path, sddl)?, contents)
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "oura.key".into(), |n| n.to_string_lossy().into_owned());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    path.with_file_name(format!("{name}.{}-{nanos}.tmp", std::process::id()))
}

/// Create `path`, which must not exist, with the DACL in `sddl`. The handle
/// allows writing and reading the DACL.
fn create_new(path: &Path, sddl: &str) -> Result<File> {
    let descriptor = security_descriptor(sddl)?;
    let path = wide(path.as_os_str());
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: FALSE,
    };
    // SAFETY: the path is NUL-terminated and the security descriptor outlives the call.
    let handle: HANDLE = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_WRITE,
            FILE_SHARE_NONE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error("creating the key file"));
    }
    // SAFETY: CreateFileW returned a valid handle that nothing else owns.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Write and flush `contents`, then close the file.
fn write_and_flush(mut file: File, contents: &[u8]) -> Result<()> {
    file.write_all(contents).context("writing the key file")?;
    file.sync_all().context("flushing the key file")?;
    Ok(())
}

/// Replace `to` with `from`, asking Windows to write the rename through to disk.
/// Both paths are in the same directory, so the file keeps its DACL.
fn replace(from: &Path, to: &Path) -> Result<()> {
    let from = wide(from.as_os_str());
    let to = wide(to.as_os_str());
    // SAFETY: both paths are NUL-terminated.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == FALSE {
        return Err(last_error("moving the new key file into place"));
    }
    Ok(())
}

/// SDDL for a protected DACL (`D:P`, so no inherited ACEs) with one ACE allowing
/// the current user full file access (`A;;FA`).
fn user_only_sddl(user: &CurrentUser) -> Result<String> {
    let mut sid_string: *mut u16 = null_mut();
    // SAFETY: the SID comes from the current token; the API allocates the string.
    if unsafe { ConvertSidToStringSidW(user.sid(), &mut sid_string) } == FALSE {
        return Err(last_error("formatting the current user's SID"));
    }
    let sid_string = LocalBox(sid_string.cast());
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated UTF-16 string.
    let sid = unsafe {
        let chars = sid_string.0.cast::<u16>();
        let len = (0..).take_while(|&i| *chars.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(chars, len))
    };
    Ok(format!("D:P(A;;FA;;;{sid})"))
}

fn security_descriptor(sddl: &str) -> Result<LocalBox> {
    let sddl = wide(sddl.as_ref());
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the SDDL string is NUL-terminated; the API allocates the descriptor.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == FALSE {
        return Err(last_error("building the key file security descriptor"));
    }
    Ok(LocalBox(descriptor))
}

/// Fails unless the open `file` has a DACL with exactly one ACE, allowing `user`.
/// The handle needs `READ_CONTROL`.
fn check_user_only(file: &File, user: &CurrentUser) -> Result<()> {
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the handle is open for the duration of the call; the API allocates
    // the descriptor that `dacl` points into, and `_descriptor` frees it after
    // the checks below.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        bail!("reading the key file permissions (Win32 error {status})");
    }
    let _descriptor = LocalBox(descriptor);
    if dacl.is_null() {
        bail!("the key file has no DACL, so everyone can open it");
    }
    // SAFETY: `dacl` is valid while `_descriptor` is alive, and GetAce returns a
    // pointer into it. An ACCESS_ALLOWED ACE stores its SID at `SidStart`.
    unsafe {
        let count = (*dacl).AceCount;
        if count != 1 {
            bail!("the key file DACL has {count} entries; expected only the current user");
        }
        let mut ace: *mut c_void = null_mut();
        if GetAce(dacl, 0, &mut ace) == FALSE {
            return Err(last_error("reading the key file DACL entry"));
        }
        if (*ace.cast::<ACE_HEADER>()).AceType != ACCESS_ALLOWED_ACE_TYPE {
            bail!("the key file DACL entry does not allow access");
        }
        let ace_sid: PSID =
            std::ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast();
        if EqualSid(ace_sid, user.sid()) == FALSE {
            bail!("the key file DACL entry is not for the current user");
        }
    }
    Ok(())
}

/// The current process user's `TOKEN_USER`, kept in a pointer-aligned buffer.
struct CurrentUser(Vec<u64>);

impl CurrentUser {
    fn get() -> Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == FALSE {
            return Err(last_error("opening the process token"));
        }
        // SAFETY: OpenProcessToken returned a handle that nothing else owns.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let token_handle = token.as_raw_handle();
        let mut needed = 0u32;
        // SAFETY: a size query with a null buffer; it fails by design and sets `needed`.
        unsafe { GetTokenInformation(token_handle, TokenUser, null_mut(), 0, &mut needed) };
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let size = (buffer.len() * 8) as u32;
        // SAFETY: the buffer holds `size` writable bytes.
        let ok = unsafe {
            GetTokenInformation(
                token_handle,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut needed,
            )
        };
        if ok == FALSE {
            return Err(last_error(
                "reading the current user from the process token",
            ));
        }
        Ok(Self(buffer))
    }

    fn sid(&self) -> PSID {
        // SAFETY: the buffer holds a TOKEN_USER written by GetTokenInformation,
        // whose SID pointer refers to the same buffer.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// Memory that a Win32 call allocated for us with `LocalAlloc`.
struct LocalBox(*mut c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 API documented to need LocalFree.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Read the thread's last Win32 error. Call before any other Win32 call can
/// overwrite it.
fn last_error(context: &str) -> anyhow::Error {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    anyhow!("{context} (Win32 error {code})")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everyone may open the file: a deliberately broad ACL.
    const EVERYONE: &str = "D:P(A;;FA;;;WD)";

    fn scratch_dir(test: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oura-key-{test}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replaces_a_broadly_readable_key_file_with_a_user_only_one() {
        let dir = scratch_dir("replace");
        let path = dir.join("ring.key");
        write_new(&path, b"old", EVERYONE).unwrap();
        assert!(check_user_only_file(&path).is_err());

        write_user_only(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        check_user_only_file(&path).unwrap();
        write_user_only(&path, b"newer").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer");
        check_user_only_file(&path).unwrap();

        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(leftovers, 1, "temporary key files were left behind");
    }

    #[test]
    fn rejects_any_acl_beyond_the_current_user() {
        let dir = scratch_dir("check");
        let user_only = user_only_sddl(&CurrentUser::get().unwrap()).unwrap();
        let shared = format!("{user_only}(A;;FR;;;WD)");
        let errors: Vec<String> = [(EVERYONE, "everyone"), (shared.as_str(), "shared")]
            .into_iter()
            .map(|(sddl, name)| {
                let path = dir.join(name);
                write_new(&path, b"key", sddl).unwrap();
                check_user_only_file(&path).unwrap_err().to_string()
            })
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            errors[0].contains("not for the current user"),
            "{}",
            errors[0]
        );
        assert!(errors[1].contains("has 2 entries"), "{}", errors[1]);
    }
}
