# RamOptimizer

Lightweight Windows tray app (Rust, ~0.4 MB working set idle). When memory load is at or above 85% it:
- trims windowless background processes (pages go to the pagefile and come back on demand; nothing is closed),
- asks before killing known apps that are "closed" but still running (Discord, Steam, Spotify, Teams, Epic),
- offers to close Antigravity (and its VM) after 30 min unfocused with CPU under 2%.

Every Sunday after 18:00 it shows a weekly summary: the total RAM freed (closed + trimmed) and the 5 most-closed apps. The totals are kept in `stats.toml`.

**Install:** `cargo build --release`, copy `target\release\ramoptimizer.exe` somewhere permanent, and run it once. It adds itself to `HKCU\...\Run` so it starts at login.
**Config:** `%APPDATA%\RamOptimizer\config.toml`. Edits apply within 10 s. Actions are logged to `log.txt` in the same folder.
**Quit:** tray icon → Quit. **Pause:** tray icon → Pause 1 hr.
**Uninstall:** Quit, then `reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v RamOptimizer /f`, then delete the exe and `%APPDATA%\RamOptimizer`.
