<p align="center"><img src="https://github.com/S4N-T0S/AvrcpKeeper/blob/main/assets/icon.png" width="96" alt="AvrcpKeeper icon"></p>

<h1 align="center">AvrcpKeeper</h1>

<p align="center">
Fix for Bluetooth speakers that go silent on Discord calls after you pause Spotify or YouTube.<br>
Windows 10 and 11. Free, open source, no installer.
</p>

<p align="center"><a href="https://github.com/S4N-T0S/AvrcpKeeper/releases/latest"><b>Download the latest release</b></a></p>

<p align="center"><img src="https://github.com/S4N-T0S/AvrcpKeeper/blob/main/assets/screenshot.png" width="420" alt="AvrcpKeeper settings window"></p>

## Why I made this

I use a Bluetooth speaker (a JBL Flip) with my PC, and kept running into the same thing: Discord calls would go completely silent. It only happened after I'd had something like Spotify or YouTube playing in the background and then paused or stopped it. From then on I couldn't hear anyone in the call until I started playing music again.

As far as I can tell, this is what happens. When you pause Spotify or YouTube, Windows tells the speaker over Bluetooth that playback is paused, and the speaker acts on that by going quiet. Discord never reports itself as a media player, so when someone starts talking, nothing tells the speaker that audio is playing again. Discord's audio is still being sent to the speaker; the speaker just outputs silence.

I made AvrcpKeeper to fix that for myself, and put it up here in case anyone else runs into the same problem.

## Symptoms

On Windows, with a Bluetooth speaker or headphones:

- No sound from Discord after you pause Spotify, YouTube or another media player. You can't hear anyone in the call.
- The speaker stays silent until you play some music again, and then the call audio comes back.
- Games, system sounds and other audio that doesn't come from a media player go silent on the speaker too.

I found and tested this on a JBL Flip, but any speaker that stops its output when it's told playback is paused will have the same problem.

## How it works

Windows sends the speaker a play status over AVRCP (the Bluetooth remote-control profile), based on whichever media player is active. AvrcpKeeper registers its own media session and reports it as "playing" while the trigger is active (by default, Discord playing audio), with a silent audio stream underneath. The speaker sees "playing" and keeps its output on. Twenty seconds after the trigger goes quiet, the session is removed and everything goes back to normal.

- **Media buttons still work.** While AvrcpKeeper is the active session, play/pause, next and previous from the speaker or keyboard go to the player you used last.
- **Play/pause on the speaker stays in sync.** Some speakers track play/pause themselves, so after each press AvrcpKeeper reports "paused" for a moment and then "playing" again. That keeps the button working on every press. Call audio dips for that moment.
- **Nothing changes the rest of the time.** AvrcpKeeper only reports "playing" while the trigger is active, and only on the outputs you pick.

## Install

1. Download `AvrcpKeeper-<version>-windows-x64.zip` from the [latest release](https://github.com/S4N-T0S/AvrcpKeeper/releases/latest).
2. Extract it to a folder only your account can change, such as `%LOCALAPPDATA%\Programs\AvrcpKeeper`. Folders directly under `C:\` (like `C:\Tools`) are usually writable by every account on the PC, which would let another user replace the exe that starts when you sign in. The settings window warns you if that's the case.
3. Run `AvrcpKeeper.exe`, click **Start** and turn on **Start with Windows** if you want it running every time you sign in.

It's a single portable exe. It doesn't install anything, doesn't need admin rights and only writes its settings file (and a log, if you turn it on) next to itself. Start with Windows adds one entry to your user's startup programs. To remove it, turn off **Start with Windows**, click **Stop** and delete the folder.

Requires Windows 10 version 1809 or later, or Windows 11 (x64).

The release builds aren't code signed, so SmartScreen may warn the first time you run one. Choose **More info** and then **Run anyway**, or build it yourself. Each release has a `.sha256` checksum and a build provenance attestation, so you can check that a download was built from this repository by GitHub Actions:

```
gh attestation verify AvrcpKeeper-<version>-windows-x64.zip -R S4N-T0S/AvrcpKeeper
```

## Settings

| Setting | What it does |
| --- | --- |
| Start with Windows | Adds `AvrcpKeeper.exe --background` to your user's startup programs. |
| Play/pause delay | How long the "paused" blip lasts after a play/pause press. 250 ms by default. If the speaker's button only works once and then stops responding, raise it. |
| Active when | **Selected apps** (Discord by default), **Any app** that is playing sound (system sounds are ignored), or **Always** while a selected output is connected. |
| Output devices | **All outputs**, or only specific ones. Bluetooth speakers stay in the list while they're switched off, so you can pick yours before it connects. |

Changes are saved right away and the running copy picks them up without a restart, including edits you make to the file by hand. The window shows "Saved and applied" once the running copy confirms a change.

Settings are stored in `AvrcpKeeper.ini` next to the exe:

```ini
trigger = apps
apps = discord, discordptb, discordcanary
devices = all
resync_ms = 250
log = false
```

`trigger` is `apps`, `any` or `always`. `apps` takes executable names with or without `.exe`. `devices` is `all` or a comma-separated list of audio endpoint IDs (the settings window fills these in for you). `log = true` writes `AvrcpKeeper.log` next to the exe (off by default). Once the log reaches 1 MB it replaces `AvrcpKeeper.log.old`, so there are never more than two log files.

## Command line

Running the exe without options opens the settings window. The options are for scripts and autostart:

| Option | |
| --- | --- |
| `--background` | Run without a window using the saved settings. Starting a second background instance replaces the first. |
| `--verbose` | With `--background`, write the log even if `log` is off, including media session changes and button presses. |
| `--stop` | Stop the background instance. |
| `--version` | Print the version. |
| `--help` | Print usage. |

## Troubleshooting

- **The speaker still mutes during a call.** Make sure the app is in the list under its process name. Task Manager > Details shows it, or pick it from **Open apps**, which lists apps playing audio first and then other open apps. With `log = true`, `AvrcpKeeper.log` shows whether AvrcpKeeper became active.
- **Play/pause on the speaker works once and then does nothing.** Raise **Play/pause delay**, for example to 500 ms.
- **Discord isn't under "Playing audio" in Open apps.** Discord usually only has an audio stream while you're in a call and not deafened. The rest of the time it's listed under "Other open apps".
- **Getting a detailed log.** Stop it, then run `AvrcpKeeper.exe --background --verbose` and look at `AvrcpKeeper.log`.
- **Memory use.** The background instance uses about 4 MB. The settings window uses more, mostly the graphics driver, but only while it's open.

## Linux

On a default Linux desktop (PipeWire or PulseAudio with BlueZ) the PC never reports "paused" to the speaker, so this problem doesn't come up and there's no Linux build. It can happen if you run `mpris-proxy`, which forwards media players such as Spotify to BlueZ. In that case WirePlumber's built-in placeholder player, which reports "playing" whenever audio is flowing, is worth a try. On WirePlumber 0.5, put this in a file under `~/.config/wireplumber/wireplumber.conf.d/` and restart WirePlumber and `mpris-proxy`:

```
monitor.bluez.properties = {
  bluez5.dummy-avrcp-player = true
}
```

## Building

You need Rust (the version is pinned in `rust-toolchain.toml`, and rustup installs it automatically) and the Windows SDK for `rc.exe`, which comes with the Visual Studio Build Tools.

```
cargo build --release
```

`scripts\package.ps1` runs the tests, builds a release and writes the zip and its SHA-256 checksum to `dist\`. Pushing a `v*` tag that matches the version in `Cargo.toml` builds, attests and publishes a GitHub release automatically.

## License

[MIT](https://github.com/S4N-T0S/AvrcpKeeper/blob/main/LICENSE). Made by S4N-T0S - [s4nt0s.eu](https://s4nt0s.eu)
