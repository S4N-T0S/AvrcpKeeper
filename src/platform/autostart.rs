//! Per-user autostart through the `HKCU\...\Run` key.

use std::io;
use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
const VALUE_NAME: PCWSTR = w!("AvrcpKeeper");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Startup {
    Off,
    On,
    /// The entry starts a different copy of AvrcpKeeper.
    OtherCopy(PathBuf),
    /// The entry exists but was switched off in Task Manager's Startup apps page.
    DisabledInTaskManager,
}

fn command() -> io::Result<String> {
    Ok(format!("\"{}\" --background", std::env::current_exe()?.display()))
}

pub fn state() -> Startup {
    let Some(value) = read_string(RUN_KEY) else { return Startup::Off };
    let Ok(expected) = command() else { return Startup::Off };
    if !value.trim().eq_ignore_ascii_case(&expected) {
        return Startup::OtherCopy(command_exe(&value));
    }
    if disabled_in_task_manager() {
        Startup::DisabledInTaskManager
    } else {
        Startup::On
    }
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    if enabled {
        let wide: Vec<u16> = command()?.encode_utf16().chain(Some(0)).collect();
        let bytes = u32::try_from(wide.len() * 2).map_err(|_| io::Error::other("path too long"))?;
        let err = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, REG_SZ.0, Some(wide.as_ptr().cast()), bytes)
        };
        check(err)?;
    } else {
        delete(RUN_KEY)?;
    }
    // Task Manager keeps its own enabled/disabled flag; drop it so the entry behaves as shown.
    delete(APPROVED_KEY)
}

/// The executable part of a Run command line, quoted or not.
fn command_exe(command: &str) -> PathBuf {
    let command = command.trim();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split(" --").next().unwrap_or(command),
    };
    PathBuf::from(exe.trim())
}

fn read_string(subkey: PCWSTR) -> Option<String> {
    let mut buf = vec![0u16; 512];
    loop {
        let mut size = u32::try_from(buf.len() * 2).ok()?;
        let err = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey,
                VALUE_NAME,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if err == ERROR_MORE_DATA && buf.len() < 32 * 1024 {
            buf.resize((size as usize).div_ceil(2) + 1, 0);
            continue;
        }
        if err != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).min(buf.len());
        let end = buf[..len].iter().position(|&c| c == 0).unwrap_or(len);
        return Some(String::from_utf16_lossy(&buf[..end]));
    }
}

fn disabled_in_task_manager() -> bool {
    let mut data = [0u8; 12];
    let mut size = data.len() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            APPROVED_KEY,
            VALUE_NAME,
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    // First byte is 0x02/0x06 when enabled and 0x03/0x07 when disabled.
    err == ERROR_SUCCESS && size >= 1 && data[0] & 1 == 1
}

fn delete(subkey: PCWSTR) -> io::Result<()> {
    let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey, VALUE_NAME) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(err)
}

fn check(err: WIN32_ERROR) -> io::Result<()> {
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(err.0 as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::command_exe;
    use std::path::PathBuf;

    #[test]
    fn extracts_exe_from_command_lines() {
        assert_eq!(
            command_exe(r#""C:\Program Files\A B\AvrcpKeeper.exe" --background"#),
            PathBuf::from(r"C:\Program Files\A B\AvrcpKeeper.exe")
        );
        assert_eq!(command_exe(r"C:\Tools\AvrcpKeeper.exe --background"), PathBuf::from(r"C:\Tools\AvrcpKeeper.exe"));
        assert_eq!(command_exe(r#""C:\x.exe""#), PathBuf::from(r"C:\x.exe"));
    }
}
