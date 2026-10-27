mod theme;
mod widgets;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Margin, RichText, Stroke, TextureHandle, Ui};
use windows::Win32::Media::Audio::IMMDeviceEnumerator;

use crate::config::{self, Config, Devices, Trigger};
use crate::platform::{self, audio, autostart, ipc, window};
use crate::{log, logging};

const TITLE: &str = "AvrcpKeeper";
const ICON_RGBA: &[u8] = include_bytes!("../../assets/icon-128.rgba");
const ICON_SIZE: usize = 128;
const SAVE_DELAY: Duration = Duration::from_millis(400);
const SAVE_RETRY: Duration = Duration::from_secs(2);
const POLL_EVERY: Duration = Duration::from_secs(1);
const DEVICES_EVERY: Duration = Duration::from_secs(3);
const PENDING_TIMEOUT: Duration = Duration::from_secs(6);
/// Starting a Bluetooth stream during a tick can take a second or two, so allow some slack.
const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Confirmation usually arrives within a frame; don't flash "Applying" for that.
const APPLYING_SHOW_AFTER: Duration = Duration::from_millis(300);
const NOTICE_SHOW: Duration = Duration::from_secs(3);
const RESYNC_UI_MAX: u32 = 1000;
const CREDIT_URL: &str = "https://s4nt0s.eu";

const HELP_RESYNC: &str = "Some Bluetooth speakers keep track of play/pause themselves. When you press pause on \
the speaker, it expects to be told that playback paused, but AvrcpKeeper keeps saying \"playing\" so the speaker \
doesn't mute. The speaker then gets out of sync and its play/pause button stops working after the first press.\n\n\
To avoid that, AvrcpKeeper briefly reports \"paused\" after each press and then \"playing\" again. This sets how long \
that moment lasts. Call audio may dip during it. Shorter feels snappier; if the button still stops responding, raise \
it.";
const HELP_SILENT: &str = "By default AvrcpKeeper only tells the speaker that something is playing, which is usually \
enough. Turning this on also plays silence to the speaker while AvrcpKeeper is active. Try it if the speaker still \
mutes during calls. Some speakers click or pop when the silence starts and stops.\n\n\
Not used with Always, so your PC can still sleep.";
const HELP_TRIGGER: &str = "AvrcpKeeper only tells the speaker that something is playing while this is true. \
The rest of the time your speaker and media keys behave as usual.";
const HELP_DEVICES: &str = "AvrcpKeeper is only active on these outputs. Bluetooth speakers stay listed while \
they're disconnected, so you can pick yours ahead of time.";

pub fn run() -> ExitCode {
    logging::set_enabled(Config::load(&config::path_next_to_exe()).is_ok_and(|c| c.log));
    let _lock = match ipc::SettingsLock::acquire() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            window::focus_existing(TITLE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            log!("Settings window blocked: {e}");
            window::error_box(&format!("The settings window could not be opened: {e}."));
            return ExitCode::FAILURE;
        }
    };
    // If WGL setup fails, glutin falls back to loading libEGL.dll by name, which searches the
    // exe's folder. Refuse to run next to one.
    if platform::exe_dir().join("libEGL.dll").exists() {
        window::error_box("A file named libEGL.dll is next to AvrcpKeeper.exe. Move AvrcpKeeper to its own folder.");
        return ExitCode::FAILURE;
    }
    let _com = platform::Com::init_sta().ok();
    let icon = egui::IconData { rgba: ICON_RGBA.to_vec(), width: ICON_SIZE as u32, height: ICON_SIZE as u32 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_app_id(TITLE)
            .with_inner_size([480.0, 740.0])
            .with_min_inner_size([420.0, 460.0])
            .with_icon(Arc::new(icon)),
        centered: true,
        ..Default::default()
    };
    match eframe::run_native(TITLE, options, Box::new(|cc| Ok(Box::new(SettingsApp::new(&cc.egui_ctx))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log!("Settings window failed: {e}");
            window::error_box(&format!("The settings window could not be opened.\n\n{e}"));
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    Start,
    Stop,
}

/// What the footer says about the last save.
#[derive(Clone, Copy, PartialEq)]
enum Notice {
    Idle,
    Saved(Instant),
    /// Saved and waiting for the running instance to confirm it reloaded.
    Applying(Instant),
    Applied(Instant),
    NotApplied,
    Restarting,
}

struct SettingsApp {
    config_path: PathBuf,
    config: Config,
    config_stamp: config::FileStamp,
    pending_stamp: Option<config::FileStamp>,
    dirty_at: Option<Instant>,
    error: Option<String>,
    startup: autostart::Startup,
    location_warning: Option<PathBuf>,
    running: bool,
    pending: Option<(Pending, Instant)>,
    restart_after_stop: bool,
    applied: ipc::AppliedSignal,
    notice: Notice,
    /// The running instance is a copy from another folder, which reads a different settings file.
    other_copy: bool,
    enumerator: Option<IMMDeviceEnumerator>,
    devices: Vec<audio::OutputDevice>,
    /// Apps that currently have an audio session, for the "Open apps" list.
    audio_apps: Vec<String>,
    /// Other running apps with a window, listed below the audio ones.
    window_apps: Vec<String>,
    last_poll: Option<Instant>,
    last_devices: Option<Instant>,
    new_app: String,
    add_feedback: Option<&'static str>,
    icon: TextureHandle,
    window_styled: u32,
}

impl SettingsApp {
    fn new(ctx: &egui::Context) -> SettingsApp {
        theme::apply(ctx);
        let config_path = config::path_next_to_exe();
        let config_stamp = config::file_stamp(&config_path);
        let applied = ipc::AppliedSignal::new(&config_path);
        let (config, error) = match Config::load(&config_path) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("Couldn't read {}: {e}", config_path.display()))),
        };
        let image = egui::ColorImage::from_rgba_unmultiplied([ICON_SIZE, ICON_SIZE], ICON_RGBA);
        let options =
            egui::TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..egui::TextureOptions::LINEAR };
        let icon = ctx.load_texture("app-icon", image, options);
        let mut app = SettingsApp {
            config_path,
            config,
            config_stamp,
            pending_stamp: None,
            dirty_at: None,
            error,
            startup: autostart::state(),
            location_warning: std::env::current_exe().ok().and_then(|exe| platform::security::unsafe_location(&exe)),
            running: ipc::background_running(),
            pending: None,
            restart_after_stop: false,
            applied,
            notice: Notice::Idle,
            other_copy: false,
            enumerator: audio::enumerator().ok(),
            devices: Vec::new(),
            audio_apps: Vec::new(),
            window_apps: Vec::new(),
            last_poll: None,
            last_devices: None,
            new_app: String::new(),
            add_feedback: None,
            icon,
            window_styled: 0,
        };
        app.refresh_devices();
        app
    }

    fn mark_dirty(&mut self) {
        self.dirty_at = Some(Instant::now());
    }

    fn save_now(&mut self) -> bool {
        match self.config.save(&self.config_path) {
            Ok(stamp) => {
                self.dirty_at = None;
                self.error = None;
                self.config_stamp = stamp;
                self.pending_stamp = None;
                if self.running && !self.other_copy {
                    self.applied.clear();
                    self.notice = Notice::Applying(Instant::now());
                } else {
                    self.notice = Notice::Saved(Instant::now());
                }
                // Sent even when `running` says no, in case an instance has only just started.
                ipc::request_reload();
                true
            }
            Err(e) => {
                log!("Saving settings failed: {e}");
                // Often a brief lock by antivirus or a sync client; try again shortly.
                self.dirty_at = Some(Instant::now() + SAVE_RETRY);
                self.error = Some(if e.kind() == io::ErrorKind::PermissionDenied {
                    "Couldn't save settings next to AvrcpKeeper.exe (access denied). If this keeps happening, move \
                     AvrcpKeeper to a folder you can write to."
                        .to_string()
                } else {
                    format!("Couldn't save settings to {}: {e}", self.config_path.display())
                });
                false
            }
        }
    }

    fn poll(&mut self) {
        let now = Instant::now();
        // `since` values can be newer than `now` once save_now has run, so never subtract directly.
        let age = |since: Instant| now.saturating_duration_since(since);
        if self.dirty_at.is_some_and(|t| now >= t + SAVE_DELAY) {
            self.save_now();
        }
        if self.last_poll.is_none_or(|t| age(t) >= POLL_EVERY) {
            self.last_poll = Some(now);
            self.running = ipc::background_running();
            // Reopen every time: a cached handle keeps an old instance's event alive and would hide
            // a quick takeover by a copy from another folder. The signaled state isn't lost, since
            // the background instance holds its own handle.
            self.applied.reset();
            self.other_copy = self.running && !self.applied.available();
            self.startup = autostart::state();
            self.pick_up_external_edits();
            if let Some((kind, since)) = self.pending {
                let done = (kind == Pending::Start) == self.running;
                if done {
                    self.pending = None;
                    if kind == Pending::Stop && std::mem::take(&mut self.restart_after_stop) {
                        self.start_background();
                    } else if kind == Pending::Start && self.notice == Notice::Restarting {
                        self.notice = Notice::Applied(now);
                    }
                } else if age(since) > PENDING_TIMEOUT {
                    self.pending = None;
                    self.restart_after_stop = false;
                    if self.notice == Notice::Restarting {
                        self.notice = Notice::Idle;
                    }
                    self.error = Some(
                        match kind {
                            Pending::Start if self.config.log => {
                                "AvrcpKeeper didn't start. AvrcpKeeper.log next to the exe may say why."
                            }
                            Pending::Start => {
                                "AvrcpKeeper didn't start. Set log = true in AvrcpKeeper.ini and try again to see why."
                            }
                            Pending::Stop => "AvrcpKeeper didn't stop in time. Try again.",
                        }
                        .to_string(),
                    );
                }
            }
        }
        if self.last_devices.is_none_or(|t| age(t) >= DEVICES_EVERY) {
            self.refresh_devices();
        }
        match self.notice {
            Notice::Applying(since) => {
                if self.applied.received() {
                    self.notice = Notice::Applied(now);
                } else if age(since) > APPLY_TIMEOUT {
                    self.notice = if ipc::background_running() { Notice::NotApplied } else { Notice::Saved(now) };
                }
            }
            Notice::NotApplied if !self.running => self.notice = Notice::Idle,
            Notice::NotApplied if self.applied.received() => self.notice = Notice::Applied(now),
            Notice::Saved(at) | Notice::Applied(at) if age(at) > NOTICE_SHOW => self.notice = Notice::Idle,
            _ => {}
        }
    }

    /// Reloads settings changed outside this window (for example in Notepad), unless there are
    /// unsaved changes here that would be lost. Like the background instance, it waits until the
    /// file looks the same on two polls so a half-written file is never loaded.
    fn pick_up_external_edits(&mut self) {
        if self.dirty_at.is_some() {
            return;
        }
        let stamp = config::file_stamp(&self.config_path);
        if stamp == self.config_stamp {
            self.pending_stamp = None;
            return;
        }
        if self.pending_stamp.as_ref() != Some(&stamp) {
            self.pending_stamp = Some(stamp);
            return;
        }
        if let Ok(config) = Config::load(&self.config_path) {
            self.config = config;
            self.config_stamp = stamp;
            self.pending_stamp = None;
        }
    }

    fn refresh_devices(&mut self) {
        self.last_devices = Some(Instant::now());
        if self.enumerator.is_none() {
            self.enumerator = audio::enumerator().ok();
        }
        let Some(en) = &self.enumerator else { return };
        let keep: &[String] = match &self.config.devices {
            Devices::Selected(ids) => ids,
            Devices::All => &[],
        };
        self.devices = audio::output_devices(en, keep);
        let mut apps: Vec<String> = audio::app_sessions(en, false, std::process::id())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.app)
            .filter(|a| a != "avrcpkeeper")
            .collect();
        apps.sort();
        apps.dedup();
        self.window_apps = window::windowed_apps().into_iter().filter(|a| !apps.contains(a)).collect();
        self.audio_apps = apps;
    }

    fn start_background(&mut self) {
        if self.dirty_at.is_some() {
            let _ = self.save_now();
        }
        match ipc::start_background() {
            Ok(()) => self.pending = Some((Pending::Start, Instant::now())),
            Err(e) => {
                self.restart_after_stop = false;
                if self.notice == Notice::Restarting {
                    self.notice = Notice::Idle;
                }
                self.error = Some(format!("Couldn't start AvrcpKeeper: {e}"));
            }
        }
        self.last_poll = None;
    }

    fn stop_background(&mut self) {
        if ipc::request_stop() {
            self.pending = Some((Pending::Stop, Instant::now()));
        } else {
            self.restart_after_stop = false;
            if self.notice == Notice::Restarting {
                self.notice = Notice::Idle;
            }
            self.error = Some("Couldn't reach the running instance to stop it.".to_string());
        }
        self.last_poll = None;
    }

    /// Also used to switch from a copy in another folder to this one: starting this exe takes
    /// over from whichever instance is running.
    fn restart_background(&mut self) {
        self.notice = Notice::Restarting;
        if !ipc::background_running() {
            self.start_background();
            return;
        }
        self.restart_after_stop = true;
        self.stop_background();
    }

    fn set_autostart(&mut self, enabled: bool) {
        match autostart::set_enabled(enabled) {
            Ok(()) => {
                if enabled && !self.running && self.pending.is_none() {
                    self.start_background();
                }
            }
            Err(e) => self.error = Some(format!("Couldn't change the startup setting: {e}")),
        }
        self.startup = autostart::state();
    }

    fn header(&mut self, ui: &mut Ui) {
        let (label, dot) = match (self.pending, self.running) {
            (Some((Pending::Start, _)), _) => ("Starting", theme::TEXT_FAINT),
            (Some((Pending::Stop, _)), _) => ("Stopping", theme::TEXT_FAINT),
            (None, true) => ("Running", theme::GOOD),
            (None, false) => ("Stopped", theme::TEXT_FAINT),
        };
        let subtitle =
            if self.running { "Running. You can close this window." } else { "Keeps your speaker from muting calls" };
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.icon).fit_to_exact_size(egui::vec2(36.0, 36.0)));
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(TITLE).size(theme::SIZE_TITLE).family(theme::semibold()).color(theme::TEXT));
                    ui.add_space(2.0);
                    status_pill(ui, label, dot);
                });
                ui.label(RichText::new(subtitle).size(theme::SIZE_SECONDARY).color(theme::TEXT_DIM));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.pending {
                Some((Pending::Start, _)) => {
                    ui.add_enabled(false, egui::Button::new("Starting…").min_size(egui::vec2(84.0, 30.0)));
                }
                Some((Pending::Stop, _)) => {
                    ui.add_enabled(false, egui::Button::new("Stopping…").min_size(egui::vec2(84.0, 30.0)));
                }
                None if self.running => {
                    if widgets::secondary_button(ui, "Stop").clicked() {
                        self.stop_background();
                    }
                }
                None => {
                    if widgets::primary_button(ui, "Start").clicked() {
                        self.start_background();
                    }
                }
            });
        });
    }

    fn general_card(&mut self, ui: &mut Ui) {
        card(ui, "General", None, |ui| {
            let mut enabled = self.startup == autostart::Startup::On;
            let subtitle = match &self.startup {
                autostart::Startup::OtherCopy(path) => {
                    format!("Currently starts another copy at {}. Turn on to use this one.", path.display())
                }
                autostart::Startup::DisabledInTaskManager => {
                    "Switched off in Task Manager. Turn on to allow it again.".to_string()
                }
                _ => "Runs in the background when you sign in".to_string(),
            };
            if widgets::toggle_row(ui, &mut enabled, "Start with Windows", Some(&subtitle)) {
                self.set_autostart(enabled);
            }
            if let Some(path) = &self.location_warning {
                let text = "Other accounts on this PC can modify this folder and could swap AvrcpKeeper for something \
                            else. Move it to a folder only you can write to, like %LOCALAPPDATA%\\Programs\\AvrcpKeeper.";
                let label = egui::Label::new(RichText::new(text).size(theme::SIZE_SECONDARY).color(theme::WARN)).wrap();
                ui.add(label).on_hover_text(format!("Writable by others: {}", path.display()));
            }
            divider(ui);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Play/pause delay").color(theme::TEXT));
                        widgets::help(ui, "Play/pause delay", HELP_RESYNC);
                    });
                    ui.label(
                        RichText::new("How long a speaker button press shows as paused")
                            .size(theme::SIZE_SECONDARY)
                            .color(theme::TEXT_DIM),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} ms", self.config.resync_ms))
                            .size(theme::SIZE_SECONDARY)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
            ui.add_space(4.0);
            let max = RESYNC_UI_MAX.max(self.config.resync_ms);
            let range = *config::RESYNC_RANGE.start()..=max;
            if widgets::slider(ui, &mut self.config.resync_ms, range, 10, "Play/pause delay").changed() {
                self.mark_dirty();
            }
            let row = egui::vec2(ui.available_width(), 20.0);
            ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
                if widgets::checkbox(ui, &mut self.config.silent_stream, "Play silent audio") {
                    self.mark_dirty();
                }
                widgets::help(ui, "Play silent audio", HELP_SILENT);
                if self.config.trigger == Trigger::Always {
                    let note = RichText::new("Not used with Always").size(theme::SIZE_SECONDARY);
                    ui.label(note.color(theme::TEXT_FAINT));
                }
            });
        });
    }

    fn trigger_card(&mut self, ui: &mut Ui) {
        card(ui, "Active when", Some(HELP_TRIGGER), |ui| {
            let options = [(Trigger::Apps, "Selected apps"), (Trigger::AnyApp, "Any app"), (Trigger::Always, "Always")];
            if widgets::segmented(ui, &mut self.config.trigger, &options) {
                self.mark_dirty();
            }
            let hint = match (self.config.trigger, &self.config.devices) {
                (Trigger::Apps, _) => "While one of these apps is playing sound.",
                (Trigger::AnyApp, _) => "While any app is playing sound. System sounds are ignored.",
                (Trigger::Always, Devices::All) => {
                    "Whenever an output is connected. Media keys still control your player."
                }
                (Trigger::Always, Devices::Selected(_)) => {
                    "Whenever a selected output is connected. Media keys still control your player."
                }
            };
            ui.label(RichText::new(hint).size(theme::SIZE_SECONDARY).color(theme::TEXT_DIM));
            if self.config.trigger == Trigger::Apps {
                ui.add_space(2.0);
                self.app_list(ui);
            }
        });
    }

    fn app_list(&mut self, ui: &mut Ui) {
        if self.config.apps.is_empty() {
            ui.label(RichText::new("No apps yet. Add one below.").size(theme::SIZE_SECONDARY).color(theme::TEXT_FAINT));
        } else {
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for (i, app) in self.config.apps.iter().enumerate() {
                    if widgets::chip(ui, app) {
                        remove = Some(i);
                    }
                }
            });
            if let Some(i) = remove {
                self.config.apps.remove(i);
                self.mark_dirty();
            }
        }
        ui.add_space(2.0);

        let not_added =
            |apps: &[String]| -> Vec<String> { apps.iter().filter(|a| !self.config.wants_app(a)).cloned().collect() };
        let groups =
            [("Playing audio", not_added(&self.audio_apps)), ("Other open apps", not_added(&self.window_apps))];
        let mut picked = None;
        let mut add_now = false;
        let mut field_id = None;
        let row = egui::vec2(ui.available_width(), 30.0);
        ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
            egui::ComboBox::from_id_salt("open-apps")
                .selected_text(RichText::new("Open apps").color(theme::TEXT_DIM))
                .width(116.0)
                .icon(widgets::chevron)
                .show_ui(ui, |ui| {
                    if groups.iter().all(|(_, apps)| apps.is_empty()) {
                        ui.label(
                            RichText::new("No other open apps found")
                                .size(theme::SIZE_SECONDARY)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                    for (heading, apps) in groups.iter().filter(|(_, apps)| !apps.is_empty()) {
                        ui.label(RichText::new(*heading).size(theme::SIZE_CAPTION).color(theme::TEXT_FAINT));
                        for app in apps {
                            if ui.selectable_label(false, app).clicked() {
                                picked = Some(app.clone());
                            }
                        }
                    }
                });
            let can_add = !self.new_app.trim().is_empty();
            add_now |= ui.add_enabled_ui(can_add, |ui| widgets::secondary_button(ui, "Add")).inner.clicked();
            let margin = 8.0;
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.new_app)
                    .hint_text("App name, e.g. zoom")
                    .desired_width(ui.available_width() - margin * 2.0)
                    .margin(Margin::symmetric(margin as i8, 6)),
            );
            ui.ctx().accesskit_node_builder(field.id, |node| node.set_label("App name"));
            if field.changed() {
                self.add_feedback = None;
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_add {
                add_now = true;
                field_id = Some(field.id);
            }
        });

        if add_now {
            if self.config.add_app(&self.new_app) {
                self.new_app.clear();
                self.add_feedback = None;
                self.mark_dirty();
            } else {
                self.add_feedback = Some(if config::normalize_app(&self.new_app).is_some() {
                    "Already added."
                } else {
                    "That doesn't look like an app name."
                });
            }
            if let Some(id) = field_id {
                ui.memory_mut(|m| m.request_focus(id));
            }
        }
        if let Some(app) = picked {
            if self.config.add_app(&app) {
                self.mark_dirty();
            }
        }
        if let Some(message) = self.add_feedback {
            ui.label(RichText::new(message).size(theme::SIZE_SECONDARY).color(theme::TEXT_DIM));
        }
    }

    fn devices_card(&mut self, ui: &mut Ui) {
        card(ui, "Output devices", Some(HELP_DEVICES), |ui| {
            let mut all = self.config.devices == Devices::All;
            let subtitle = all.then_some("Works with whichever device is playing");
            if widgets::toggle_row(ui, &mut all, "All outputs", subtitle) {
                self.config.devices = if all {
                    Devices::All
                } else {
                    let default = self.enumerator.as_ref().and_then(audio::default_output_id);
                    Devices::Selected(default.into_iter().collect())
                };
                self.mark_dirty();
            }
            let Devices::Selected(selected) = &self.config.devices else { return };
            let selected = selected.clone();
            divider(ui);
            if self.devices.is_empty() && selected.is_empty() {
                ui.label(
                    RichText::new("No output devices found.").size(theme::SIZE_SECONDARY).color(theme::TEXT_FAINT),
                );
            }
            let mut changes = Vec::new();
            ui.spacing_mut().item_spacing.y = 2.0;
            for device in &self.devices {
                let mut on = selected.iter().any(|s| s.eq_ignore_ascii_case(&device.id));
                let status =
                    if device.connected { ("Connected", theme::GOOD) } else { ("Disconnected", theme::TEXT_FAINT) };
                if widgets::check_row(ui, &mut on, &device.name, Some(status)) {
                    changes.push((device.id.clone(), on));
                }
            }
            for id in selected.iter().filter(|id| !self.devices.iter().any(|d| d.id.eq_ignore_ascii_case(id))) {
                let mut on = true;
                if widgets::check_row(ui, &mut on, "Unknown device", Some(("Not found", theme::TEXT_FAINT))) {
                    changes.push((id.clone(), on));
                }
            }
            for (id, on) in changes {
                self.config.set_device(&id, on);
                self.mark_dirty();
            }
            if matches!(&self.config.devices, Devices::Selected(ids) if ids.is_empty()) {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Select at least one output, or turn on All outputs.")
                        .size(theme::SIZE_SECONDARY)
                        .color(theme::ERROR),
                );
            }
        });
    }

    fn footer(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 6.0;
        if let Some(error) = self.error.clone() {
            egui::Frame::new()
                .fill(theme::ERROR.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0, theme::ERROR.gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let dismiss_w = 64.0;
                        ui.allocate_ui(egui::vec2(ui.available_width() - dismiss_w, 0.0), |ui| {
                            let text = RichText::new(&error).size(theme::SIZE_SECONDARY).color(theme::ERROR);
                            ui.add(egui::Label::new(text).wrap());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            let dismiss =
                                egui::Button::new(RichText::new("Dismiss").size(theme::SIZE_SECONDARY)).frame(false);
                            if ui.add(dismiss).clicked() {
                                self.error = None;
                            }
                        });
                    });
                });
        } else {
            self.save_status(ui);
        }

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let caption = |text: &str| RichText::new(text).size(theme::SIZE_CAPTION).color(theme::TEXT_FAINT);
            ui.label(caption("by S4N-T0S - "));
            ui.scope(|ui| {
                // One step brighter than the text around it, with an underline in the same color on hover.
                ui.visuals_mut().hyperlink_color = theme::TEXT_DIM;
                ui.hyperlink_to(RichText::new("s4nt0s.eu").size(theme::SIZE_CAPTION), CREDIT_URL);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(caption(concat!("v", env!("CARGO_PKG_VERSION"))));
            });
        });
    }

    /// One line about the last save: saved, applied to the running instance, or not confirmed.
    fn save_status(&mut self, ui: &mut Ui) {
        let caption = |text: &str, color: Color32| RichText::new(text).size(theme::SIZE_CAPTION).color(color);
        let link = |text: &str| RichText::new(text).size(theme::SIZE_CAPTION);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if self.other_copy {
                status_dot(ui, theme::WARN);
                ui.label(caption("Another copy of AvrcpKeeper is running.", theme::WARN)).on_hover_text(
                    "It runs from a different folder and reads its own settings file, so changes made here don't \
                     affect it.",
                );
                if ui.link(link("Use this one")).clicked() {
                    self.restart_background();
                }
                return;
            }
            match self.notice {
                Notice::Applying(since) if since.elapsed() >= APPLYING_SHOW_AFTER => {
                    ui.label(caption("Applying…", theme::TEXT_DIM));
                }
                Notice::Idle | Notice::Applying(_) if self.running => {
                    ui.label(caption("Changes are saved and applied right away", theme::TEXT_FAINT));
                }
                Notice::Idle | Notice::Applying(_) => {
                    ui.label(caption("Changes are saved and take effect when AvrcpKeeper starts", theme::TEXT_FAINT));
                }
                Notice::Saved(_) => {
                    ui.label(caption("Saved", theme::TEXT_DIM));
                }
                Notice::Restarting => {
                    ui.label(caption("Restarting…", theme::TEXT_DIM));
                }
                Notice::Applied(_) => {
                    status_dot(ui, theme::GOOD);
                    ui.label(caption("Saved and applied", theme::TEXT_DIM));
                }
                Notice::NotApplied => {
                    status_dot(ui, theme::WARN);
                    ui.label(caption("Saved, but AvrcpKeeper didn't confirm it.", theme::WARN)).on_hover_text(
                        "The settings file was saved, but AvrcpKeeper didn't report loading it. Restarting makes it \
                         read the file again.",
                    );
                    if ui.link(link("Restart")).clicked() {
                        self.restart_background();
                    }
                }
            }
        });
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.window_styled < 30 {
            if window::style_own_window(TITLE, [theme::BG.r(), theme::BG.g(), theme::BG.b()]) {
                self.window_styled = u32::MAX;
            } else {
                self.window_styled += 1;
            }
        }
        self.poll();

        let panel =
            egui::Frame::new().fill(theme::BG).inner_margin(Margin { left: 20, right: 20, top: 18, bottom: 14 });
        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            self.header(ui);
            ui.add_space(12.0);
            let footer =
                egui::Frame::new().fill(theme::BG).inner_margin(Margin { left: 0, right: 0, top: 10, bottom: 0 });
            egui::TopBottomPanel::bottom("footer")
                .frame(footer)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.footer(ui));
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                self.general_card(ui);
                self.trigger_card(ui);
                self.devices_card(ui);
            });
        });

        let busy = self.dirty_at.is_some()
            || self.pending.is_some()
            || matches!(self.notice, Notice::Applying(_) | Notice::Restarting);
        ctx.request_repaint_after(if busy { Duration::from_millis(150) } else { Duration::from_millis(500) });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty_at.is_some() {
            self.save_now();
        }
    }
}

fn card(ui: &mut Ui, title: &str, help: Option<&str>, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                let text = RichText::new(title).size(theme::SIZE_CARD_TITLE).family(theme::semibold());
                ui.label(text.color(theme::TEXT));
                if let Some(text) = help {
                    widgets::help(ui, title, text);
                }
            });
            add(ui);
        });
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::BORDER);
}

/// A small dot flush with the left edge of the line it starts.
fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), egui::Sense::hover());
    ui.painter().circle_filled(egui::pos2(rect.left() + 3.5, rect.center().y), 3.5, color);
}

fn status_pill(ui: &mut Ui, label: &str, dot: Color32) {
    let font = egui::FontId::proportional(theme::SIZE_CAPTION);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, theme::TEXT_DIM);
    let size = egui::vec2(galley.size().x + 30.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let stroke = Stroke::new(1.0, theme::BORDER);
    painter.rect(rect, CornerRadius::same(12), theme::SURFACE, stroke, egui::StrokeKind::Inside);
    painter.circle_filled(egui::pos2(rect.left() + 13.0, rect.center().y), 4.0, dot);
    painter.galley(egui::pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT_DIM);
}
