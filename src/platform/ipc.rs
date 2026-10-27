//! Named kernel objects shared by the background instance and the settings window. They live
//! in the session-local namespace and are created with a private security descriptor.

use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, ReleaseMutex, ResetEvent, SetEvent, WaitForSingleObject,
    CREATE_BREAKAWAY_FROM_JOB, EVENT_MODIFY_STATE, MUTEX_MODIFY_STATE, SYNCHRONIZATION_READ_CONTROL,
    SYNCHRONIZATION_SYNCHRONIZE,
};

use super::security::{self, ObjectAttributes};

const BACKGROUND_MUTEX: PCWSTR = w!(r"Local\AvrcpKeeper.Background");
const SETTINGS_MUTEX: PCWSTR = w!(r"Local\AvrcpKeeper.Settings");
const STOP_EVENT: PCWSTR = w!(r"Local\AvrcpKeeper.Stop");
const RELOAD_EVENT: PCWSTR = w!(r"Local\AvrcpKeeper.Reload");

/// One "applied" event per settings file, so a copy running from another folder (which reads
/// its own settings file) can't confirm changes made here. The folder is canonicalized so a
/// junction, subst drive or short path to the same folder gives the same name.
fn applied_event_name(config_path: &Path) -> HSTRING {
    let canonical = config_path
        .parent()
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .zip(config_path.file_name())
        .map(|(dir, name)| dir.join(name))
        .unwrap_or_else(|| config_path.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    HSTRING::from(format!(r"Local\AvrcpKeeper.Applied.{hash:016x}"))
}

pub struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Debug)]
pub enum IpcError {
    Os(windows::core::Error),
    /// An object with our name was created by a lower-integrity process.
    Untrusted,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Os(e) => write!(f, "{}", e.message()),
            IpcError::Untrusted => f.write_str("a lower-integrity process is holding AvrcpKeeper's named objects"),
        }
    }
}

enum Kind {
    Mutex,
    Event,
}

fn create(kind: Kind, name: PCWSTR) -> Result<OwnedHandle, IpcError> {
    let attributes = ObjectAttributes::private();
    let sa = attributes.as_ref().map(|a| a.as_ptr());
    let handle = unsafe {
        match kind {
            Kind::Mutex => CreateMutexW(sa, false, name),
            Kind::Event => CreateEventW(sa, false, false, name),
        }
    }
    .map_err(IpcError::Os)?;
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = OwnedHandle(handle);
    if existed && !security::existing_object_trusted(handle.0) {
        return Err(IpcError::Untrusted);
    }
    Ok(handle)
}

fn wait_owned(mutex: HANDLE, timeout_ms: u32) -> bool {
    let r = unsafe { WaitForSingleObject(mutex, timeout_ms) };
    r == WAIT_OBJECT_0 || r == WAIT_ABANDONED
}

/// Held by the background instance for its whole lifetime.
pub struct BackgroundLock {
    mutex: OwnedHandle,
    pub stop: OwnedHandle,
    pub reload: OwnedHandle,
    /// Set after each settings reload so the settings window can confirm the change took effect.
    pub applied: OwnedHandle,
}

impl BackgroundLock {
    /// Takes over from an already running instance by asking it to exit first.
    /// Returns `Ok(None)` if it doesn't exit in time.
    pub fn acquire(config_path: &Path) -> Result<Option<BackgroundLock>, IpcError> {
        let stop = create(Kind::Event, STOP_EVENT)?;
        let reload = create(Kind::Event, RELOAD_EVENT)?;
        let applied_name = applied_event_name(config_path);
        let applied = create(Kind::Event, PCWSTR(applied_name.as_ptr()))?;
        let mutex = create(Kind::Mutex, BACKGROUND_MUTEX)?;
        if !wait_owned(mutex.0, 0) {
            unsafe {
                let _ = SetEvent(stop.0);
            }
            if !wait_owned(mutex.0, 5000) {
                return Ok(None);
            }
        }
        unsafe {
            let _ = ResetEvent(stop.0);
            let _ = ResetEvent(reload.0);
        }
        Ok(Some(BackgroundLock { mutex, stop, reload, applied }))
    }

    pub fn stop_requested(&self) -> bool {
        unsafe { WaitForSingleObject(self.stop.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for BackgroundLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.mutex.0);
        }
    }
}

/// Single settings window per session.
pub struct SettingsLock(OwnedHandle);

impl SettingsLock {
    /// `Ok(None)` when another settings window already holds it.
    pub fn acquire() -> Result<Option<SettingsLock>, IpcError> {
        let mutex = create(Kind::Mutex, SETTINGS_MUTEX)?;
        Ok(wait_owned(mutex.0, 0).then_some(SettingsLock(mutex)))
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex((self.0).0);
        }
    }
}

/// The settings window's view of the background instance's "applied" confirmation.
pub struct AppliedSignal {
    name: HSTRING,
    handle: Option<OwnedHandle>,
}

impl AppliedSignal {
    pub fn new(config_path: &Path) -> AppliedSignal {
        AppliedSignal { name: applied_event_name(config_path), handle: None }
    }

    fn handle(&mut self) -> Option<HANDLE> {
        if self.handle.is_none() {
            let name = PCWSTR(self.name.as_ptr());
            let access = SYNCHRONIZATION_SYNCHRONIZE | SYNCHRONIZATION_READ_CONTROL;
            // Only trust an event the background instance could have created, not one squatted
            // by another account or a lower-integrity process.
            self.handle = unsafe { OpenEventW(access, false, name) }
                .ok()
                .map(OwnedHandle)
                .filter(|h| security::existing_object_trusted(h.0));
        }
        self.handle.as_ref().map(|h| h.0)
    }

    /// False while the running instance belongs to a copy that uses a different settings file.
    pub fn available(&mut self) -> bool {
        self.handle().is_some()
    }

    /// Forgets the open handle, so the next check sees whichever instance runs next.
    pub fn reset(&mut self) {
        self.handle = None;
    }

    /// Drops a confirmation left over from an earlier reload.
    pub fn clear(&mut self) {
        if let Some(h) = self.handle() {
            unsafe { WaitForSingleObject(h, 0) };
        }
    }

    /// True once for each reload the background instance has confirmed since the last call.
    pub fn received(&mut self) -> bool {
        self.handle().is_some_and(|h| unsafe { WaitForSingleObject(h, 0) } == WAIT_OBJECT_0)
    }
}

/// True while some process owns the background mutex. A process merely holding a handle to it
/// doesn't count.
pub fn background_running() -> bool {
    let access = SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE;
    let Ok(mutex) = (unsafe { OpenMutexW(access, false, BACKGROUND_MUTEX) }) else { return false };
    let mutex = OwnedHandle(mutex);
    if wait_owned(mutex.0, 0) {
        unsafe {
            let _ = ReleaseMutex(mutex.0);
        }
        return false;
    }
    true
}

pub fn request_stop() -> bool {
    signal(STOP_EVENT)
}

/// Does nothing if no background instance is running.
pub fn request_reload() -> bool {
    signal(RELOAD_EVENT)
}

fn signal(name: PCWSTR) -> bool {
    match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, name) } {
        Ok(h) => {
            let h = OwnedHandle(h);
            unsafe { SetEvent(h.0) }.is_ok()
        }
        Err(_) => false,
    }
}

pub fn wait_until_stopped(timeout: Duration) -> bool {
    let start = Instant::now();
    while background_running() {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

pub fn start_background() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let spawn = |flags: u32| {
        Command::new(&exe)
            .arg("--background")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
            .map(drop)
    };
    // Leave the settings window's job object (if any) so closing it can't take the
    // background instance down with it. Not every job allows that, so fall back.
    spawn(CREATE_BREAKAWAY_FROM_JOB.0).or_else(|_| spawn(0))
}
