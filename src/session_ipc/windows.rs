//! Windows endpoint identity and current-user-only named-pipe security.

use interprocess::os::windows::security_descriptor::{
    AsSecurityDescriptorExt, BorrowedSecurityDescriptor, SecurityDescriptor,
};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::os::windows::{
    fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::Path;
use windows::Win32::{
    Foundation::{HANDLE, HLOCAL, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};
use windows::core::{PCWSTR, PWSTR};

fn current_user_sid() -> std::io::Result<String> {
    // OpenProcessToken returns an owned handle. OwnedHandle closes it on every exit.
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let token_handle = HANDLE(token.as_raw_handle());
    let mut size = 0;
    // The sizing call normally fails with ERROR_INSUFFICIENT_BUFFER.
    let _ = unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut size) };
    if size == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Word alignment is required for TOKEN_USER and the SID stored after it.
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )?;
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) }?;
    let result = unsafe { sid.to_string() }.map_err(std::io::Error::other);
    unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))) };
    result
}

pub(super) fn endpoint(cwd: &Path) -> std::io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(cwd)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session worktree must be a directory",
        ));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    // Volume + directory file ID collapses case, junction and extended-path aliases
    // without conflating distinct directories on case-sensitive Windows filesystems.
    let mut hash = Sha256::new();
    hash.update(b"xi-session-ipc-windows-v1\0");
    hash.update(current_user_sid()?.as_bytes());
    hash.update(info.dwVolumeSerialNumber.to_le_bytes());
    hash.update(info.nFileIndexHigh.to_le_bytes());
    hash.update(info.nFileIndexLow.to_le_bytes());
    Ok(format!(
        r"\\.\pipe\xi-session-v1-{}",
        super::hex_digest(&hash.finalize())
    ))
}

pub(super) fn security_descriptor() -> std::io::Result<SecurityDescriptor> {
    // Protected DACL: only the process user receives access, not Everyone or
    // inherited groups. Remote clients are separately rejected by the listener.
    let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            1,
            &mut raw,
            None,
        )
    }?;
    // interprocess owns an absolute copy; the Windows conversion returns a
    // self-relative allocation that must be released with LocalFree.
    let result = unsafe { BorrowedSecurityDescriptor::from_ptr(raw.0) }.to_owned_sd();
    unsafe { LocalFree(Some(HLOCAL(raw.0))) };
    result
}
