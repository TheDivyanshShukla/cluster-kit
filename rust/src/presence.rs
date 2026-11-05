//! Is someone using this PC right now? (SCH-10 students first.) Agent side. Owner: a4.
//! Contract: `busy() -> bool` (one cheap probe, no long blocking) and `enabled() -> bool`.
//! Privacy (SEC-07): only idle time and session state; never keystrokes, screens or files.
//! Linux: an active graphical logind session whose IdleHint is "no", else other processes' CPU above 30 %.
//! macOS: HID idle time under 60 s (ioreg IOHIDSystem). Windows: not yet (always idle).
//! `CK_PRESENCE=off` disables it; virtual nodes never probe.
//! Test hook: `CK_PRESENCE_FAKE=/path/file` makes `busy()` read "busy" or "idle" from that file.

use std::process::Command;

pub fn enabled() -> bool {
    std::env::var("CK_PRESENCE").map_or(true, |v| v != "off")
}

pub fn busy() -> bool {
    if let Ok(f) = std::env::var("CK_PRESENCE_FAKE") {
        return std::fs::read_to_string(f).is_ok_and(|t| t.trim() == "busy");
    }
    probe()
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(target_os = "macos")]
fn probe() -> bool {
    run("ioreg", &["-c", "IOHIDSystem", "-d", "4"]).and_then(|t| hid_idle_ns(&t)).is_some_and(|ns| ns < 60_000_000_000)
}

#[cfg(target_os = "linux")]
fn probe() -> bool {
    match run("loginctl", &["list-sessions", "--no-legend"]) {
        Some(list) => {
            let ids = session_ids(&list);
            let shown = ids.iter().any(|id| run("loginctl", &["show-session", id, "-p", "Type", "-p", "Active", "-p", "IdleHint"]).is_some_and(|t| session_busy(&t)));
            shown || other_cpu_busy()
        }
        None => other_cpu_busy(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe() -> bool {
    false
}

/// Other processes' summed CPU above 30 % of the whole PC, from the change since the previous probe.
#[cfg(target_os = "linux")]
fn other_cpu_busy() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    static SYS: std::sync::Mutex<Option<System>> = std::sync::Mutex::new(None);
    let mut g = SYS.lock().unwrap();
    let sys = g.get_or_insert_with(System::new);
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let me = std::process::id();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
    let sum: f32 = sys.processes().iter().filter(|(p, _)| p.as_u32() != me).map(|(_, p)| p.cpu_usage()).sum();
    sum / cores > 30.0 // ponytail: the first probe reads 0 (no earlier sample); children of ck jobs count as "other"
}

/// First column of each `loginctl list-sessions --no-legend` line.
#[allow(dead_code)]
fn session_ids(out: &str) -> Vec<String> {
    out.lines().filter_map(|l| l.split_whitespace().next()).map(String::from).collect()
}

/// `loginctl show-session -p Type -p Active -p IdleHint` output: graphical + active + not idle.
#[allow(dead_code)]
fn session_busy(out: &str) -> bool {
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('='))).map(str::trim);
    matches!(get("Type"), Some("x11" | "wayland")) && get("Active") == Some("yes") && get("IdleHint") == Some("no")
}

/// `"HIDIdleTime" = 123456789` (nanoseconds) from ioreg output.
#[allow(dead_code)]
fn hid_idle_ns(out: &str) -> Option<u64> {
    let r = out.split("\"HIDIdleTime\"").nth(1)?.trim_start().strip_prefix('=')?.trim_start();
    r.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Hysteresis (pure; time is passed in): busy is reported at once, idle only after `idle_after` seconds of continuous
/// idle probes. The first probe reports its state at once.
#[derive(Default)]
pub struct Hysteresis {
    reported: Option<bool>,
    idle_since: Option<u64>,
}

impl Hysteresis {
    /// Some(new state) when a CTRL message should be sent.
    pub fn step(&mut self, probe_busy: bool, now: u64, idle_after: u64) -> Option<bool> {
        let next = if probe_busy {
            self.idle_since = None;
            true
        } else {
            let since = *self.idle_since.get_or_insert(now);
            self.reported == Some(true) && now.saturating_sub(since) < idle_after
        };
        (self.reported != Some(next)).then(|| {
            self.reported = Some(next);
            next
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers() {
        assert_eq!(session_ids("c1 1000 alice seat0 tty2\n 3 1001 bob seat0 -\n"), ["c1", "3"]);
        assert!(session_ids("").is_empty());
        assert!(session_busy("Type=x11\nActive=yes\nIdleHint=no\n"));
        assert!(session_busy("Type=wayland\nActive=yes\nIdleHint=no"));
        assert!(!session_busy("Type=tty\nActive=yes\nIdleHint=no\n"));
        assert!(!session_busy("Type=x11\nActive=no\nIdleHint=no\n"));
        assert!(!session_busy("Type=x11\nActive=yes\nIdleHint=yes\n"));
        let io = "    | |   \"HIDIdleTime\" = 4123456789\n    | |   \"Other\" = 1\n";
        assert_eq!(hid_idle_ns(io), Some(4_123_456_789));
        assert_eq!(hid_idle_ns("nothing"), None);
    }

    #[test]
    fn hysteresis() {
        let mut h = Hysteresis::default();
        assert_eq!(h.step(false, 0, 300), Some(false), "first probe reports at once");
        assert_eq!(h.step(false, 2, 300), None);
        assert_eq!(h.step(true, 4, 300), Some(true), "busy at once");
        assert_eq!(h.step(true, 6, 300), None);
        assert_eq!(h.step(false, 8, 300), None, "idle not yet");
        assert_eq!(h.step(false, 200, 300), None);
        assert_eq!(h.step(true, 250, 300), None, "busy again resets the idle clock");
        assert_eq!(h.step(false, 260, 300), None);
        assert_eq!(h.step(false, 559, 300), None);
        assert_eq!(h.step(false, 560, 300), Some(false), "300 s of continuous idle");
        assert_eq!(h.step(false, 562, 300), None);
        let mut h = Hysteresis::default();
        assert_eq!(h.step(true, 0, 300), Some(true), "first probe busy");
    }

    #[test]
    fn fake_file() {
        let f = std::env::temp_dir().join(format!("ck-fake-{}", std::process::id()));
        std::fs::write(&f, "busy\n").unwrap();
        std::env::set_var("CK_PRESENCE_FAKE", &f);
        assert!(busy());
        std::fs::write(&f, "idle").unwrap();
        assert!(!busy());
        std::env::remove_var("CK_PRESENCE_FAKE");
        let _ = std::fs::remove_file(f);
    }
}

/// OBS-03: CPU package energy from Linux RAPL (powercap), in joules since an arbitrary start; None when not readable
/// (not Linux, no RAPL, or energy_uj is root-only on this kernel). Counters wrap at max_energy_range_uj.
pub struct Rapl {
    zones: Vec<(std::path::PathBuf, u64, u64)>, // (energy_uj path, range, last reading)
    total_uj: u128,
}

impl Rapl {
    pub fn open() -> Option<Rapl> {
        let mut zones = vec![];
        for e in std::fs::read_dir("/sys/class/powercap").ok()?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // top-level packages only ("intel-rapl:0"); sub-zones ("intel-rapl:0:0") are inside them
            if name.matches(':').count() != 1 || !name.contains("rapl") {
                continue;
            }
            let read = |f: &str| std::fs::read_to_string(e.path().join(f)).ok()?.trim().parse::<u64>().ok();
            if let (Some(now), Some(range)) = (read("energy_uj"), read("max_energy_range_uj")) {
                zones.push((e.path().join("energy_uj"), range, now));
            }
        }
        (!zones.is_empty()).then_some(Rapl { zones, total_uj: 0 })
    }
    /// Joules used since open().
    pub fn joules(&mut self) -> f64 {
        for z in &mut self.zones {
            if let Some(now) = std::fs::read_to_string(&z.0).ok().and_then(|t| t.trim().parse::<u64>().ok()) {
                self.total_uj += wrap_delta(z.2, now, z.1) as u128;
                z.2 = now;
            }
        }
        self.total_uj as f64 / 1e6
    }
}

/// Counter delta with one wrap at `range`.
pub fn wrap_delta(last: u64, now: u64, range: u64) -> u64 {
    if now >= last { now - last } else { range.saturating_sub(last) + now }
}

#[cfg(test)]
mod rapl_tests {
    #[test]
    fn wrap() {
        assert_eq!(super::wrap_delta(10, 25, 100), 15);
        assert_eq!(super::wrap_delta(90, 5, 100), 15);
    }
}

/// A tunable, with an old env var as fallback while the value is still the default.
pub fn get_or_env(key: &str, env: &str) -> f64 {
    match (crate::tune::source(key), std::env::var(env).ok().and_then(|v| v.parse::<f64>().ok())) {
        ("default", Some(v)) => v,
        _ => crate::tune::get(key),
    }
}
