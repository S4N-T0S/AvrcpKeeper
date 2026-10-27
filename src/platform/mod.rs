pub mod audio;
pub mod autostart;
pub mod console;
pub mod ipc;
pub mod security;
pub mod window;

use std::path::{Path, PathBuf};

use windows::core::{IInspectable, HSTRING, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemWindowsDirectoryW};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::RoGetActivationFactory;

/// Libraries loaded at runtime by name come from System32 only, so a DLL dropped next to the
/// exe (for example in a Downloads folder) is never picked up.
pub fn restrict_dll_search() {
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

pub fn local_timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."))
}

/// The real Windows directory, independent of the WINDIR environment variable.
pub fn windows_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len >= buf.len() {
        return PathBuf::from(r"C:\Windows");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

pub fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())])))
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// Checks that the WinRT classes are registered. If one isn't, windows-rs falls back to
/// loading the class's DLL by name, which would search the exe's folder.
pub fn require_winrt_classes(classes: &[&str]) -> Result<(), String> {
    for class in classes {
        if let Err(e) = unsafe { RoGetActivationFactory::<IInspectable>(&HSTRING::from(*class)) } {
            return Err(format!("{class} is unavailable ({})", e.message()));
        }
    }
    Ok(())
}

/// Initializes COM for the current thread for as long as the value lives.
pub struct Com(());

impl Com {
    pub fn init_mta() -> windows::core::Result<Com> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Com(()))
    }

    pub fn init_sta() -> windows::core::Result<Com> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Com(()))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
