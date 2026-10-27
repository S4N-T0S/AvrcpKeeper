# Changelog

## 1.0.0

First release.

- Reports a "playing" media session while a trigger is active, so Bluetooth speakers that mute on AVRCP "paused" keep playing calls and game audio.
- Triggers: selected apps (Discord by default), any app playing audio, or always.
- "Open apps" picker that lists apps playing audio first, then other open apps.
- Output device filter, with disconnected Bluetooth speakers kept in the list.
- Speaker play/pause, next and previous are forwarded to the last media player you used.
- Settings window, Start with Windows option and a `--background` mode for autostart.
- Settings live in `AvrcpKeeper.ini` next to the exe and apply without a restart, including hand edits. The settings window shows when a change has been applied.
- Log file, off unless `log = true`.
