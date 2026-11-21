//! Pure decision logic: no Win32 calls, so every rule is unit-tested.

use crate::config::{AppRule, Config, Decision};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

const MB: u64 = 1024 * 1024;
const NOT_NOW: Duration = Duration::from_secs(30 * 60);
const SNOOZE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub exe: String,
    pub ws_bytes: u64,
    pub cpu_100ns: u64,
    pub same_session: bool,
}

pub struct Facts {
    pub now: Instant,
    pub mem_load: u32,
    pub procs: Vec<Proc>,
    pub visible_pids: HashSet<u32>,
    pub foreground_pid: u32,
    pub self_pid: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Trim(u32),
    PromptApp { app_idx: usize, mb: u64 },
    KillApp { app_idx: usize },
    PromptIdle { group_idx: usize, mb: u64 },
}

pub fn exe_in(list: &[String], exe: &str) -> bool {
    list.iter().any(|e| e.eq_ignore_ascii_case(exe))
}

/// MB used by the app when at least one of its processes runs and none owns a visible window.
pub fn app_closed(rule: &AppRule, f: &Facts) -> Option<u64> {
    let mut any = false;
    let mut bytes = 0;
    for p in f.procs.iter().filter(|p| exe_in(&rule.exe, &p.exe)) {
        if f.visible_pids.contains(&p.pid) {
            return None;
        }
        any = true;
        bytes += p.ws_bytes;
    }
    any.then_some(bytes / MB)
}

#[derive(Default)]
struct GroupState {
    last_fg: Option<Instant>,
    prev: Option<(Instant, u64)>,
    ring: VecDeque<f64>,
    quiet_until: Option<Instant>,
}

pub struct Planner {
    ncpu: f64,
    not_now_apps: HashMap<usize, Instant>,
    groups: Vec<GroupState>,
    trimmed: HashMap<u32, Instant>,
}

fn active(until: Option<&Instant>, now: Instant) -> bool {
    until.is_some_and(|u| now < *u)
}

impl Planner {
    pub fn new(cfg: &Config) -> Self {
        Planner {
            ncpu: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64,
            not_now_apps: HashMap::new(),
            groups: cfg.idle_groups.iter().map(|_| GroupState::default()).collect(),
            trimmed: HashMap::new(),
        }
    }

    pub fn not_now_app(&mut self, idx: usize, now: Instant) {
        self.not_now_apps.insert(idx, now + NOT_NOW);
    }
    pub fn not_now_group(&mut self, idx: usize, now: Instant) {
        if let Some(g) = self.groups.get_mut(idx) {
            g.quiet_until = Some(now + NOT_NOW);
        }
    }
    pub fn snooze_group(&mut self, idx: usize, now: Instant) {
        if let Some(g) = self.groups.get_mut(idx) {
            g.quiet_until = Some(now + SNOOZE);
        }
    }
    pub fn note_trimmed(&mut self, pid: u32, now: Instant) {
        self.trimmed.insert(pid, now);
    }

    /// Samples CPU/foreground for every idle group; returns which groups are idle now.
    fn track_groups(&mut self, cfg: &Config, f: &Facts) -> Vec<(usize, u64)> {
        // Config reload may change the group count.
        self.groups.resize_with(cfg.idle_groups.len(), GroupState::default);
        let fg_exe = f.procs.iter().find(|p| p.pid == f.foreground_pid).map(|p| p.exe.as_str());
        let mut idle = Vec::new();
        for (i, (g, st)) in cfg.idle_groups.iter().zip(self.groups.iter_mut()).enumerate() {
            let members: Vec<&Proc> = f
                .procs
                .iter()
                .filter(|p| exe_in(&g.exe, &p.exe) || exe_in(&g.companions, &p.exe))
                .collect();
            if !members.iter().any(|p| exe_in(&g.exe, &p.exe)) {
                let quiet = st.quiet_until;
                *st = GroupState { quiet_until: quiet, ..Default::default() };
                continue;
            }
            let cpu: u64 = members.iter().map(|p| p.cpu_100ns).sum();
            if let Some((t, c)) = st.prev {
                let wall = f.now.saturating_duration_since(t).as_nanos() as f64 / 100.0;
                if wall > 0.0 {
                    st.ring.push_back(cpu.saturating_sub(c) as f64 / (wall * self.ncpu) * 100.0);
                }
            }
            st.prev = Some((f.now, cpu));
            let cap = (g.idle_minutes * 60 / cfg.poll_seconds.max(1) as u64).max(1) as usize;
            while st.ring.len() > cap {
                st.ring.pop_front();
            }
            if st.last_fg.is_none() || fg_exe.is_some_and(|e| exe_in(&g.exe, e)) {
                st.last_fg = Some(f.now);
            }
            let quiet_for = f.now.saturating_duration_since(st.last_fg.unwrap());
            let avg = st.ring.iter().sum::<f64>() / st.ring.len().max(1) as f64;
            if st.ring.len() >= cap
                && quiet_for >= Duration::from_secs(g.idle_minutes * 60)
                && avg < g.cpu_percent
                && !active(st.quiet_until.as_ref(), f.now)
            {
                let mb = members.iter().map(|p| p.ws_bytes).sum::<u64>() / MB;
                idle.push((i, mb));
            }
        }
        idle
    }

    pub fn plan(&mut self, cfg: &Config, f: &Facts, popup_open: bool, paused: bool) -> Vec<Action> {
        let idle = self.track_groups(cfg, f);
        if paused || f.mem_load < cfg.threshold_percent {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut can_prompt = !popup_open;

        for (i, rule) in cfg.apps.iter().enumerate() {
            let Some(mb) = app_closed(rule, f) else { continue };
            match rule.decision {
                Decision::Always => out.push(Action::KillApp { app_idx: i }),
                Decision::Never => {}
                Decision::Ask => {
                    if can_prompt && !active(self.not_now_apps.get(&i), f.now) {
                        out.push(Action::PromptApp { app_idx: i, mb });
                        can_prompt = false;
                    }
                }
            }
        }
        for (i, mb) in idle {
            if can_prompt {
                out.push(Action::PromptIdle { group_idx: i, mb });
                can_prompt = false;
            }
        }

        let cooldown = Duration::from_secs(cfg.trim_cooldown_minutes * 60);
        self.trimmed.retain(|_, t| f.now.saturating_duration_since(*t) < cooldown);
        let protected = |exe: &str| {
            exe_in(&cfg.exclude, exe)
                || cfg.idle_groups.iter().any(|g| exe_in(&g.exe, exe) || exe_in(&g.companions, exe))
        };
        for p in &f.procs {
            if p.same_session
                && p.ws_bytes >= cfg.trim_min_mb * MB
                && p.pid != f.foreground_pid
                && p.pid != f.self_pid
                && !f.visible_pids.contains(&p.pid)
                && !self.trimmed.contains_key(&p.pid)
                && !protected(&p.exe)
            {
                out.push(Action::Trim(p.pid));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Decision};
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    const MB: u64 = 1024 * 1024;
    const SEC_100NS: u64 = 10_000_000;

    fn p(pid: u32, exe: &str, mb: u64) -> Proc {
        Proc { pid, exe: exe.into(), ws_bytes: mb * MB, cpu_100ns: 0, same_session: true }
    }

    fn facts(now: Instant, load: u32, procs: Vec<Proc>) -> Facts {
        Facts {
            now,
            mem_load: load,
            procs,
            visible_pids: HashSet::new(),
            foreground_pid: 1,
            self_pid: 2,
        }
    }

    fn prompts(a: &[Action]) -> usize {
        a.iter()
            .filter(|x| matches!(x, Action::PromptApp { .. } | Action::PromptIdle { .. }))
            .count()
    }

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn below_threshold_no_actions() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let f = facts(Instant::now(), 80, vec![p(10, "Discord.exe", 200), p(11, "foo.exe", 300)]);
        assert!(pl.plan(&cfg, &f, false, false).is_empty());
    }

    #[test]
    fn closed_discord_prompts_once() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let f = facts(
            Instant::now(),
            90,
            vec![p(10, "Discord.exe", 100), p(11, "Discord.exe", 200), p(12, "Discord.exe", 235)],
        );
        let a = pl.plan(&cfg, &f, false, false);
        let found: Vec<_> = a.iter().filter(|x| matches!(x, Action::PromptApp { .. })).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(*found[0], Action::PromptApp { app_idx: 0, mb: 535 });
    }

    #[test]
    fn visible_discord_not_closed() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let mut f = facts(Instant::now(), 90, vec![p(10, "Discord.exe", 100), p(11, "Discord.exe", 200)]);
        f.visible_pids.insert(11);
        let a = pl.plan(&cfg, &f, false, false);
        assert!(!a.iter().any(|x| matches!(x, Action::PromptApp { .. } | Action::KillApp { .. })));
    }

    #[test]
    fn always_kills_never_skips() {
        let mut cfg = Config::default();
        cfg.apps[0].decision = Decision::Always;
        let mut pl = Planner::new(&cfg);
        let f = facts(Instant::now(), 90, vec![p(10, "Discord.exe", 100)]);
        let a = pl.plan(&cfg, &f, false, false);
        assert!(a.contains(&Action::KillApp { app_idx: 0 }));
        assert_eq!(prompts(&a), 0);

        cfg.apps[0].decision = Decision::Never;
        let a = pl.plan(&cfg, &f, false, false);
        assert!(!a.iter().any(|x| matches!(x, Action::PromptApp { .. } | Action::KillApp { .. })));
    }

    #[test]
    fn prompts_suppressed_while_popup_open() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let f = facts(Instant::now(), 90, vec![p(10, "Discord.exe", 100), p(11, "foo.exe", 300)]);
        let a = pl.plan(&cfg, &f, true, false);
        assert_eq!(prompts(&a), 0);
        assert!(a.contains(&Action::Trim(11)));
    }

    #[test]
    fn paused_no_actions() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let f = facts(Instant::now(), 99, vec![p(10, "Discord.exe", 100), p(11, "foo.exe", 300)]);
        assert!(pl.plan(&cfg, &f, false, true).is_empty());
    }

    #[test]
    fn not_now_suppresses_30_min() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let t = Instant::now();
        pl.not_now_app(0, t);
        let at = |d| facts(t + d, 90, vec![p(10, "Discord.exe", 100)]);
        assert_eq!(prompts(&pl.plan(&cfg, &at(mins(29)), false, false)), 0);
        assert_eq!(prompts(&pl.plan(&cfg, &at(mins(31)), false, false)), 1);
    }

    #[test]
    fn nothing_actionable_is_silent() {
        let mut cfg = Config::default();
        cfg.apps[0].decision = Decision::Never;
        let mut pl = Planner::new(&cfg);
        let t = Instant::now();
        for i in 0..100u64 {
            let mut f = facts(
                t + Duration::from_secs(i * 10),
                99,
                vec![p(10, "Discord.exe", 30), p(11, "foo.exe", 300)], // 30 MB: below trim floor, Never: no prompt
            );
            f.visible_pids.insert(11);
            assert!(pl.plan(&cfg, &f, false, false).is_empty(), "tick {i}");
        }
    }

    #[test]
    fn trim_filters() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let t = Instant::now();
        let mut other_session = p(27, "remote.exe", 300);
        other_session.same_session = false;
        let procs = vec![
            p(20, "small.exe", 49),
            p(21, "big.exe", 50),
            p(1, "fore.exe", 300),             // foreground
            p(2, "ramoptimizer.exe", 300),     // self
            p(23, "svchost.exe", 300),         // excluded
            p(24, "vmmem", 900),               // companion
            p(25, "Antigravity IDE.exe", 900), // group exe
            other_session,
        ];
        let a = pl.plan(&cfg, &facts(t, 90, procs.clone()), false, false);
        let trims: Vec<_> = a.iter().filter_map(|x| if let Action::Trim(p) = x { Some(*p) } else { None }).collect();
        assert_eq!(trims, vec![21]);

        pl.note_trimmed(21, t);
        let mut again = |d| pl_trims(&mut pl, &cfg, facts(t + d, 90, procs.clone()));
        assert!(!again(mins(9)).contains(&21));
        assert!(again(mins(11)).contains(&21));
    }

    fn pl_trims(pl: &mut Planner, cfg: &Config, f: Facts) -> Vec<u32> {
        pl.plan(cfg, &f, false, false)
            .into_iter()
            .filter_map(|x| if let Action::Trim(p) = x { Some(p) } else { None })
            .collect()
    }

    /// Drives the planner one tick every 10 s for `ticks` ticks; the IDE uses `cpu_pct` of the machine.
    struct Sim {
        cfg: Config,
        pl: Planner,
        t0: Instant,
        i: u64,
        cpu: u64,
        ncpu: u64,
    }

    impl Sim {
        fn new() -> Self {
            let cfg = Config::default();
            let pl = Planner::new(&cfg);
            let ncpu = std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1);
            Sim { cfg, pl, t0: Instant::now(), i: 0, cpu: 0, ncpu }
        }
        /// One tick. Returns true if an idle prompt was emitted.
        fn tick(&mut self, cpu_pct: f64, foreground: bool) -> bool {
            self.i += 1;
            self.cpu += (cpu_pct / 100.0 * 10.0 * SEC_100NS as f64 * self.ncpu as f64) as u64;
            let mut ide = p(30, "Antigravity IDE.exe", 2700);
            ide.cpu_100ns = self.cpu;
            let mut f = facts(self.t0 + Duration::from_secs(self.i * 10), 90, vec![ide]);
            if foreground {
                f.foreground_pid = 30;
            }
            self.pl
                .plan(&self.cfg, &f, false, false)
                .iter()
                .any(|x| matches!(x, Action::PromptIdle { group_idx: 0, .. }))
        }
        fn run(&mut self, minutes: u64, cpu_pct: f64) -> bool {
            let mut any = false;
            for _ in 0..minutes * 6 {
                any |= self.tick(cpu_pct, false);
            }
            any
        }
    }

    #[test]
    fn idle_group_prompts_after_30_quiet_minutes() {
        let mut s = Sim::new();
        s.tick(1.0, true);
        assert!(!s.run(29, 1.0));
        assert!(s.run(2, 1.0));
    }

    #[test]
    fn idle_group_busy_cpu_never_prompts() {
        let mut s = Sim::new();
        s.tick(5.0, true);
        assert!(!s.run(60, 5.0));
    }

    #[test]
    fn idle_group_foreground_restarts_clock() {
        let mut s = Sim::new();
        s.tick(1.0, true);
        assert!(!s.run(20, 1.0));
        s.tick(1.0, true);
        assert!(!s.run(25, 1.0));
        assert!(s.run(6, 1.0));
    }

    #[test]
    fn idle_group_snooze_60_min() {
        let mut s = Sim::new();
        s.tick(1.0, true);
        assert!(!s.run(29, 1.0));
        let now = s.t0 + Duration::from_secs(s.i * 10);
        s.pl.snooze_group(0, now);
        assert!(!s.run(59, 1.0));
        assert!(s.run(2, 1.0));
    }

    #[test]
    fn exe_match_case_insensitive() {
        let cfg = Config::default();
        let mut pl = Planner::new(&cfg);
        let f = facts(Instant::now(), 90, vec![p(10, "discord.EXE", 100)]);
        assert_eq!(prompts(&pl.plan(&cfg, &f, false, false)), 1);
    }
}
