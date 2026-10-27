use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_CLOAKED,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, EnumWindows, FindWindowExW, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, LoadImageW, MessageBoxW, SendMessageW, SetForegroundWindow,
    ShowWindow, GWL_EXSTYLE, GW_OWNER, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, MB_ICONERROR, MB_OK,
    SM_CXICON, SM_CXSMICON, SW_RESTORE, WM_SETICON, WS_EX_TOOLWINDOW,
};

/// Shell and system processes that own windows but aren't apps anyone would pick.
const NOT_APPS: [&str; 9] = [
    "applicationframehost",
    "avrcpkeeper",
    "explorer",
    "lockapp",
    "searchhost",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "systemsettings",
    "textinputhost",
];

/// Lowercase executable names of running apps with a normal top-level window, roughly what the
/// taskbar shows: visible, not owned by another window, not a tool window, not cloaked.
pub fn windowed_apps() -> Vec<String> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let pids = &mut *(lparam.0 as *mut Vec<u32>);
        let cloaked = || {
            let mut value = 0u32;
            let size = size_of::<u32>() as u32;
            DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut value as *mut u32 as *mut c_void, size).is_ok()
                && value != 0
        };
        // Cheapest checks first; most top-level windows are hidden.
        let listed = IsWindowVisible(hwnd).as_bool()
            && GetWindow(hwnd, GW_OWNER).is_err()
            && GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 == 0
            && GetWindowTextLengthW(hwnd) > 0
            && !cloaked();
        if listed {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != 0 {
                pids.push(pid);
            }
        }
        true.into()
    }

    let mut pids: Vec<u32> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut pids as *mut Vec<u32> as isize));
    }
    let own = std::process::id();
    let mut names: HashMap<u32, Option<String>> = HashMap::new();
    let mut apps: Vec<String> = pids
        .into_iter()
        .filter(|&pid| pid != own)
        .filter_map(|pid| {
            names
                .entry(pid)
                .or_insert_with(|| {
                    let path = super::process_path(pid)?;
                    Some(path.file_stem()?.to_string_lossy().to_lowercase())
                })
                .clone()
        })
        .filter(|name| !name.is_empty() && !NOT_APPS.contains(&name.as_str()))
        .collect();
    apps.sort();
    apps.dedup();
    apps
}

/// Brings an already open settings window to the front. Only windows that belong to a process
/// running this same executable are considered, so an Explorer window on a folder named
/// "AvrcpKeeper" isn't picked by mistake.
pub fn focus_existing(title: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let title = HSTRING::from(title);
    let mut after = None;
    for _ in 0..64 {
        let Ok(hwnd) = (unsafe { FindWindowExW(None, after, PCWSTR::null(), &title) }) else { return false };
        after = Some(hwnd);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == std::process::id() || !super::process_path(pid).is_some_and(|p| super::same_path(&p, &exe)) {
            continue;
        }
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            return SetForegroundWindow(hwnd).as_bool();
        }
    }
    false
}

pub fn error_box(text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("AvrcpKeeper"), MB_OK | MB_ICONERROR);
    }
}

/// Gives the window on this thread titled `title` a dark title bar, a matching caption color and
/// the embedded icon. Returns false if the window doesn't exist yet.
pub fn style_own_window(title: &str, caption_rgb: [u8; 3]) -> bool {
    let Some(hwnd) = find_thread_window(title) else { return false };
    unsafe {
        let dark: BOOL = TRUE;
        set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark);
        let color =
            COLORREF(u32::from(caption_rgb[0]) | u32::from(caption_rgb[1]) << 8 | u32::from(caption_rgb[2]) << 16);
        // Windows 11 only; older versions ignore these.
        set_attr(hwnd, DWMWA_CAPTION_COLOR, &color);
        set_attr(hwnd, DWMWA_BORDER_COLOR, &color);
        set_icons(hwnd);
    }
    true
}

unsafe fn set_attr<T>(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: &T) {
    let _ = DwmSetWindowAttribute(hwnd, attr, value as *const T as *const c_void, size_of::<T>() as u32);
}

unsafe fn set_icons(hwnd: HWND) {
    let Ok(module) = GetModuleHandleW(None) else { return };
    let dpi = GetDpiForWindow(hwnd);
    for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
        let size = GetSystemMetricsForDpi(metric, dpi);
        // Resource ID 1 is the application icon (see build.rs).
        if let Ok(icon) = LoadImageW(Some(module.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
        }
    }
}

fn find_thread_window(title: &str) -> Option<HWND> {
    struct Search<'a> {
        title: &'a str,
        found: Option<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == search.title {
            search.found = Some(hwnd);
            return false.into();
        }
        true.into()
    }

    let mut search = Search { title, found: None };
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}
