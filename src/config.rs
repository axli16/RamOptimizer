use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub threshold_percent: u32,
    pub poll_seconds: u32,
    pub trim_min_mb: u64,
    pub trim_cooldown_minutes: u64,
    pub exclude: Vec<String>,
    pub apps: Vec<AppRule>,
    pub idle_groups: Vec<IdleGroup>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AppRule {
    /// Shown in popups; empty = first exe without ".exe".
    #[serde(default)]
    pub name: String,
    pub exe: Vec<String>,
    /// Helper processes are only "closed" while none of these (the main app) run, visible or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unless_running: Vec<String>,
    #[serde(default)]
    pub decision: Decision,
}

impl AppRule {
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let e = self.exe.first().map(String::as_str).unwrap_or("?");
        let n = e.len().saturating_sub(4);
        if e.len() > 4 && e[n..].eq_ignore_ascii_case(".exe") { e[..n].to_string() } else { e.to_string() }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    #[default]
    #[serde(rename = "")]
    Ask,
    Always,
    Never,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct IdleGroup {
    pub name: String,
    pub exe: Vec<String>,
    #[serde(default)]
    pub companions: Vec<String>,
    pub idle_minutes: u64,
    pub cpu_percent: f64,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl Default for Config {
    fn default() -> Self {
        let app = |name: &str, exe: &[&str], unless: &[&str]| AppRule {
            name: name.into(),
            exe: strs(exe),
            unless_running: strs(unless),
            decision: Decision::Ask,
        };
        const OFFICE: &[&str] = &["WINWORD.EXE", "EXCEL.EXE", "POWERPNT.EXE", "OUTLOOK.EXE", "ONENOTE.EXE", "MSACCESS.EXE"];
        Config {
            threshold_percent: 85,
            poll_seconds: 10,
            trim_min_mb: 50,
            trim_cooldown_minutes: 10,
            exclude: strs(&[
                "svchost.exe", "dwm.exe", "csrss.exe", "lsass.exe", "MsMpEng.exe", "explorer.exe",
                "winlogon.exe", "services.exe", "smss.exe", "wininit.exe", "ramoptimizer.exe",
            ]),
            apps: vec![
                app("", &["Discord.exe"], &[]),
                app("Steam", &["steam.exe", "steamwebhelper.exe"], &[]),
                app("Spotify", &["Spotify.exe", "SpotifyLauncher.exe"], &[]),
                app("Teams", &["ms-teams.exe"], &[]),
                app("Epic Games", &["EpicGamesLauncher.exe"], &[]),
                app("Microsoft Store", &["WinStore.App.exe"], &[]),
                app("Copilot", &["Copilot.exe"], &[]),
                // wslservice.exe runs as SYSTEM, so only the per-user parts can be closed without admin.
                app("WSL", &["wsl.exe", "wslhost.exe", "wslrelay.exe"], &[]),
                app(
                    "MathWorks services",
                    &["MathWorksServiceHost.exe", "MathWorksServiceHost-Monitor.exe", "MathWorksCrashReporter.exe"],
                    &["MATLAB.exe"],
                ),
                // OfficeClickToRun.exe runs as SYSTEM; these are the per-user helpers.
                app("Office background", &["msoia.exe", "OfficeC2RClient.exe", "msoadfsb.exe"], OFFICE),
                app("Widgets", &["WidgetBoard.exe", "WidgetService.exe"], &[]),
            ],
            idle_groups: vec![IdleGroup {
                name: "Antigravity".into(),
                exe: strs(&["Antigravity IDE.exe", "agy.exe"]),
                companions: strs(&["vmmem"]),
                idle_minutes: 30,
                cpu_percent: 2.0,
            }],
        }
    }
}

/// Missing file: write and return the default. Parse error: return the default plus the error, never overwrite.
pub fn load_or_create(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let c = Config::default();
            let err = save(path, &c).err().map(|e| e.to_string());
            (c, err)
        }
        Err(e) => (Config::default(), Some(e.to_string())),
    }
}

pub fn save(path: &Path, cfg: &Config) -> io::Result<()> {
    let text = toml::to_string_pretty(cfg).map_err(io::Error::other)?;
    std::fs::write(path, text)
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join("RamOptimizer");
    let _ = std::fs::create_dir_all(&d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "ramopt-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_writes_default() {
        let d = tempdir();
        let p = d.join("config.toml");
        let (c, err) = load_or_create(&p);
        assert!(err.is_none());
        assert!(p.exists());
        assert_eq!(c, Config::default());
        assert_eq!(c.threshold_percent, 85);
        assert_eq!(c.idle_groups[0].companions, vec!["vmmem"]);
    }

    #[test]
    fn bad_toml_falls_back_without_overwrite() {
        let d = tempdir();
        let p = d.join("config.toml");
        std::fs::write(&p, "threshold_percent = [oops").unwrap();
        let (c, err) = load_or_create(&p);
        assert!(err.is_some());
        assert_eq!(c, Config::default());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "threshold_percent = [oops");
    }

    #[test]
    fn decision_roundtrip() {
        let d = tempdir();
        let p = d.join("config.toml");
        let mut c = Config::default();
        c.apps[0].decision = Decision::Always;
        save(&p, &c).unwrap();
        assert_eq!(load_or_create(&p).0.apps[0].decision, Decision::Always);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let d = tempdir();
        let p = d.join("config.toml");
        std::fs::write(&p, "threshold_percent = 90\n").unwrap();
        let c = load_or_create(&p).0;
        assert_eq!(c.threshold_percent, 90);
        assert_eq!(c.poll_seconds, 10);
    }

    #[test]
    fn display_name_prefers_name_then_exe() {
        let mut r = AppRule { name: String::new(), exe: vec!["Discord.exe".into()], unless_running: vec![], decision: Decision::Ask };
        assert_eq!(r.display_name(), "Discord");
        r.name = "MathWorks services".into();
        assert_eq!(r.display_name(), "MathWorks services");
    }

    #[test]
    fn default_covers_requested_apps() {
        let c = Config::default();
        for n in ["Microsoft Store", "Copilot", "WSL", "MathWorks services", "Office background", "Widgets"] {
            assert!(c.apps.iter().any(|a| a.name == n), "{n}");
        }
        assert_eq!(c.apps[0].exe, vec!["Discord.exe"]);
    }
}
