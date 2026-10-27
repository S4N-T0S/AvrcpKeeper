use std::collections::HashMap;

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioClient, IAudioRenderClient,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_NOTPRESENT,
    DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

/// Two seconds, in 100 ns units. Refilled once a second.
const SILENT_BUFFER_HNS: i64 = 20_000_000;

pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

pub struct AppSession {
    pub device_id: String,
    /// Lowercase executable name without extension.
    pub app: String,
    /// Current peak sample level, 0.0 to 1.0.
    pub peak: f32,
}

pub fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

pub fn default_output_id(en: &IMMDeviceEnumerator) -> Option<String> {
    let device = unsafe { en.GetDefaultAudioEndpoint(eRender, eMultimedia) }.ok()?;
    device_id(&device)
}

pub fn connected_output_ids(en: &IMMDeviceEnumerator) -> Result<Vec<String>> {
    let list = unsafe { en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }?;
    let count = unsafe { list.GetCount() }?;
    Ok((0..count).filter_map(|i| unsafe { list.Item(i) }.ok()).filter_map(|d| device_id(&d)).collect())
}

/// Connected and unplugged outputs, plus any of `keep_ids` that Windows reports as not present
/// (so a configured device never silently disappears from the list).
pub fn output_devices(en: &IMMDeviceEnumerator, keep_ids: &[String]) -> Vec<OutputDevice> {
    let mask = DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0 | DEVICE_STATE_NOTPRESENT.0);
    let Ok(list) = (unsafe { en.EnumAudioEndpoints(eRender, mask) }) else { return Vec::new() };
    let count = unsafe { list.GetCount() }.unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..count {
        let Ok(device) = (unsafe { list.Item(i) }) else { continue };
        let Some(id) = device_id(&device) else { continue };
        let state = unsafe { device.GetState() }.unwrap_or_default();
        let connected = state == DEVICE_STATE_ACTIVE;
        if !connected && state != DEVICE_STATE_UNPLUGGED && !keep_ids.iter().any(|k| k.eq_ignore_ascii_case(&id)) {
            continue;
        }
        let name = friendly_name(&device).unwrap_or_else(|| id.clone());
        out.push(OutputDevice { id, name, connected });
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

pub fn device_name(en: &IMMDeviceEnumerator, id: &str) -> Option<String> {
    let id = HSTRING::from(id);
    let device = unsafe { en.GetDevice(PCWSTR(id.as_ptr())) }.ok()?;
    friendly_name(&device)
}

/// Audio sessions on connected outputs, excluding system sounds, `exclude_pid` and, with
/// `active_only`, sessions that aren't rendering. Fails only if the outputs can't be listed.
pub fn app_sessions(en: &IMMDeviceEnumerator, active_only: bool, exclude_pid: u32) -> Result<Vec<AppSession>> {
    let mut out = Vec::new();
    let mut names: HashMap<u32, Option<String>> = HashMap::new();
    let list = unsafe { en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }?;
    let count = unsafe { list.GetCount() }?;
    for i in 0..count {
        let Ok(device) = (unsafe { list.Item(i) }) else { continue };
        let Some(device_id) = device_id(&device) else { continue };
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else { continue };
        let session_count = unsafe { sessions.GetCount() }.unwrap_or(0);
        for j in 0..session_count {
            let Ok(control) = (unsafe { sessions.GetSession(j) }) else { continue };
            let Ok(state) = (unsafe { control.GetState() }) else { continue };
            if state == AudioSessionStateExpired || (active_only && state != AudioSessionStateActive) {
                continue;
            }
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
            if unsafe { control2.IsSystemSoundsSession() } == S_OK {
                continue;
            }
            let pid = unsafe { control2.GetProcessId() }.unwrap_or(0);
            if pid == 0 || pid == exclude_pid {
                continue;
            }
            let Some(app) = names.entry(pid).or_insert_with(|| process_name(pid)).clone() else { continue };
            let peak = control
                .cast::<IAudioMeterInformation>()
                .and_then(|meter| unsafe { meter.GetPeakValue() })
                .unwrap_or(0.0);
            out.push(AppSession { device_id: device_id.clone(), app, peak });
        }
    }
    Ok(out)
}

fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe {
        let id = device.GetId().ok()?;
        let s = String::from_utf16_lossy(id.as_wide());
        CoTaskMemFree(Some(id.0 as *const _));
        Some(s)
    }
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    let name = value.to_string();
    (!name.trim().is_empty()).then_some(name)
}

fn process_name(pid: u32) -> Option<String> {
    let path = super::process_path(pid)?;
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    (!stem.is_empty()).then_some(stem)
}

/// A shared-mode stream of silence, so the media session has real playback behind it.
pub struct SilentStream {
    client: IAudioClient,
    render: IAudioRenderClient,
    frames: u32,
}

impl SilentStream {
    pub fn start(en: &IMMDeviceEnumerator, device_id: &str) -> Result<SilentStream> {
        unsafe {
            let id = HSTRING::from(device_id);
            let device = en.GetDevice(PCWSTR(id.as_ptr()))?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, SILENT_BUFFER_HNS, 0, format, None);
            CoTaskMemFree(Some(format as *const _));
            init?;
            let frames = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            let stream = SilentStream { client, render, frames };
            stream.refill()?;
            stream.client.Start()?;
            Ok(stream)
        }
    }

    pub fn refill(&self) -> Result<()> {
        unsafe {
            let free = self.frames.saturating_sub(self.client.GetCurrentPadding()?);
            if free > 0 {
                self.render.GetBuffer(free)?;
                self.render.ReleaseBuffer(free, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
            }
        }
        Ok(())
    }
}

impl Drop for SilentStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
