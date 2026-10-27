use std::fs::OpenOptions;
use std::io::Write;

use windows::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};

/// Prints for a GUI-subsystem executable: uses redirected stdout if there is one, otherwise
/// attaches to the console of the shell that started us.
pub fn print(text: &str) {
    let redirected = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if redirected {
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        return;
    }
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        if let Ok(mut con) = OpenOptions::new().write(true).open("CONOUT$") {
            let _ = con.write_all(text.as_bytes());
        }
    }
}
