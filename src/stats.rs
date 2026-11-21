//! Weekly totals for the Sunday summary popup, persisted in stats.toml.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Summary shows on the first tick after Sunday 18:00 local time.
const ANCHOR_HOUR: u64 = 18;

#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone)]
pub struct AppStat {
    pub count: u64,
    pub mb: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone)]
#[serde(default)]
pub struct Stats {
    /// Epoch seconds of the last summary (or of first run). 0 = never initialised.
    pub last_summary: u64,
    pub trim_mb: u64,
    pub closed_mb: u64,
    pub apps: BTreeMap<String, AppStat>,
}

/// Epoch seconds of the most recent Sunday 18:00 at or before `now`, from local weekday (0 = Sunday)
/// and time. shortcut: ignores a DST change inside the week (off by 1 h at most).
pub fn last_anchor(now: u64, wday: u64, hour: u64, min: u64, sec: u64) -> u64 {
    let into_week = ((wday * 24 + hour) * 60 + min) * 60 + sec;
    let anchor_off = ANCHOR_HOUR * 3600;
    let since = if into_week >= anchor_off { into_week - anchor_off } else { into_week + 7 * 86400 - anchor_off };
    now.saturating_sub(since)
}

fn gb(mb: u64) -> f64 {
    mb as f64 / 1024.0
}

impl Stats {
    /// True when a Sunday 18:00 has passed since the last summary. The first run only initialises.
    pub fn check_due(&mut self, now: u64, anchor: u64) -> bool {
        if self.last_summary == 0 {
            self.last_summary = now;
            return false;
        }
        self.last_summary < anchor
    }

    pub fn record_trim(&mut self, mb: u64) {
        self.trim_mb += mb;
    }

    pub fn record_close(&mut self, name: &str, mb: u64) {
        self.closed_mb += mb;
        let a = self.apps.entry(name.to_string()).or_default();
        a.count += 1;
        a.mb += mb;
    }

    pub fn reset(&mut self, now: u64) {
        *self = Stats { last_summary: now, ..Default::default() };
    }

    /// (headline, body) for the popup.
    pub fn summary_text(&self) -> (String, String) {
        // Headline stays short to fit the card's single bold line; the split goes in the body.
        let head = format!("Freed {:.1} GB this week", gb(self.closed_mb + self.trim_mb));
        let mut body = format!("{:.1} GB closed, {:.1} GB trimmed", gb(self.closed_mb), gb(self.trim_mb));
        if self.apps.is_empty() {
            body += "\nNo apps were closed this week.";
            return (head, body);
        }
        let mut top: Vec<_> = self.apps.iter().collect();
        top.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(b.1.mb.cmp(&a.1.mb)));
        body += "\nMost closed:";
        for (name, s) in top.into_iter().take(5) {
            // Tab = left column / right-aligned column in the card.
            let size = if s.mb < 1024 { format!("{} MB", s.mb) } else { format!("{:.1} GB", gb(s.mb)) };
            body += &format!("\n{name}\t{}×  {size}", s.count);
        }
        (head, body)
    }
}

/// Missing or corrupt file gives empty stats; never crashes.
pub fn load(path: &Path) -> Stats {
    std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(path: &Path, s: &Stats) -> std::io::Result<()> {
    std::fs::write(path, toml::to_string_pretty(s).map_err(std::io::Error::other)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;
    const D: u64 = 86400;

    #[test]
    fn anchor_is_last_sunday_1800() {
        // Sunday 19:30:00 -> anchor is 1.5 h ago.
        assert_eq!(last_anchor(1_000_000, 0, 19, 30, 0), 1_000_000 - (H + H / 2));
        // Sunday 17:00 -> previous Sunday 18:00 (7 days minus 1 h ago).
        assert_eq!(last_anchor(1_000_000, 0, 17, 0, 0), 1_000_000 - (7 * D - H));
        // Wednesday 10:00 -> Sunday 18:00 was 2 days 16 h ago.
        assert_eq!(last_anchor(1_000_000, 3, 10, 0, 0), 1_000_000 - (2 * D + 16 * H));
    }

    #[test]
    fn due_once_per_week_and_not_on_first_run() {
        let mut s = Stats::default();
        assert!(!s.check_due(1_000_000, 900_000)); // first run: initialise, don't show
        assert_eq!(s.last_summary, 1_000_000);
        assert!(!s.check_due(1_000_100, 900_000)); // anchor before last summary
        assert!(s.check_due(1_700_000, 1_600_000)); // new Sunday passed
        s.reset(1_700_000);
        assert!(!s.check_due(1_700_100, 1_600_000)); // shown already this week
    }

    #[test]
    fn records_and_ranks() {
        let mut s = Stats::default();
        s.record_trim(1024);
        s.record_trim(1024);
        for _ in 0..3 {
            s.record_close("Discord", 800);
        }
        s.record_close("Widgets", 20);
        s.record_close("Widgets", 20);
        s.record_close("Tailscale tray", 60);
        let (head, body) = s.summary_text();
        assert_eq!(head, "Freed 4.4 GB this week");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "2.4 GB closed, 2.0 GB trimmed");
        assert_eq!(lines[1], "Most closed:");
        let lines = &lines[1..];
        assert!(lines[1].contains('\t')); // right-aligned column in the card
        assert!(lines[1].starts_with("Discord") && lines[1].contains("3×") && lines[1].contains("2.3 GB"));
        assert!(lines[2].starts_with("Widgets") && lines[2].contains("2×") && lines[2].ends_with("40 MB"));
        assert!(lines[3].starts_with("Tailscale tray"));
    }

    #[test]
    fn top_five_only_and_empty_text() {
        let mut s = Stats::default();
        let (_, body) = s.summary_text();
        assert_eq!(body, "0.0 GB closed, 0.0 GB trimmed
No apps were closed this week.");
        for (i, n) in ["a", "b", "c", "d", "e", "f"].iter().enumerate() {
            for _ in 0..=i {
                s.record_close(n, 10);
            }
        }
        assert_eq!(s.summary_text().1.lines().count(), 7); // split + header + 5
        assert!(!s.summary_text().1.contains("\na "));
    }

    #[test]
    fn reset_clears_totals_and_roundtrips() {
        let d = std::env::temp_dir().join(format!("ramopt-stats-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let p = d.join("stats.toml");
        let mut s = Stats::default();
        s.record_close("Discord", 800);
        s.record_trim(50);
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        s.reset(42);
        assert_eq!((s.trim_mb, s.closed_mb, s.apps.len(), s.last_summary), (0, 0, 0, 42));
        std::fs::write(&p, "garbage = [").unwrap();
        assert_eq!(load(&p), Stats::default()); // corrupt file never crashes
    }
}
