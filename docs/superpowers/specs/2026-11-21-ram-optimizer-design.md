# RamOptimizer — Design

Date: 2026-11-21
Status: Approved 2026-11-21

## Intent and why

The user's machine has 16 GB of RAM (15.7 GB usable) and sits at 90%+ usage. Code builds sometimes run out of memory.
A major cause is apps that keep their memory after the user "closes" them (Discord minimizes to the tray to boot faster), along with heavy apps left open but idle.
RamOptimizer is a very lightweight background program. It notices these apps and frees their memory: it trims memory silently, and it kills apps only after the user says yes in a popup.

Snapshot of the machine on 2026-11-21 (1.1 GB free): Antigravity IDE 2.7 GB, claude 1.7 GB, Brave 1.3 GB, svchost 1.1 GB, vmmem 952 MB, Discord 535 MB, Obsidian 494 MB, Chrome 454 MB.

## Goals

- Run all the time at login with a near-zero footprint (target: under 5 MB of working set while idle, and under 0.1% CPU).
- Act only when RAM pressure is high (memory load ≥ 85%, checked every 10 s).
- **Trim** (`EmptyWorkingSet`) background processes that have no visible window, without asking. Nothing is lost, because the pages move to the pagefile and come back when the process touches them. This is the "save to disk and bring back" behavior the user asked for.
- **Ask before killing** "known apps" that are still running with no visible window (for example, Discord closed to the tray).
- **Idle rule for heavy apps:** Antigravity plus its Hyper-V VM (`vmmem`) count as one unit. Offer to close that unit only when Antigravity has not been the foreground window for 30 minutes **and** the combined CPU of Antigravity and `vmmem` has averaged under 2% over that time. A long-running agent keeps CPU above 2%, so it is correctly left alone.

## Non-goals

- Freezing a whole app into a file and restoring it later. Windows has no supported way to do this, so trim is the substitute.
- Running as a true Windows Service. A service runs in session 0 and cannot show popups to the user.
- Admin or elevated operation. The program runs as the user and only touches the user's own processes.
- A settings GUI. Settings live in a TOML file the user edits by hand.

## Anti-goals (failure even if it "works")

- Killing anything without a yes, apart from apps the user marked "Always kill".
- Touching system processes (`svchost`, `dwm`, `csrss`, `lsass`, `MsMpEng`, `explorer`, and so on) or the current foreground app.
- Interrupting an active Antigravity session or a running agent.
- Showing popups repeatedly about the same app.
- Using enough RAM or CPU that it noticeably contributes to the problem.

## Approaches considered

| | Approach | Trade-off |
|-|----------|-----------|
| **Picked** | **Rust tray app** using the `windows-sys` crate | Idles at about 3–5 MB. More code than C#. The user chose it because lighter is better. |
| | C# (.NET) tray app | Fastest to build, but idles at about 30–40 MB. |
| | PowerShell script plus Task Scheduler | No build step, but costs about 80 MB per run and has clunky popups. |

## Design

### Process model
A single `ramoptimizer.exe`, built as a GUI-subsystem binary so no console window appears.
It starts at login through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
A tray icon has a menu with three items: **Pause 1 hr**, **Open config**, and **Quit**.
The main thread runs the Win32 message loop. A 10 s `SetTimer` drives the check loop, so no extra threads are needed.

### Each tick (10 s)
1. Call `GlobalMemoryStatusEx`. If `dwMemoryLoad` is below the threshold (default 85) or the tool is paused, update the CPU and idle samples and stop there.
2. Take a snapshot of the processes (`CreateToolhelp32Snapshot`). Build the set of PIDs that own a **visible** top-level window (`EnumWindows` + `IsWindowVisible`, skipping cloaked windows via `DwmGetWindowAttribute(DWMWA_CLOAKED)`).
3. **Known-app check:** for each `[[apps]]` entry, find all of its processes by exe name. If none of them owns a visible window, the app is "closed but running", and it is handled according to its saved decision:
   - `always`: kill it with no prompt.
   - `never`: skip it.
   - Unset: show a popup (at most one popup on screen at a time).
4. **Idle-group check:** for each `[[idle_groups]]` entry (Antigravity + `vmmem`), apply the idle rule described under Goals. If the group is idle, show a popup.
5. **Trim:** call `EmptyWorkingSet` on each process that meets all of these conditions:
   - it belongs to the current user;
   - it has no visible window;
   - it is not on the exclude list;
   - it is not the foreground app;
   - its working set is at least 50 MB;
   - it has not been trimmed in the last 10 minutes.

   The 10-minute gap avoids thrashing. Access-denied errors are skipped silently.

### Nothing to do
High RAM usage on its own is acceptable. If steps 3–5 find nothing to act on, the tick ends silently, with no alert and no log line, and monitoring continues on the next tick. Saved decisions, cooldowns, and snoozes are respected even when usage stays above the threshold. Usage above the threshold is never a reason to prompt again.

### Popups
The popups use `TaskDialogIndirect`, which supports custom buttons. This needs a comctl32 v6 manifest, which is embedded at build time.
- Known app: `Discord is closed but using 535 MB.` with the buttons **[Kill] [Not now] [Always kill] [Never ask]**.
- Idle group: `Antigravity idle for 30 min (IDE + VM using 3.7 GB).` with the buttons **[Close it] [Not now] [Snooze 1 hr]**.
- "Not now" suppresses that app for 30 minutes.
- "Always kill" and "Never ask" are written back to the config file.

### Kill vs. close
- **Windowless known app (Discord):** `TerminateProcess` on every process with that exe name. There is no unsaved state to lose, because the app has no visible window.
- **Idle group (Antigravity):** post `WM_CLOSE` to its windows so it can show its own "save changes?" prompt. Its VM shuts down when Antigravity exits.
  - If Antigravity is still running after 60 s, show a second popup: **[Force kill] [Leave it]**.
  - `vmmem` is never killed directly. Doing so needs admin rights and could corrupt the IDE's sandbox.

### Config — `%APPDATA%\RamOptimizer\config.toml`
```toml
threshold_percent = 85
poll_seconds = 10
trim_min_mb = 50
trim_cooldown_minutes = 10

exclude = ["svchost.exe","dwm.exe","csrss.exe","lsass.exe","MsMpEng.exe","explorer.exe",
           "winlogon.exe","services.exe","smss.exe","wininit.exe","ramoptimizer.exe"]

[[apps]]               # killed (after asking) when no visible window
exe = ["Discord.exe"]
decision = ""          # "", "always", "never"
# initial list: Discord, Steam (steam.exe, steamwebhelper.exe), Spotify, Teams (ms-teams.exe), Epic (EpicGamesLauncher.exe)

[[idle_groups]]
name = "Antigravity"
exe = ["Antigravity IDE.exe", "agy.exe"]   # verified 2026-11-21; agy.exe = agent CLI (~470 MB)
companions = ["vmmem"]          # counted for CPU/RAM only, never killed
idle_minutes = 30
cpu_percent = 2.0
```
If the file is missing, the program writes this default on the first run.

### Idle tracking
- Every tick, call `GetForegroundWindow` → `GetWindowThreadProcessId`. If the foreground process is in the group, reset `last_foreground` for that group.
- CPU usage comes from `GetProcessTimes` deltas across all processes in the group and its companions. It is kept as a rolling average over `idle_minutes`, using one sample per tick in a fixed ring buffer.
- Checked 2026-11-21: a non-admin user cannot read `vmmem` CPU time (the value comes back empty). The program therefore measures idle CPU from `Antigravity IDE.exe` + `agy.exe`, which covers agent runs. It tries `vmmem` and uses it when readable, and silently skips it otherwise. The VM's RAM goes into the popup text only if it can be read.

### Logging
`%APPDATA%\RamOptimizer\log.txt` records each action as one line (trim, kill, or close, with the MB freed). The file is capped at 1 MB, and the old file is moved to `log.old.txt` when it rolls over.

### Dependencies
`windows-sys` (Win32 bindings), `serde` + `toml` (config), `embed-resource` or `winres` (manifest and icon). Nothing else.
The release profile uses `opt-level="z"`, `lto=true`, `panic="abort"`, and `strip=true`.

### Testing
- Unit tests cover the pure logic: config parse and default write, the idle-rule decision from a sample sequence, the trim eligibility filter, and the known-app "closed" check given a PID→visible-window map.
- Manual check: close Discord to the tray, confirm the popup appears, click Kill, and confirm the RAM drops.
- Measure the program's own working set after 1 hour in Task Manager. The target is under 5 MB.

## Calls made by the builder/designer (not decided by the user)
- Threshold 85%, 10 s polling, a 50 MB trim floor, and a 10 min trim cooldown.
- Tray icon with Pause, Open config, and Quit (so the user can tell it is running and can stop it).
- Autostart via the HKCU Run key, not Task Scheduler.
- "Not now" means 30 minutes.
- The initial known-apps list.
- Running without admin. Consequence: it cannot trim system or other-user processes, and it might not read `vmmem` CPU.

## Left to the builder
- The exact exe names for Teams and Epic, verified on the machine.
- The tray icon art.
- Code layout inside `src/`.
