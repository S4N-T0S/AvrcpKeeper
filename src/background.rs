//! The background instance: reports a media session as "playing" while the configured trigger
//! is active, so speakers that mute on AVRCP "paused" keep playing call and game audio.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::core::{factory, w, Error, Result, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as SessionStatus,
};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use windows::Win32::Foundation::{E_UNEXPECTED, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Media::Audio::IMMDeviceEnumerator;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{SetEvent, INFINITE};
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetTimer, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT,
    WINDOW_EX_STYLE, WM_APP, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT, WM_TIMER, WNDCLASSW,
    WS_OVERLAPPED,
};
use windows_future::{AsyncStatus, IAsyncOperation};

use crate::config::{self, Config, Devices, Trigger};
use crate::platform::{self, audio, ipc, Com};
use crate::{log, logging};

const APP_ID: &str = "AvrcpKeeper";
const TICK_MS: u32 = 1000;
/// How long to stay active after the trigger app goes quiet, to ride out gaps in a call.
const HOLD_OFF: Duration = Duration::from_secs(20);
const ASSERT_INTERVAL: Duration = Duration::from_secs(3);
const ASSERT_BACKOFF: Duration = Duration::from_secs(60);
const MAX_QUICK_ASSERTS: u32 = 3;
/// Being the current session this long resets the reassert backoff.
const CURRENT_STABLE: Duration = Duration::from_secs(30);
/// Consecutive ticks our session may be missing from the session list before it is rebuilt.
const SELF_MISSING_TICKS: u32 = 5;
const START_RETRY: Duration = Duration::from_secs(5);
const STREAM_RETRY: Duration = Duration::from_secs(30);
const MANAGER_RETRY: Duration = Duration::from_secs(10);
const RESUME_DEBOUNCE: Duration = Duration::from_secs(10);
/// In "any app" mode a stream only counts if it produced sound within this window, so apps
/// that hold a silent stream open forever don't keep AvrcpKeeper active.
const AUDIBLE_WINDOW: Duration = Duration::from_secs(300);
const AUDIBLE_PEAK: f32 = 0.001;
const WM_APP_BUTTON: u32 = WM_APP + 1;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const THUMBNAIL_PNG: &[u8] = include_bytes!("../assets/icon-128.png");
const WINRT_CLASSES: [&str; 5] = [
    "Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager",
    "Windows.Media.SystemMediaTransportControls",
    "Windows.Storage.Streams.InMemoryRandomAccessStream",
    "Windows.Storage.Streams.DataWriter",
    "Windows.Storage.Streams.RandomAccessStreamReference",
];

/// Set while a button press is queued or being handled; further presses are dropped so a flood
/// of events can't back up the window thread.
static BUTTON_PENDING: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

pub fn run(verbose: bool) -> ExitCode {
    match run_inner(verbose) {
        Ok(code) => code,
        Err(e) => {
            log!("Fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(verbose: bool) -> std::result::Result<ExitCode, String> {
    let config_path = config::path_next_to_exe();
    // Taken before reading, so a save that lands while we start up still counts as a change.
    let config_stamp = config::file_stamp(&config_path);
    let config = load_config(&config_path);
    logging::set_enabled(config.log || verbose);

    let _com = Com::init_mta().map_err(|e| e.message())?;
    platform::require_winrt_classes(&WINRT_CLASSES)?;
    let Some(lock) = ipc::BackgroundLock::acquire(&config_path).map_err(|e| e.to_string())? else {
        log!("Another instance is running and did not exit; giving up.");
        return Ok(ExitCode::FAILURE);
    };
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
    }

    log!("Started v{}{}. {}", env!("CARGO_PKG_VERSION"), if verbose { " (verbose)" } else { "" }, describe(&config));

    let hwnd = create_window().map_err(|e| e.message())?;
    let state = State::new(hwnd, lock.applied.0, verbose, config_path, config_stamp, config);
    STATE.with(|s| *s.borrow_mut() = Some(state));
    unsafe {
        SetTimer(Some(hwnd), 1, TICK_MS, None);
    }
    with_state(State::tick);

    let handles = [lock.stop.0, lock.reload.0];
    let mut msg = MSG::default();
    'outer: loop {
        let r = unsafe { MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT) };
        match r.0.wrapping_sub(WAIT_OBJECT_0.0) {
            0 => break,
            1 => with_state(State::reload),
            2 => unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break 'outer;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                    if lock.stop_requested() {
                        break 'outer;
                    }
                }
            },
            _ => {
                log!("Wait failed; exiting.");
                break;
            }
        }
    }

    with_state(|s| s.stop_keeper("exiting"));
    // Release every COM object while COM is still initialized on this thread.
    STATE.with(|s| drop(s.borrow_mut().take()));
    log!("Stopped.");
    Ok(ExitCode::SUCCESS)
}

fn load_config(path: &Path) -> Config {
    Config::load(path).unwrap_or_else(|e| {
        log!("Could not read {} ({e}); using defaults.", path.display());
        Config::default()
    })
}

fn describe(cfg: &Config) -> String {
    let trigger = match cfg.trigger {
        Trigger::Apps => format!("apps ({})", if cfg.apps.is_empty() { "none".into() } else { cfg.apps.join(", ") }),
        Trigger::AnyApp => "any app".into(),
        Trigger::Always => "always".into(),
    };
    let devices = match &cfg.devices {
        Devices::All => "all".to_string(),
        Devices::Selected(ids) => format!("{} selected", ids.len()),
    };
    format!(
        "Trigger: {trigger}; outputs: {devices}; resync: {} ms; silent stream: {}.",
        cfg.resync_ms,
        if cfg.silent_stream { "on" } else { "off" }
    )
}

fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("AvrcpKeeper.Background");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Never shown. The SMTC Win32 interop needs a top-level window to attach the session to.
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("AvrcpKeeper Background"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => with_state(State::tick),
        WM_APP_BUTTON => {
            with_state(|s| s.on_button(SystemMediaTransportControlsButton(wparam.0 as i32)));
            BUTTON_PENDING.store(false, Ordering::Release);
        }
        WM_POWERBROADCAST if wparam.0 == PBT_APMRESUMEAUTOMATIC => with_state(State::on_resume),
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION if wparam.0 != 0 => {
            with_state(|s| s.stop_keeper("session ending"));
            unsafe { PostQuitMessage(0) };
        }
        // Explorer restarted; the media session broker may have gone with it.
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) => with_state(State::on_shell_restart),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(state) = guard.as_mut() {
                f(state);
            }
        }
    });
}

struct Keeper {
    smtc: SystemMediaTransportControls,
    button_token: i64,
    /// The output the silent stream belongs on; None for status only (Always mode).
    device_id: Option<String>,
    stream: Option<audio::SilentStream>,
    stream_retry_at: Option<Instant>,
}

struct State {
    hwnd: HWND,
    /// Owned by the `BackgroundLock`, which outlives the state.
    applied_event: HANDLE,
    verbose: bool,
    config_path: PathBuf,
    config_stamp: config::FileStamp,
    /// A changed stamp seen on the previous tick, waiting to be seen once more before applying.
    pending_stamp: Option<config::FileStamp>,
    config: Config,
    own_pid: u32,
    enumerator: Option<IMMDeviceEnumerator>,
    manager: Option<SessionManager>,
    manager_request: Option<IAsyncOperation<SessionManager>>,
    manager_retry_at: Option<Instant>,
    thumbnail: Option<RandomAccessStreamReference>,
    keeper: Option<Keeper>,
    rebuild: bool,
    retry_start_at: Option<Instant>,
    last_trigger: Option<Instant>,
    audible: HashMap<String, Instant>,
    last_assert: Option<Instant>,
    quick_asserts: u32,
    other_idle_ticks: u32,
    current_since: Option<Instant>,
    self_missing_ticks: u32,
    needs_sync: bool,
    last_resume: Option<Instant>,
    idle_reason: &'static str,
    last_error: String,
    last_current_id: String,
    last_other_id: String,
    session_status: HashMap<String, &'static str>,
}

impl State {
    fn new(
        hwnd: HWND,
        applied_event: HANDLE,
        verbose: bool,
        config_path: PathBuf,
        config_stamp: config::FileStamp,
        config: Config,
    ) -> State {
        State {
            hwnd,
            applied_event,
            verbose,
            config_stamp,
            pending_stamp: None,
            config_path,
            config,
            own_pid: std::process::id(),
            enumerator: None,
            manager: None,
            manager_request: None,
            manager_retry_at: None,
            thumbnail: None,
            keeper: None,
            rebuild: false,
            retry_start_at: None,
            last_trigger: None,
            audible: HashMap::new(),
            last_assert: None,
            quick_asserts: 0,
            other_idle_ticks: 0,
            current_since: None,
            self_missing_ticks: 0,
            needs_sync: false,
            last_resume: None,
            idle_reason: "",
            last_error: String::new(),
            last_current_id: String::new(),
            last_other_id: String::new(),
            session_status: HashMap::new(),
        }
    }

    /// Logs an error unless it repeats the previous one.
    fn log_error(&mut self, msg: String) {
        if msg != self.last_error {
            log!("{msg}");
            self.last_error = msg;
        }
    }

    /// Triggered by the settings window after it saves. Saves replace the file in one step, so
    /// there's no need to wait for it to settle.
    fn reload(&mut self) {
        if self.apply_config_file() {
            self.tick();
        }
    }

    /// Rereads the settings file and confirms through the "applied" event. Returns true if the
    /// settings changed.
    fn apply_config_file(&mut self) -> bool {
        let stamp = config::file_stamp(&self.config_path);
        self.pending_stamp = None;
        let config = match Config::load(&self.config_path) {
            Ok(config) => config,
            Err(e) => {
                // The stamp stays stale, so the next tick tries again.
                self.log_error(format!(
                    "Could not reload {} ({e}); keeping the current settings.",
                    self.config_path.display()
                ));
                return false;
            }
        };
        self.config_stamp = stamp;
        let changed = config != self.config;
        if changed {
            logging::set_enabled(config.log || self.verbose);
            log!("Settings changed. {}", describe(&config));
            self.config = config;
        }
        unsafe {
            let _ = SetEvent(self.applied_event);
        }
        changed
    }

    /// Hand edits apply once the file has looked the same on two ticks in a row, so an editor
    /// that truncates before writing never gets a half-written file applied.
    fn check_config_file(&mut self) {
        let stamp = config::file_stamp(&self.config_path);
        if stamp == self.config_stamp {
            self.pending_stamp = None;
        } else if self.pending_stamp.as_ref() == Some(&stamp) {
            self.apply_config_file();
        } else {
            self.pending_stamp = Some(stamp);
        }
    }

    fn on_resume(&mut self) {
        if self.last_resume.is_some_and(|t| t.elapsed() < RESUME_DEBOUNCE) {
            return;
        }
        self.last_resume = Some(Instant::now());
        log!("Resumed from sleep.");
        self.enumerator = None;
        self.manager = None;
        self.rebuild = true;
        self.tick();
    }

    fn on_shell_restart(&mut self) {
        log!("Shell restarted; reconnecting to media sessions.");
        self.manager = None;
        self.rebuild = true;
    }

    fn tick(&mut self) {
        self.check_config_file();
        self.poll_manager();
        if let Err(e) = self.tick_inner() {
            self.log_error(format!("Audio query failed: {}", e.message()));
            self.enumerator = None;
        }
    }

    /// Requests the session manager without blocking the window thread.
    fn poll_manager(&mut self) {
        if self.manager.is_some() {
            return;
        }
        if let Some(request) = &self.manager_request {
            let status = request.Status();
            if matches!(status, Ok(AsyncStatus::Started)) {
                return;
            }
            let result = match status {
                Ok(AsyncStatus::Completed) => request.GetResults(),
                Ok(_) => Err(Error::new(E_UNEXPECTED, "request did not complete")),
                Err(e) => Err(e),
            };
            self.manager_request = None;
            match result {
                Ok(manager) => self.manager = Some(manager),
                Err(e) => {
                    self.manager_retry_at = Some(Instant::now() + MANAGER_RETRY);
                    self.log_error(format!("Media session manager unavailable: {}", e.message()));
                }
            }
            return;
        }
        if self.manager_retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        match SessionManager::RequestAsync() {
            Ok(request) => self.manager_request = Some(request),
            Err(e) => {
                self.manager_retry_at = Some(Instant::now() + MANAGER_RETRY);
                self.log_error(format!("Media session manager unavailable: {}", e.message()));
            }
        }
    }

    fn tick_inner(&mut self) -> Result<()> {
        let en = match &self.enumerator {
            Some(en) => en.clone(),
            None => self.enumerator.insert(audio::enumerator()?).clone(),
        };
        // A failed query leaves the current state alone rather than reading as "nothing playing".
        let target = self.evaluate(&en)?;

        if let Some(keeper) = &self.keeper {
            let keep = !self.rebuild && matches!(&target, Ok(device) if *device == keeper.device_id);
            if !keep {
                let why = match &target {
                    Err(reason) => {
                        self.idle_reason = reason;
                        *reason
                    }
                    Ok(_) if self.rebuild => "reconnecting",
                    Ok(_) => "switching output",
                };
                self.stop_keeper(why);
            }
        }

        match target {
            Ok(device) if self.keeper.is_none() => {
                if self.retry_start_at.is_none_or(|at| Instant::now() >= at) {
                    self.start_keeper(&en, device);
                }
            }
            Err(reason) if self.keeper.is_none() && reason != self.idle_reason => {
                log!("Idle: {reason}.");
                self.idle_reason = reason;
            }
            _ => {}
        }

        self.maintain_stream(&en);
        if self.keeper.is_some() {
            self.keep_current();
        }
        if self.verbose {
            self.log_session_changes();
        }
        Ok(())
    }

    /// Ok(device for the silent stream, or None for status only) when the session should be
    /// up; Err(reason) when it should not.
    fn evaluate(&mut self, en: &IMMDeviceEnumerator) -> Result<std::result::Result<Option<String>, &'static str>> {
        let connected: Vec<String> =
            audio::connected_output_ids(en)?.into_iter().filter(|id| self.config.wants_device(id)).collect();
        if connected.is_empty() {
            return Ok(Err("no selected output is connected"));
        }
        if self.config.trigger == Trigger::Always {
            // No silent stream here: an endless stream would keep the PC from sleeping.
            return Ok(Ok(None));
        }

        let sessions = audio::app_sessions(en, true, self.own_pid)?;
        let now = Instant::now();
        if self.config.trigger == Trigger::AnyApp {
            for s in sessions.iter().filter(|s| s.peak > AUDIBLE_PEAK) {
                self.audible.insert(s.app.clone(), now);
            }
            self.audible.retain(|_, heard| now - *heard < AUDIBLE_WINDOW);
        }
        let current = self.keeper.as_ref().and_then(|k| k.device_id.clone());
        let hits: Vec<String> = sessions
            .into_iter()
            .filter(|s| connected.contains(&s.device_id))
            .filter(|s| match self.config.trigger {
                Trigger::Apps => self.config.wants_app(&s.app),
                _ => self.audible.contains_key(&s.app),
            })
            .map(|s| s.device_id)
            .collect();
        if let Some(first) = hits.first() {
            self.last_trigger = Some(now);
            let device = current.filter(|c| hits.contains(c)).unwrap_or_else(|| first.clone());
            return Ok(Ok(Some(device)));
        }

        let recent = self.last_trigger.is_some_and(|t| now - t < HOLD_OFF);
        Ok(match current {
            Some(device) if recent && connected.contains(&device) => Ok(Some(device)),
            _ if self.config.trigger == Trigger::Apps => Err("waiting for a selected app to play audio"),
            _ => Err("waiting for an app to play audio"),
        })
    }

    fn start_keeper(&mut self, en: &IMMDeviceEnumerator, device_id: Option<String>) {
        if self.thumbnail.is_none() {
            self.thumbnail = thumbnail().ok();
        }
        match create_session(self.hwnd, self.thumbnail.as_ref()) {
            Ok((smtc, button_token)) => {
                let mut keeper = Keeper { smtc, button_token, device_id, stream: None, stream_retry_at: None };
                match &keeper.device_id {
                    Some(id) if self.config.silent_stream => {
                        let name = audio::device_name(en, id).unwrap_or_else(|| id.clone());
                        match audio::SilentStream::start(en, id) {
                            Ok(stream) => {
                                keeper.stream = Some(stream);
                                log!("Active, silent stream on {name}.");
                            }
                            Err(e) => {
                                keeper.stream_retry_at = Some(Instant::now() + STREAM_RETRY);
                                log!("Active on {name} without a silent stream ({}).", e.message());
                            }
                        }
                    }
                    Some(id) => log!("Active on {}.", audio::device_name(en, id).unwrap_or_else(|| id.clone())),
                    None => log!("Active."),
                }
                self.keeper = Some(keeper);
                self.rebuild = false;
                self.retry_start_at = None;
                // The current session usually hasn't switched to us yet; give it a moment
                // before considering a reassert.
                self.last_assert = Some(Instant::now());
                self.quick_asserts = 0;
                self.other_idle_ticks = 0;
                self.current_since = None;
                self.self_missing_ticks = 0;
                self.needs_sync = true;
                self.idle_reason = "";
                self.last_current_id.clear();
            }
            Err(e) => {
                self.retry_start_at = Some(Instant::now() + START_RETRY);
                self.log_error(format!("Could not start: {}", e.message()));
            }
        }
    }

    fn stop_keeper(&mut self, why: &str) {
        if let Some(keeper) = self.keeper.take() {
            let _ = keeper.smtc.RemoveButtonPressed(keeper.button_token);
            let _ = keeper.smtc.SetPlaybackStatus(MediaPlaybackStatus::Stopped);
            let _ = keeper.smtc.SetIsEnabled(false);
            drop(keeper);
            log!("Inactive: {why}.");
        }
    }

    /// Keeps the silent stream topped up, and recreates it if it fails, without touching the
    /// media session (so the speaker never sees "stopped" for a stream hiccup). Also starts or
    /// stops it when the setting changes.
    fn maintain_stream(&mut self, en: &IMMDeviceEnumerator) {
        let Some(keeper) = &mut self.keeper else { return };
        let Some(device) = keeper.device_id.clone() else { return };
        if !self.config.silent_stream {
            keeper.stream_retry_at = None;
            if keeper.stream.take().is_some() {
                log!("Silent stream stopped.");
            }
            return;
        }
        let now = Instant::now();
        if let Some(stream) = &keeper.stream {
            if stream.refill().is_err() {
                keeper.stream = None;
                keeper.stream_retry_at = Some(now + Duration::from_secs(2));
                log!("Silent stream lost; recreating it.");
            }
            return;
        }
        if keeper.stream_retry_at.is_some_and(|t| now < t) {
            return;
        }
        match audio::SilentStream::start(en, &device) {
            Ok(stream) => {
                keeper.stream = Some(stream);
                keeper.stream_retry_at = None;
                log!("Silent stream started.");
            }
            Err(e) => {
                keeper.stream_retry_at = Some(now + STREAM_RETRY);
                log!("Silent stream unavailable ({}); retrying later.", e.message());
            }
        }
    }

    fn keep_current(&mut self) {
        let Some(manager) = self.manager.clone() else { return };
        if let Err(e) = self.keep_current_inner(&manager) {
            self.log_error(format!("Media session query failed: {}", e.message()));
            self.manager = None;
            self.rebuild = true;
        }
    }

    /// The speaker follows the current session, so when another app is current but not
    /// playing, flip our status to make Windows pick us again.
    fn keep_current_inner(&mut self, manager: &SessionManager) -> Result<()> {
        let current = match manager.GetCurrentSession() {
            Ok(session) => Some(session),
            // windows-rs reports a null result as an error with a success code.
            Err(e) if e.code().is_ok() => None,
            Err(e) => return Err(e),
        };
        if manager.GetSessions()?.into_iter().any(|s| is_self(&session_id(&s))) {
            self.self_missing_ticks = 0;
        } else {
            self.self_missing_ticks += 1;
            if self.self_missing_ticks > SELF_MISSING_TICKS {
                return Err(Error::new(E_UNEXPECTED, "AvrcpKeeper's media session disappeared"));
            }
        }

        let current_id = current.as_ref().map(session_id).unwrap_or_default();
        if current_id != self.last_current_id {
            if self.verbose {
                log!("Current media session: {}", if current_id.is_empty() { "(none)" } else { &current_id });
            }
            self.last_current_id = current_id.clone();
        }

        let now = Instant::now();
        if is_self(&current_id) {
            self.other_idle_ticks = 0;
            let since = *self.current_since.get_or_insert(now);
            if now - since >= CURRENT_STABLE {
                self.quick_asserts = 0;
            }
            // The speaker may still think it is paused from presses made while we weren't
            // running; one blip puts it back in sync.
            if self.needs_sync && self.find_playing_other().is_none() {
                self.needs_sync = false;
                self.blip();
            }
            return Ok(());
        }
        self.current_since = None;
        if let Some(session) = &current {
            self.last_other_id = current_id;
            if is_busy(session) {
                self.other_idle_ticks = 0;
                return Ok(());
            }
        }
        // Wait for a second idle tick so a player switching tracks isn't mistaken for paused.
        self.other_idle_ticks += 1;
        if self.other_idle_ticks < 2 {
            return Ok(());
        }

        // Back off if flipping doesn't make us current, rather than blipping every few seconds.
        let wait = if self.quick_asserts < MAX_QUICK_ASSERTS { ASSERT_INTERVAL } else { ASSERT_BACKOFF };
        if self.last_assert.is_some_and(|t| now - t < wait) {
            return Ok(());
        }
        self.last_assert = Some(now);
        self.quick_asserts += 1;
        if self.verbose {
            log!("Current session not playing; reasserting (attempt {}).", self.quick_asserts);
        }
        self.blip();
        Ok(())
    }

    /// Reports paused briefly, then playing. Some speakers track play state themselves and
    /// only resync on a status change; without this their play/pause button sends "play"
    /// to a session that is already playing, and Windows drops it.
    fn blip(&self) {
        if let Some(keeper) = &self.keeper {
            let _ = keeper.smtc.SetPlaybackStatus(MediaPlaybackStatus::Paused);
            std::thread::sleep(Duration::from_millis(u64::from(self.config.resync_ms)));
            let _ = keeper.smtc.SetPlaybackStatus(MediaPlaybackStatus::Playing);
        }
    }

    /// Media buttons reach us while we are the current session; forward them to the player
    /// the user means.
    fn on_button(&mut self, button: SystemMediaTransportControlsButton) {
        if self.keeper.is_none() {
            return;
        }
        let target = self.find_other_session();
        if self.verbose {
            let name = target.as_ref().map(session_id).unwrap_or_else(|| "(no other session)".into());
            log!("Button {} -> {name}", button.0);
        }
        if button == SystemMediaTransportControlsButton::Play || button == SystemMediaTransportControlsButton::Pause {
            self.play_pause(target.as_ref());
        } else if let Some(session) = &target {
            if button == SystemMediaTransportControlsButton::Next {
                let _ = session.TrySkipNextAsync();
            } else if button == SystemMediaTransportControlsButton::Previous {
                let _ = session.TrySkipPreviousAsync();
            }
        }
    }

    /// Acts on the target's real state, and blips only while nothing else is playing, since
    /// Windows would otherwise make the other session current and the blip would be lost.
    fn play_pause(&self, target: Option<&Session>) {
        match target {
            Some(session) if is_playing(session) => {
                let _ = session.TryPauseAsync();
                for _ in 0..15 {
                    if !is_playing(session) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                self.blip();
            }
            _ => {
                self.blip();
                if let Some(session) = target {
                    let _ = session.TryPlayAsync();
                }
            }
        }
    }

    fn find_other_session(&self) -> Option<Session> {
        let sessions = self.manager.as_ref()?.GetSessions().ok()?;
        let mut fallback = None;
        for session in sessions {
            let id = session_id(&session);
            if is_self(&id) {
                continue;
            }
            if id == self.last_other_id {
                return Some(session);
            }
            fallback.get_or_insert(session);
        }
        fallback
    }

    fn find_playing_other(&self) -> Option<Session> {
        let sessions = self.manager.as_ref()?.GetSessions().ok()?;
        sessions.into_iter().find(|s| !is_self(&session_id(s)) && is_playing(s))
    }

    /// Verbose only. Sessions sharing an app ID (several browser windows) are merged.
    fn log_session_changes(&mut self) {
        let Some(sessions) = self.manager.as_ref().and_then(|m| m.GetSessions().ok()) else { return };
        let mut now: HashMap<String, &'static str> = HashMap::new();
        for session in sessions {
            let status = status_name(&session);
            now.entry(session_id(&session))
                .and_modify(|s| {
                    if status == "playing" {
                        *s = status;
                    }
                })
                .or_insert(status);
        }
        for (id, status) in &now {
            if self.session_status.get(id) != Some(status) {
                log!("Session {id} -> {status}");
            }
        }
        for id in self.session_status.keys().filter(|id| !now.contains_key(*id)) {
            log!("Session {id} closed");
        }
        self.session_status = now;
    }
}

/// Registers and configures the media session for `hwnd`. Everything is undone if a step
/// fails, so a retry never stacks a second button handler on the same window session.
fn create_session(
    hwnd: HWND,
    thumbnail: Option<&RandomAccessStreamReference>,
) -> Result<(SystemMediaTransportControls, i64)> {
    let interop = factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
    let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
    let configure = || -> Result<i64> {
        smtc.SetIsEnabled(true)?;
        smtc.SetIsPlayEnabled(true)?;
        smtc.SetIsPauseEnabled(true)?;
        smtc.SetIsNextEnabled(true)?;
        smtc.SetIsPreviousEnabled(true)?;
        let display = smtc.DisplayUpdater()?;
        display.SetType(MediaPlaybackType::Music)?;
        let music = display.MusicProperties()?;
        music.SetTitle(&"Keeping speaker active".into())?;
        music.SetArtist(&APP_ID.into())?;
        if let Some(thumbnail) = thumbnail {
            let _ = display.SetThumbnail(thumbnail);
        }
        display.Update()?;

        // Button events arrive on a WinRT thread; hand them to the window thread.
        let target = hwnd.0 as isize;
        let token = smtc.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = &*args {
                let button = args.Button()?;
                if !BUTTON_PENDING.swap(true, Ordering::AcqRel) {
                    let posted = unsafe {
                        PostMessageW(Some(HWND(target as _)), WM_APP_BUTTON, WPARAM(button.0 as usize), LPARAM(0))
                    };
                    if posted.is_err() {
                        BUTTON_PENDING.store(false, Ordering::Release);
                    }
                }
            }
            Ok(())
        }))?;
        if let Err(e) = smtc.SetPlaybackStatus(MediaPlaybackStatus::Playing) {
            let _ = smtc.RemoveButtonPressed(token);
            return Err(e);
        }
        Ok(token)
    };
    match configure() {
        Ok(token) => Ok((smtc, token)),
        Err(e) => {
            let _ = smtc.SetIsEnabled(false);
            Err(e)
        }
    }
}

fn thumbnail() -> Result<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(THUMBNAIL_PNG)?;
    writer.StoreAsync()?.get()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    RandomAccessStreamReference::CreateFromStream(&stream)
}

fn session_id(session: &Session) -> String {
    session.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default()
}

fn is_self(id: &str) -> bool {
    id.eq_ignore_ascii_case(APP_ID) || id.eq_ignore_ascii_case("AvrcpKeeper.exe")
}

fn playback_status(session: &Session) -> Option<SessionStatus> {
    session.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).ok()
}

fn is_playing(session: &Session) -> bool {
    playback_status(session) == Some(SessionStatus::Playing)
}

/// Playing, or between tracks.
fn is_busy(session: &Session) -> bool {
    matches!(playback_status(session), Some(SessionStatus::Playing | SessionStatus::Changing))
}

fn status_name(session: &Session) -> &'static str {
    match playback_status(session) {
        Some(SessionStatus::Closed) => "closed",
        Some(SessionStatus::Opened) => "opened",
        Some(SessionStatus::Changing) => "changing",
        Some(SessionStatus::Stopped) => "stopped",
        Some(SessionStatus::Playing) => "playing",
        Some(SessionStatus::Paused) => "paused",
        _ => "unknown",
    }
}
