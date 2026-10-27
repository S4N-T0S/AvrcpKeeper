use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const FILE_NAME: &str = "AvrcpKeeper.log";
/// When the log passes this size it replaces AvrcpKeeper.log.old, so at most two files exist.
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINE_CHARS: usize = 2000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

static PATH: OnceLock<PathBuf> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static LOCK: Mutex<()> = Mutex::new(());

/// Sets where the log goes. Nothing is written until `set_enabled(true)`.
pub fn init(dir: &Path) {
    let _ = PATH.set(dir.join(FILE_NAME));
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if $crate::logging::enabled() {
            $crate::logging::write(&format!($($arg)*));
        }
    };
}

pub fn write(msg: &str) {
    if !enabled() {
        return;
    }
    let Some(path) = PATH.get() else { return };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match fs::symlink_metadata(path) {
        // Never write through a link someone placed where the log should be.
        Ok(meta) if meta.file_type().is_symlink() => return,
        Ok(meta) if meta.len() > MAX_BYTES => rotate(path),
        _ => {}
    }
    let line = format!("{}  {}\r\n", crate::platform::local_timestamp(), sanitize(msg));
    let file = OpenOptions::new().create(true).append(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path);
    if let Ok(mut f) = file {
        // One write per line keeps lines from the GUI and background processes intact.
        let _ = f.write_all(line.as_bytes());
    }
}

fn rotate(path: &Path) {
    if fs::rename(path, path.with_extension("log.old")).is_err() {
        // Someone has the file open without delete sharing; start over rather than grow forever.
        let _ = OpenOptions::new().write(true).truncate(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path);
    }
}

/// Session IDs, process and device names come from other programs; keep them on one line and
/// free of characters that change how the log displays.
fn sanitize(msg: &str) -> String {
    msg.chars()
        .filter(|c| !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if c.is_control() || c == '\u{2028}' || c == '\u{2029}' { ' ' } else { c })
        .take(MAX_LINE_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_and_bidi_characters() {
        assert_eq!(sanitize("a\r\nb\tc\u{1b}[31m"), "a  b c [31m");
        assert_eq!(sanitize("x\u{202E}y\u{2066}z\u{2028}w"), "xyz w");
        assert_eq!(sanitize(&"a".repeat(5000)).len(), MAX_LINE_CHARS);
    }
}
