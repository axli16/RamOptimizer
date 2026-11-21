# RamOptimizer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Windows tray app in Rust that frees RAM. When memory load is at or above 85%, it trims windowless background processes, asks before killing known apps that are "closed" but still running, and offers to close idle Antigravity.

**Architecture:** There is one GUI-subsystem exe. The main thread runs the Win32 message loop and fires a timer every 10 s. All decisions live in a pure module, `policy.rs`, which is unit-tested and contains no Win32 calls. Thin Win32 modules gather facts (`procs.rs`) and carry out actions (`actions.rs`, `tray.rs`), and `main.rs` wires them together.

**Tech Stack:** Rust 1.99 (stable, MSVC), `windows-sys`, `serde` + `toml`, `embed-resource` (build-dep).

**Spec:** `docs/superpowers/specs/2026-11-21-ram-optimizer-design.md`

## Global Constraints

- The only runtime dependencies are `windows-sys`, `serde` (derive), and `toml`. The only build dependency is `embed-resource`.
- Release profile: `opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip=true`.
- `#![windows_subsystem = "windows"]`, so no console window appears.
- The app runs without admin and never asks for elevation. The manifest uses `requestedExecutionLevel level="asInvoker"`, and it also declares the Common-Controls v6 dependency that `TaskDialogIndirect` needs.
- Config is stored at `%APPDATA%\RamOptimizer\config.toml`, and the log at `%APPDATA%\RamOptimizer\log.txt` (1 MB cap, rolled to `log.old.txt`).
- Defaults: `threshold_percent=85`, `poll_seconds=10`, `trim_min_mb=50`, `trim_cooldown_minutes=10`. "Not now" means 30 min, "Snooze" means 1 hr, and Pause means 1 hr. The force-kill re-check happens 60 s after the close request.
- Exe names match case-insensitively.
- `vmmem` is never terminated or trimmed. Companions are only measured.
- When nothing is actionable, the tick ends silently: no log line and no popup. Monitoring continues on the next tick.
- Idle performance targets: working set under 5 MB and CPU under 0.1%.

## Review Focus

1. **A timer tick that fires while a popup is open.** `TaskDialogIndirect` runs its own nested message loop, so `WM_TIMER` re-enters `tick()`. Expected: sampling and trimming continue, no second popup appears, and nothing is double-killed. The test belongs to Task 2: `prompts_suppressed_while_popup_open`.
2. **A window on another virtual desktop.** That window is `DWM_CLOAKED_SHELL`. Expected: the app counts as **open**, not closed. Only `DWM_CLOAKED_APP` and `DWM_CLOAKED_INHERITED` count as hidden. The test belongs to Task 3: `cloak_shell_counts_visible`.
3. **A malformed or hand-edited config.toml.** Expected: the app does not crash and does not overwrite the user's file. It logs one line and runs with defaults. The test belongs to Task 1: `bad_toml_falls_back_without_overwrite`.
4. **A process that exits or whose PID is reused between snapshot and action.** Expected: before terminating, the action opens the PID, re-checks its exe name, and skips the process on mismatch or error. The test belongs to Task 4: `terminate_skips_name_mismatch`.
5. **Memory stays above the threshold with nothing left to do, for example after you clicked "Never ask".** Expected: no repeated popups or log lines on any tick, and suppression windows and decisions are respected. The test belongs to Task 2: `nothing_actionable_is_silent`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `Cargo.toml`, `build.rs`, `res/app.rc`, `res/app.manifest`, `res/app.ico` | Build, release profile, embedded manifest and icon |
| `src/config.rs` | `Config` types, load, default write, decision save |
| `src/policy.rs` | Pure decisions: closed apps, trim candidates, idle tracking, the `Planner` state machine |
| `src/procs.rs` | Win32 facts: process snapshot, visible-window PIDs, foreground PID, memory load |
| `src/actions.rs` | Win32 actions: trim, terminate (with re-check), WM_CLOSE, popups |
| `src/log.rs` | Append a line, roll the file at 1 MB |
| `src/tray.rs` | Tray icon and menu, Run-key autostart |
| `src/main.rs` | Window class, message loop, timer, glue |

---

### Task 1: Scaffold + config

**Files:**
- Create: `Cargo.toml`, `build.rs`, `res/app.rc`, `res/app.manifest`, `res/app.ico`, `src/main.rs` (stub `fn main(){}` plus `mod` declarations), `src/config.rs`, `.gitignore` (`/target`)
- Test: inline `#[cfg(test)] mod tests` in `src/config.rs`

**Interfaces:**
- Produces:
  - `#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)] struct Config { threshold_percent: u32, poll_seconds: u32, trim_min_mb: u64, trim_cooldown_minutes: u64, exclude: Vec<String>, apps: Vec<AppRule>, idle_groups: Vec<IdleGroup> }`. The default for every field is `#[serde(default)]`.
  - `struct AppRule { exe: Vec<String>, decision: Decision }`
  - `#[serde(rename_all="lowercase")] enum Decision { #[serde(rename="")] Ask, Always, Never }`
  - `struct IdleGroup { name: String, exe: Vec<String>, companions: Vec<String>, idle_minutes: u64, cpu_percent: f64 }`
  - `impl Default for Config`: the values from the spec's TOML block. The default apps are Discord `["Discord.exe"]`, Steam `["steam.exe","steamwebhelper.exe"]`, Spotify `["Spotify.exe"]`, Teams `["ms-teams.exe"]`, and Epic `["EpicGamesLauncher.exe"]`. The default group is Antigravity `["Antigravity IDE.exe","agy.exe"]` with companions `["vmmem"]`, 30 minutes, 2.0%.
  - `fn load_or_create(path: &Path) -> (Config, Option<String>)`. A missing file means write the default and return it. A parse error means return the default plus `Some(error)` and do **not** write.
  - `fn save(path: &Path, cfg: &Config) -> io::Result<()>`
  - `fn config_dir() -> PathBuf`, which returns `%APPDATA%\RamOptimizer` and creates it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn missing_file_writes_default() {
    let d = tempdir(); let p = d.join("config.toml");
    let (c, err) = load_or_create(&p);
    assert!(err.is_none()); assert!(p.exists()); assert_eq!(c, Config::default());
    assert_eq!(c.threshold_percent, 85); assert_eq!(c.idle_groups[0].companions, vec!["vmmem"]);
}
#[test] fn bad_toml_falls_back_without_overwrite() {
    let d = tempdir(); let p = d.join("config.toml");
    std::fs::write(&p, "threshold_percent = [oops").unwrap();
    let (c, err) = load_or_create(&p);
    assert!(err.is_some()); assert_eq!(c, Config::default());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "threshold_percent = [oops");
}
#[test] fn decision_roundtrip() {
    let d = tempdir(); let p = d.join("config.toml");
    let mut c = Config::default(); c.apps[0].decision = Decision::Always;
    save(&p, &c).unwrap(); assert_eq!(load_or_create(&p).0.apps[0].decision, Decision::Always);
}
#[test] fn partial_file_fills_defaults() {
    let d = tempdir(); let p = d.join("config.toml");
    std::fs::write(&p, "threshold_percent = 90\n").unwrap();
    let c = load_or_create(&p).0; assert_eq!(c.threshold_percent, 90); assert_eq!(c.poll_seconds, 10);
}
```
`tempdir()` is a test helper that creates a unique dir under `std::env::temp_dir()`. Do not add a `tempfile` dependency.

- [ ] **Step 2: Run `cargo test config`.** Expected: a compile failure, because the items are not defined yet.
- [ ] **Step 3: Implement `config.rs` and the scaffold.** `build.rs` calls `embed_resource::compile("res/app.rc", embed_resource::NONE)`. `app.rc` embeds the icon as resource ID `1` and the manifest (`1 24 "app.manifest"`).
- [ ] **Step 4: Run `cargo test config`.** Expected: 4 passed. Then run `cargo build --release`. Expected: success.
- [ ] **Step 5: Commit** with `git commit -m "feat: scaffold and config load/save"`.

---

### Task 2: Pure policy

**Files:**
- Create: `src/policy.rs` (tests inline)

**Interfaces:**
- Consumes: `Config`, `AppRule`, `Decision`, and `IdleGroup` from Task 1.
- Produces:
  - `struct Proc { pid: u32, exe: String, ws_bytes: u64, cpu_100ns: u64, same_session: bool }`
  - `struct Facts { now: Instant, mem_load: u32, procs: Vec<Proc>, visible_pids: HashSet<u32>, foreground_pid: u32, self_pid: u32 }`
  - `enum Action { Trim(u32), PromptApp { app_idx: usize, mb: u64 }, KillApp { app_idx: usize }, PromptIdle { group_idx: usize, mb: u64 } }`. The app name shown in the popup is `exe[0]` without its `.exe` suffix.
  - `struct Planner`, with:
    - `Planner::new(cfg: &Config) -> Self`
    - `fn plan(&mut self, cfg: &Config, f: &Facts, popup_open: bool, paused: bool) -> Vec<Action>`
    - `fn not_now_app(&mut self, idx, now)` (30 min)
    - `fn not_now_group(&mut self, idx, now)` (30 min)
    - `fn snooze_group(&mut self, idx, now)` (60 min)
    - `fn note_trimmed(&mut self, pid, now)`
  - `fn app_closed(rule: &AppRule, f: &Facts) -> Option<u64>`. It returns the MB used when at least one process matches and none is in `visible_pids`.

Rules inside `plan`, in this order:
1. Always update idle tracking for every group. That means sampling the CPU delta, and setting `last_fg = now` if the foreground PID's exe is in the group.
2. If `paused`, or `mem_load < threshold`, return `[]`.
3. **Known apps:** for each closed app, `Always` produces `KillApp`, and `Never` is skipped. `Ask` produces `PromptApp`, unless `popup_open`, a prompt was already emitted this tick, or the app is inside its not-now window.
4. **Idle groups:** a group gets `PromptIdle` when all of these hold:
   - the ring is full (`idle_minutes*60/poll_seconds` samples);
   - `now - last_fg >= idle_minutes`;
   - the average CPU percentage is below `cpu_percent`;
   - no popup is open;
   - it is not snoozed or in a not-now window.
5. **Trim:** emit `Trim(pid)` for each process that meets all of these conditions:
   - `same_session`;
   - not in `visible_pids`;
   - not the foreground or self PID;
   - exe not in `exclude`, and not any group's exe or companion;
   - `ws_bytes >= trim_min_mb`;
   - not trimmed within the cooldown.

The CPU percentage is computed as `Δcpu_100ns / (Δwall_100ns * num_cpus) * 100`. `num_cpus` comes from `std::thread::available_parallelism`. A group with no live processes resets its ring.

- [ ] **Step 1: Write the failing tests.** Build `Facts` with a small helper. Each test asserts on the returned `Vec<Action>`.
  - `below_threshold_no_actions`: with `mem_load = 80`, the result is `[]`.
  - `closed_discord_prompts_once`: three `Discord.exe` PIDs, none visible, `mem_load = 90`. The result contains exactly one `PromptApp{app_idx:0, mb: sum}`.
  - `visible_discord_not_closed`: if one Discord PID is in `visible_pids`, there is no `PromptApp`.
  - `always_kills_never_skips`: `Decision::Always` produces `KillApp{0}`. `Decision::Never` produces no app actions.
  - `prompts_suppressed_while_popup_open`: with `popup_open = true`, there is no `PromptApp`/`PromptIdle`, but `Trim` is still emitted.
  - `not_now_suppresses_30_min`: after `not_now_app(0, t)`, there is no prompt at `t+29min`, and a prompt at `t+31min`.
  - `nothing_actionable_is_silent`: with Discord on `Never`, everything visible, and `mem_load = 99`, 100 consecutive `plan` calls all return `[]`.
  - `trim_filters`: a 49 MB process is not trimmed, a 50 MB one is. Foreground, self, `svchost.exe` (excluded), `vmmem`, `Antigravity IDE.exe`, and `same_session = false` are each excluded. A PID trimmed at `t` is not trimmed again before `t+10min`.
  - `idle_group_rules`: Antigravity foreground at `t0`, then not foreground, with CPU 1% on every sample. No prompt at `t0+29min`, then a `PromptIdle` at `t0+30min`. With CPU at 5%, never. If it becomes foreground again at `t0+20min`, the 30 minutes restart. After `snooze_group`, there is no prompt for 60 min.
  - `exe_match_case_insensitive`: `discord.EXE` matches the rule `Discord.exe`.
- [ ] **Step 2: Run `cargo test policy`.** Expected: a compile failure.
- [ ] **Step 3: Implement `policy.rs` per the rules above.** The ring is a `VecDeque<f64>` capped at capacity.
- [ ] **Step 4: Run `cargo test policy`.** Expected: all pass.
- [ ] **Step 5: Commit** with `git commit -m "feat: pure policy planner"`.

---

### Task 3: Win32 facts

**Files:**
- Create: `src/procs.rs`. Tests are inline and run against the live system.

**Interfaces:**
- Consumes: `Proc` and `Facts` from Task 2.
- Produces:
  - `fn gather(now: Instant) -> Facts`
  - `fn mem_load() -> u32` (`GlobalMemoryStatusEx.dwMemoryLoad`)
  - `fn cloak_hides(cloaked: u32) -> bool`. It returns true when `cloaked & (DWM_CLOAKED_APP | DWM_CLOAKED_INHERITED) != 0` and false for `DWM_CLOAKED_SHELL` alone.

How `gather` gets its facts:
- Process list: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)`.
- Per process, open it with `PROCESS_QUERY_LIMITED_INFORMATION`:
  - working set via `K32GetProcessMemoryInfo` (0 on failure);
  - CPU time via `GetProcessTimes`, kernel plus user (0 on failure).
- `same_session`: `ProcessIdToSessionId(pid) == own session`.
- Visible-window PIDs: `EnumWindows`, keeping a window when `IsWindowVisible`, `GetWindow(GW_OWNER)` is null, and `!cloak_hides(DWMWA_CLOAKED value)`. The PID comes from `GetWindowThreadProcessId`.
- Foreground PID: `GetForegroundWindow` → `GetWindowThreadProcessId`.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn gather_sees_self() {
    let f = gather(Instant::now());
    let me = f.procs.iter().find(|p| p.pid == std::process::id()).unwrap();
    assert!(me.ws_bytes > 0 && me.same_session);
    assert!(f.mem_load > 0 && f.mem_load <= 100);
}
#[test] fn cloak_shell_counts_visible() {
    assert!(!cloak_hides(DWM_CLOAKED_SHELL)); assert!(cloak_hides(DWM_CLOAKED_APP));
    assert!(cloak_hides(DWM_CLOAKED_INHERITED)); assert!(!cloak_hides(0));
}
```
- [ ] **Step 2: Run `cargo test procs`.** Expected: a compile failure.
- [ ] **Step 3: Implement `procs.rs`.** Add only the `windows-sys` features these calls need.
- [ ] **Step 4: Run `cargo test procs`.** Expected: pass.
- [ ] **Step 5: Commit** with `git commit -m "feat: win32 process facts"`.

---

### Task 4: Actions + log

**Files:**
- Create: `src/actions.rs`, `src/log.rs` (tests inline)

**Interfaces:**
- Produces:
  - `fn log_line(dir: &Path, msg: &str)`. It writes the line with an epoch-seconds prefix and rolls the file to `log.old.txt` when `log.txt` exceeds 1_048_576 bytes. I/O errors are ignored.
  - `fn trim(pid: u32) -> bool`. It opens the process with `PROCESS_SET_QUOTA|PROCESS_QUERY_LIMITED_INFORMATION` and calls `K32EmptyWorkingSet`.
  - `fn terminate_checked(pid: u32, expected_exe: &str) -> bool`. It opens with `PROCESS_TERMINATE|PROCESS_QUERY_LIMITED_INFORMATION` and compares the `QueryFullProcessImageNameW` file name case-insensitively. On mismatch it returns false and does nothing. Otherwise it calls `TerminateProcess(h, 1)`.
  - `fn close_windows(pids: &HashSet<u32>)`. It sends `PostMessageW(WM_CLOSE)` to every visible top-level window owned by those PIDs.
  - `enum AppChoice { Kill, NotNow, Always, Never }`, with `fn ask_app(owner: HWND, name: &str, mb: u64) -> AppChoice`
  - `enum IdleChoice { Close, NotNow, Snooze }`, with `fn ask_idle(owner: HWND, name: &str, minutes: u64, mb: u64) -> IdleChoice`
  - `fn ask_force(owner: HWND, name: &str) -> bool`
  - The popups use `TaskDialogIndirect` with custom buttons. The text is copied exactly from the spec: `"{name} is closed but using {mb} MB."`, `"{name} idle for {minutes} min (IDE + VM using {gb:.1} GB)."`. Closing the dialog with X counts as NotNow, or false for the force prompt.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn log_rolls_at_1mb() {
    let d = tempdir(); std::fs::write(d.join("log.txt"), vec![b'x'; 1_048_577]).unwrap();
    log_line(&d, "hello");
    assert!(d.join("log.old.txt").exists());
    assert!(std::fs::read_to_string(d.join("log.txt")).unwrap().ends_with("hello\n"));
}
#[test] fn trim_self_ok() { assert!(trim(std::process::id())); }
#[test] fn terminate_skips_name_mismatch() {
    let mut c = std::process::Command::new("cmd").args(["/c","timeout","/t","30"]).spawn().unwrap();
    assert!(!terminate_checked(c.id(), "notepad.exe"));
    assert!(c.try_wait().unwrap().is_none());           // still alive
    assert!(terminate_checked(c.id(), "CMD.EXE"));
    assert!(c.wait().is_ok());
}
#[test] fn terminate_dead_pid_false() { assert!(!terminate_checked(0xFFFF_FFF0, "x.exe")); }
```
- [ ] **Step 2: Run `cargo test actions log`.** Expected: a compile failure.
- [ ] **Step 3: Implement `log.rs` and `actions.rs`.**
- [ ] **Step 4: Run `cargo test`.** Expected: all pass, including Tasks 1–3.
- [ ] **Step 5: Commit** with `git commit -m "feat: trim/kill/close actions, popups, log"`.

---

### Task 5: Main loop + tray + autostart

**Files:**
- Modify: `src/main.rs`
- Create: `src/tray.rs`

**Interfaces:**
- Consumes everything above.
- Produces:
  - `tray::add(hwnd)` and `tray::remove(hwnd)`. These handle `Shell_NotifyIconW`: the icon is resource ID 1, the tooltip is `"RamOptimizer"`, and the callback message is `WM_APP+1`.
  - `tray::show_menu(hwnd) -> Option<MenuCmd>`, with `enum MenuCmd { Pause, OpenConfig, Quit }`. The labels are exactly `Pause 1 hr`, `Open config`, `Quit`.
  - `tray::ensure_autostart()`. It writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `RamOptimizer` = the quoted path of the current exe, and only writes it if the value is missing or different.

`main` steps:
1. Single instance: `CreateMutexW("Local\\RamOptimizer")`. If `ERROR_ALREADY_EXISTS`, exit.
2. Load config with `load_or_create`. If an error comes back, log it as `config error: {e}; using defaults`.
3. Create a message-only window (`HWND_MESSAGE`) to receive messages.
4. Call `tray::add`, `ensure_autostart`, and `SetTimer(poll_seconds*1000)`.
5. Run the message loop.

`WM_TIMER` handling: `gather` → `planner.plan(cfg, facts, popup_open, paused)` → run the actions.
- `Trim`: call `actions::trim`, then `note_trimmed`, then log `trim {exe} {pid} {mb}MB`.
- `KillApp`: call `terminate_checked` for each matching PID and log `kill {name} {mb}MB`.
- `PromptApp`: set `popup_open = true`, call `ask_app`, then set `popup_open = false`.
  - Kill: terminate the app's processes.
  - Always: save `Decision::Always` to config, then kill.
  - Never: save `Decision::Never`.
  - NotNow: call `not_now_app`.
- `PromptIdle`: show `ask_idle` the same way.
  - Close: call `close_windows(group pids)`, record a deadline of `now + 60s`, and on a later tick, if group processes are still alive, call `ask_force`. If that returns true, call `terminate_checked` on the group's `exe` PIDs only, never companions.
  - NotNow or Snooze: call the matching planner method.

Menu commands: Pause sets `paused_until = now + 1h`. Open config runs `ShellExecuteW("open", config path)`. Quit calls `tray::remove` and `PostQuitMessage`.

The config is re-read every tick by comparing the file's modified time, so your edits apply without a restart. If the file now fails to parse, the app keeps its last good config.

- [ ] **Step 1: Implement `tray.rs` and `main.rs` per the above.** This glue code is checked manually in Task 6, because Tasks 2 and 4 already cover the logic.
- [ ] **Step 2: Run `cargo build --release && cargo test`.** Expected: the build succeeds and all tests pass.
- [ ] **Step 3: Commit** with `git commit -m "feat: tray app main loop"`.

---

### Task 6: Manual verification on the real machine

- [ ] **Step 1:** Run `target\release\ramoptimizer.exe`. Expected: a tray icon appears, `%APPDATA%\RamOptimizer\config.toml` is created, and the Run key value exists (`reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v RamOptimizer`).
- [ ] **Step 2:** Set `threshold_percent = 1` in the config so it triggers right away. Close Discord to the tray. Expected within 10 s: the popup `Discord is closed but using N MB.` appears. Click Kill. Expected: Discord's processes are gone, and `log.txt` contains `kill Discord`.
- [ ] **Step 3:** Expected: within 20 s, `log.txt` shows `trim` lines, and the trimmed processes still run.
- [ ] **Step 4:** Set `idle_minutes = 1`. Leave Antigravity unfocused and quiet. Expected after about 1 min: an idle popup. Click Snooze. Expected: no popup for 1 hr. With an agent running, expected: no popup.
- [ ] **Step 5:** Set an invalid value in the config. Expected: the app keeps running, logs one error line, and does not rewrite the file. Then restore the original values.
- [ ] **Step 6:** After 1 hr, check in Task Manager. Expected: `ramoptimizer.exe` working set under 5 MB and CPU at about 0%. Record the actual numbers. If the working set is higher, call `trim(self_pid)` at the end of each tick and measure again.
- [ ] **Step 7:** Update `README.md` with 5 lines: what it does, how to install (copy the exe and run it once), where the config is, how to quit, and how to uninstall (Quit, then `reg delete ... /v RamOptimizer`). Commit with `git commit -m "docs: readme and verification results"`.
