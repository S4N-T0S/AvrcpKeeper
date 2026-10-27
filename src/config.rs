use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "AvrcpKeeper.ini";
pub const DEFAULT_RESYNC_MS: u32 = 150;
pub const RESYNC_RANGE: RangeInclusive<u32> = 50..=2000;
pub const DEFAULT_APPS: [&str; 3] = ["discord", "discordptb", "discordcanary"];

const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 64;
const MAX_APP_LEN: usize = 128;
const MAX_DEVICE_ID_LEN: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Engage while one of `Config::apps` is outputting audio.
    Apps,
    /// Engage while any app (other than system sounds) is outputting audio.
    AnyApp,
    /// Engage whenever a selected output device is connected.
    Always,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Apps => "apps",
            Trigger::AnyApp => "any",
            Trigger::Always => "always",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "apps" => Some(Trigger::Apps),
            "any" => Some(Trigger::AnyApp),
            "always" => Some(Trigger::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Devices {
    All,
    /// Audio endpoint IDs.
    Selected(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub trigger: Trigger,
    /// Lowercase executable names without the `.exe` extension.
    pub apps: Vec<String>,
    pub devices: Devices,
    pub resync_ms: u32,
    /// Play a silent stream on the output while the session is up.
    pub silent_stream: bool,
    /// Write AvrcpKeeper.log next to the exe.
    pub log: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            trigger: Trigger::Apps,
            apps: DEFAULT_APPS.iter().map(|s| s.to_string()).collect(),
            devices: Devices::All,
            resync_ms: DEFAULT_RESYNC_MS,
            silent_stream: false,
            log: false,
        }
    }
}

impl Config {
    /// Lenient parser: unknown keys and malformed lines are ignored, values are clamped.
    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "trigger" => {
                    if let Some(t) = Trigger::parse(value) {
                        cfg.trigger = t;
                    }
                }
                "apps" => cfg.apps = parse_list(value, normalize_app),
                "devices" => {
                    cfg.devices = if value.eq_ignore_ascii_case("all") {
                        Devices::All
                    } else {
                        Devices::Selected(parse_list(value, normalize_device_id))
                    };
                }
                "resync_ms" => {
                    if let Ok(ms) = value.parse::<u32>() {
                        cfg.resync_ms = ms.clamp(*RESYNC_RANGE.start(), *RESYNC_RANGE.end());
                    }
                }
                "silent_stream" => {
                    if let Some(on) = parse_bool(value) {
                        cfg.silent_stream = on;
                    }
                }
                "log" => {
                    if let Some(on) = parse_bool(value) {
                        cfg.log = on;
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn to_ini(&self) -> String {
        let devices = match &self.devices {
            Devices::All => "all".to_string(),
            Devices::Selected(ids) => ids.join(", "),
        };
        format!(
            "; AvrcpKeeper settings\n\
             ; trigger: apps, any or always\n\
             trigger = {}\n\
             apps = {}\n\
             ; devices: all, or a comma-separated list of audio endpoint IDs\n\
             devices = {}\n\
             resync_ms = {}\n\
             ; silent_stream: true plays silence on the output while active\n\
             silent_stream = {}\n\
             ; log: true writes AvrcpKeeper.log next to the exe\n\
             log = {}\n",
            self.trigger.as_str(),
            self.apps.join(", "),
            devices,
            self.resync_ms,
            self.silent_stream,
            self.log
        )
    }

    /// Missing file means defaults. Read errors are returned so the caller can report them.
    pub fn load(path: &Path) -> io::Result<Config> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES).read_to_end(&mut bytes)?;
        Ok(Config::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Writes to a temporary file and renames it over the target, so readers never see a
    /// partially written file. Returns the stamp of what was written (the rename keeps it).
    pub fn save(&self, path: &Path) -> io::Result<FileStamp> {
        let tmp = path.with_extension(format!("ini.{}.tmp", std::process::id()));
        // create_new never opens an existing file or link; clear a leftover from a crash first.
        let _ = fs::remove_file(&tmp);
        let text = self.to_ini();
        let result = (|| {
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            let meta = f.metadata()?;
            drop(f);
            fs::rename(&tmp, path)?;
            Ok(Some((meta.modified()?, meta.len(), fnv1a(text.as_bytes()))))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    pub fn wants_app(&self, exe_stem: &str) -> bool {
        self.apps.iter().any(|a| a.eq_ignore_ascii_case(exe_stem))
    }

    pub fn wants_device(&self, id: &str) -> bool {
        match &self.devices {
            Devices::All => true,
            Devices::Selected(ids) => ids.iter().any(|d| d.eq_ignore_ascii_case(id)),
        }
    }

    pub fn add_app(&mut self, name: &str) -> bool {
        match normalize_app(name) {
            Some(app) if !self.wants_app(&app) && self.apps.len() < MAX_ENTRIES => {
                self.apps.push(app);
                true
            }
            _ => false,
        }
    }

    pub fn set_device(&mut self, id: &str, selected: bool) {
        let Some(id) = normalize_device_id(id) else { return };
        let Devices::Selected(ids) = &mut self.devices else { return };
        let present = ids.iter().position(|d| d.eq_ignore_ascii_case(&id));
        match (selected, present) {
            (true, None) if ids.len() < MAX_ENTRIES => ids.push(id),
            (false, Some(i)) => {
                ids.remove(i);
            }
            _ => {}
        }
    }
}

/// Modified time, size and a hash of the contents, used to notice when the file changes. The
/// hash catches same-size edits that coarse timestamps (FAT32 keeps 2 s steps) would miss.
pub type FileStamp = Option<(std::time::SystemTime, u64, u64)>;

pub fn file_stamp(path: &Path) -> FileStamp {
    let meta = fs::metadata(path).ok()?;
    let mut bytes = Vec::new();
    File::open(path).ok()?.take(MAX_FILE_BYTES).read_to_end(&mut bytes).ok()?;
    Some((meta.modified().ok()?, meta.len(), fnv1a(&bytes)))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

pub fn path_next_to_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// Accepts "Discord", "Discord.exe" or a full path; returns "discord".
pub fn normalize_app(name: &str) -> Option<String> {
    let name = name.trim();
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let name = match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => &name[..i],
        _ => name,
    };
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_APP_LEN || name.chars().any(|c| c.is_control() || c == ',') {
        return None;
    }
    Some(name.to_lowercase())
}

fn normalize_device_id(id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_DEVICE_ID_LEN || id.chars().any(|c| c.is_control() || c == ',') {
        return None;
    }
    Some(id.to_string())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn parse_list(value: &str, normalize: fn(&str) -> Option<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in value.split(',').filter_map(normalize) {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&item)) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JBL: &str = "{0.0.0.00000000}.{3f2a9c4e-7b1d-4e8a-9c5f-2d6b8a1e4f70}";

    #[test]
    fn empty_text_gives_defaults() {
        assert_eq!(Config::parse(""), Config::default());
    }

    #[test]
    fn parses_all_keys() {
        let cfg = Config::parse(&format!(
            "trigger = any\napps = Spotify.exe, C:\\Games\\game.exe\ndevices = {JBL}\nresync_ms = 400\nsilent_stream = on\nlog = yes\n"
        ));
        assert_eq!(cfg.trigger, Trigger::AnyApp);
        assert_eq!(cfg.apps, vec!["spotify", "game"]);
        assert_eq!(cfg.devices, Devices::Selected(vec![JBL.to_string()]));
        assert_eq!(cfg.resync_ms, 400);
        assert!(cfg.silent_stream);
        assert!(cfg.log);
    }

    #[test]
    fn logging_is_off_unless_enabled() {
        assert!(!Config::default().log);
        assert!(!Config::parse("log = maybe\n").log);
        assert!(Config::parse("log = TRUE\n").log);
        assert!(!Config::parse("log = true\nlog = off\n").log);
    }

    #[test]
    fn silent_stream_is_off_unless_enabled() {
        assert!(!Config::default().silent_stream);
        assert!(Config::parse("silent_stream = true\n").silent_stream);
    }

    #[test]
    fn handles_bom_crlf_comments_and_sections() {
        let cfg = Config::parse("\u{feff}; comment\r\n[settings]\r\n# other\r\ntrigger=always\r\n");
        assert_eq!(cfg.trigger, Trigger::Always);
    }

    #[test]
    fn ignores_invalid_values() {
        let cfg = Config::parse("trigger = sometimes\nresync_ms = -5\nnonsense\n=\nresync_ms = abc\n");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn clamps_resync() {
        assert_eq!(Config::parse("resync_ms = 1").resync_ms, *RESYNC_RANGE.start());
        assert_eq!(Config::parse("resync_ms = 999999").resync_ms, *RESYNC_RANGE.end());
    }

    #[test]
    fn empty_lists_are_allowed() {
        let cfg = Config::parse("apps =\ndevices =\n");
        assert!(cfg.apps.is_empty());
        assert_eq!(cfg.devices, Devices::Selected(vec![]));
    }

    #[test]
    fn dedupes_and_caps_lists() {
        let many: Vec<String> = (0..200).map(|i| format!("app{i}")).collect();
        let cfg = Config::parse(&format!("apps = a, A, a.exe, {}", many.join(",")));
        assert_eq!(cfg.apps.len(), MAX_ENTRIES);
        assert_eq!(cfg.apps.iter().filter(|a| *a == "a").count(), 1);
    }

    #[test]
    fn round_trips() {
        let mut cfg =
            Config { trigger: Trigger::Always, resync_ms: 300, silent_stream: true, log: true, ..Config::default() };
        cfg.devices = Devices::Selected(vec![JBL.to_string()]);
        assert_eq!(Config::parse(&cfg.to_ini()), cfg);
        assert_eq!(Config::parse(&Config::default().to_ini()), Config::default());
    }

    #[test]
    fn normalizes_app_names() {
        assert_eq!(normalize_app("  Discord.EXE "), Some("discord".into()));
        assert_eq!(normalize_app("C:/x/y/Game Name.exe"), Some("game name".into()));
        assert_eq!(normalize_app("ünïcode.exe"), Some("ünïcode".into()));
        assert_eq!(normalize_app("é"), Some("é".into()));
        assert_eq!(normalize_app(".exe"), None);
        assert_eq!(normalize_app("a,b"), None);
        assert_eq!(normalize_app("bad\nname"), None);
        assert_eq!(normalize_app(&"x".repeat(MAX_APP_LEN + 1)), None);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let cfg = Config::parse(&format!("devices = {}", JBL.to_uppercase()));
        assert!(cfg.wants_device(JBL));
        assert!(cfg.wants_app("Discord"));
        assert!(!cfg.wants_app("spotify"));
    }

    #[test]
    fn add_app_and_set_device() {
        let mut cfg = Config { devices: Devices::Selected(vec![]), ..Config::default() };
        assert!(cfg.add_app("Game.exe"));
        assert!(!cfg.add_app("game"));
        cfg.set_device(JBL, true);
        cfg.set_device(JBL, true);
        assert_eq!(cfg.devices, Devices::Selected(vec![JBL.to_string()]));
        cfg.set_device(&JBL.to_uppercase(), false);
        assert_eq!(cfg.devices, Devices::Selected(vec![]));
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("avrcpkeeper-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let cfg = Config { trigger: Trigger::AnyApp, resync_ms: 500, ..Config::default() };
        let saved = cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        assert_eq!(saved, file_stamp(&path), "save reports the stamp of the file it wrote");
        assert!(!path.with_extension(format!("ini.{}.tmp", std::process::id())).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stamp_notices_same_size_edits() {
        let dir = std::env::temp_dir().join(format!("avrcpkeeper-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        fs::write(&path, "resync_ms = 300\n").unwrap();
        let first = file_stamp(&path);
        fs::write(&path, "resync_ms = 400\n").unwrap();
        assert_ne!(first, file_stamp(&path));
        fs::remove_dir_all(&dir).unwrap();
    }
}
