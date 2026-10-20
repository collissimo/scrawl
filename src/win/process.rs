//! Process facts by pid: executable path, app id (D5) and display name.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use win32::Win32::Foundation::{CloseHandle, HANDLE};
use win32::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use win32::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
    TH32CS_SNAPPROCESS,
};
use win32::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use win32::core::{HSTRING, PWSTR};

pub fn own_pid() -> u32 {
    std::process::id()
}

/// Closes a handle when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle was opened by us and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// The full Win32 path of `pid`'s executable.
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    // SAFETY: plain query access; the handle is closed by `Owned`.
    let handle = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; `len` receives the written length.
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    buf.truncate(len as usize);
    Some(PathBuf::from(String::from_utf16_lossy(&buf)))
}

/// The app id Scrawl records for `pid` (D5): the lowercase exe stem, `chrome`, `msedge`,
/// `windowsterminal`.
pub fn app_id(pid: u32) -> Option<String> {
    let path = exe_path(pid)?;
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    (!stem.is_empty()).then_some(stem)
}

static NAMES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

/// The app's display name: the executable's FileDescription, else its stem. Cached per pid.
pub fn app_name(pid: u32) -> Option<String> {
    if let Some(name) = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|names| names.get(&pid).cloned())
    {
        return Some(name);
    }
    let path = exe_path(pid)?;
    let name = file_description(&path).or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    })?;
    NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(pid, name.clone());
    Some(name)
}

fn file_description(path: &std::path::Path) -> Option<String> {
    let file = HSTRING::from(path.as_os_str());
    // SAFETY: `file` outlives the calls; `data` is sized by GetFileVersionInfoSizeW and the
    // pointers VerQueryValueW returns point into it.
    unsafe {
        let size = GetFileVersionInfoSizeW(&file, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&file, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            &HSTRING::from("\\VarFileInfo\\Translation"),
            &mut ptr,
            &mut len,
        )
        .as_bool()
            || len < 4
            || ptr.is_null()
        {
            return None;
        }
        let lang = *(ptr as *const u16);
        let code_page = *(ptr as *const u16).add(1);
        let key = format!("\\StringFileInfo\\{lang:04x}{code_page:04x}\\FileDescription");
        let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut value_len = 0u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            &HSTRING::from(key),
            &mut value,
            &mut value_len,
        )
        .as_bool()
            || value.is_null()
            || value_len == 0
        {
            return None;
        }
        let chars = std::slice::from_raw_parts(value as *const u16, value_len as usize);
        let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
        let text = String::from_utf16_lossy(&chars[..end]).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// The pid that started `pid`, from a Toolhelp process snapshot. `None` when `pid` isn't
/// listed or its parent is 0.
pub fn parent_pid(pid: u32) -> Option<u32> {
    // SAFETY: the snapshot handle is closed by `Owned`; the entry has its size set.
    unsafe {
        let snapshot = Owned(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut entry).ok()?;
        loop {
            if entry.th32ProcessID == pid {
                let parent = entry.th32ParentProcessID;
                return (parent != 0).then_some(parent);
            }
            if Process32NextW(snapshot.0, &mut entry).is_err() {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_has_an_exe_and_an_app_id() {
        let pid = super::own_pid();
        assert!(super::exe_path(pid).is_some());
        assert!(super::app_id(pid).is_some_and(|id| id == id.to_lowercase()));
        assert!(super::app_name(pid).is_some());
        assert!(super::parent_pid(pid).is_some());
    }
}
