//! Every adjustable value in one registry (user, 2026-11-05: "every thing and value should be adjustable").
//! Owner: lead. Contract for wave 9: keep KNOBS keys stable; call sites read values with `get` / `secs` / `n`.
//!
//! Where a value comes from, in priority order: env `CK_TUNE_<KEY>` (upper case, e.g. CK_TUNE_LOST_AFTER_S=8), then the
//! head database (store meta "tune", JSON {key: value}, replicated to standbys), then the default below. The head sends
//! the agent-scope values to every agent on connect and on change (CTRL `{"tune": {key: value}}`); agents apply them
//! with `apply_remote`. Reads are cheap (one RwLock read + a small map lookup) but keep them out of per-byte loops.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Head,
    Agent,
    Both,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Knob {
    pub key: &'static str,
    pub group: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub unit: &'static str,
    pub scope: Scope,
    pub help: &'static str,
}

const fn k(key: &'static str, group: &'static str, default: f64, min: f64, max: f64, unit: &'static str, scope: Scope, help: &'static str) -> Knob {
    Knob { key, group, default, min, max, unit, scope, help }
}
use Scope::{Agent, Both, Head};

/// The registry. Defaults equal the constants the code used before wave 9.
pub const KNOBS: &[Knob] = &[
    // liveness
    k("heartbeat_s", "Liveness", 1.0, 0.2, 10.0, "s", Both, "How often head and agents send a heartbeat."),
    k("lost_after_s", "Liveness", 5.0, 1.0, 120.0, "s", Both, "Silence after which a PC (or the head) counts as lost and its work moves elsewhere."),
    // dispatch
    k("pipeline", "Dispatch", 2.0, 1.0, 16.0, "chunks", Head, "Chunks each PC holds at once: one computing, the rest queued."),
    k("target_chunk_s", "Dispatch", 1.0, 0.05, 60.0, "s", Head, "Adaptive chunks aim to take this long on the PC that gets them."),
    k("max_chunk", "Dispatch", 50_000_000.0, 1_000.0, 1e12, "numbers", Head, "Largest chunk the head ever hands out."),
    k("max_attempts", "Dispatch", 3.0, 1.0, 20.0, "tries", Head, "Tries per range before a job fails (failed program runs)."),
    k("max_events", "Dispatch", 2000.0, 100.0, 100_000.0, "chunks", Head, "Finished chunks kept for the live task stream."),
    // head failover
    k("member_window_s", "Head failover", 60.0, 5.0, 3600.0, "s", Head, "PCs seen within this window count toward the majority a new head waits for; ones that never return drop out this long after a takeover."),
    k("noq_grace_s", "Head failover", 10.0, 2.0, 600.0, "s", Head, "A head with standbys that has no majority for this long stops handing out work (split-brain guard)."),
    k("standby_step_s", "Head failover", 2.0, 0.5, 60.0, "s", Head, "Extra wait per place in the standby order, so standbys take over one at a time."),
    k("ring_max", "Head failover", 100_000.0, 1_000.0, 10_000_000.0, "changes", Head, "Changes kept in memory for standbys that fall behind; older gaps need a full copy."),
    k("switchover_wait_s", "Head failover", 5.0, 1.0, 120.0, "s", Head, "Planned switchover: how long to wait for the standby to confirm."),
    k("client_failover_s", "Head failover", 60.0, 5.0, 3600.0, "s", Agent, "How long `ck run` keeps looking for a new head after losing one."),
    // dynamic heads
    k("target_standbys", "Dynamic heads", 2.0, 0.0, 5.0, "standbys", Head, "Standby heads to keep; a missing one is recruited automatically (also in Standby heads)."),
    k("recruit_tick_s", "Dynamic heads", 10.0, 1.0, 600.0, "s", Head, "How often the head checks whether a standby is missing."),
    k("recruit_every_s", "Dynamic heads", 60.0, 5.0, 3600.0, "s", Head, "At most one new standby request per this interval."),
    k("recruit_retry_s", "Dynamic heads", 600.0, 30.0, 86_400.0, "s", Head, "A PC that was asked (or failed) is not asked again for this long."),
    k("head_w_speed", "Dynamic heads", 1.0, 0.0, 10.0, "weight", Head, "Head choice: weight of the PC's measured speed (calibration)."),
    k("head_w_memory", "Dynamic heads", 1.0, 0.0, 10.0, "weight", Head, "Head choice: weight of RAM and cores."),
    k("head_w_stability", "Dynamic heads", 2.0, 0.0, 10.0, "weight", Head, "Head choice: weight of staying connected (few losses in the last day)."),
    k("head_w_free", "Dynamic heads", 1.0, 0.0, 10.0, "weight", Head, "Head choice: weight of not being used by a person right now."),
    // power
    k("eco_tick_s", "Power", 30.0, 5.0, 3600.0, "s", Head, "How often eco mode looks for PCs to sleep or wake."),
    k("waking_for_s", "Power", 120.0, 10.0, 3600.0, "s", Head, "A PC that does not reconnect this long after a wake packet shows 'did not wake'."),
    k("wake_retry_s", "Power", 60.0, 5.0, 3600.0, "s", Head, "Wait before sending another wake packet to the same PC."),
    k("wake_max", "Power", 64.0, 1.0, 10_000.0, "PCs", Head, "Most PCs woken at once for waiting work."),
    k("energy_every_s", "Power", 10.0, 1.0, 600.0, "s", Agent, "How often agents report CPU energy (RAPL)."),
    // presence
    k("presence_idle_s", "Presence", 300.0, 10.0, 86_400.0, "s", Agent, "A PC counts as free again after nobody touched it for this long."),
    k("presence_probe_s", "Presence", 2.0, 0.5, 60.0, "s", Agent, "How often agents check keyboard and mouse activity."),
    k("calib_s", "Presence", 3.0, 0.5, 60.0, "s", Agent, "Length of the speed test an agent runs when it joins."),
    // agent limits
    k("unacked_max", "Agent limits", 10_000.0, 100.0, 10_000_000.0, "results", Agent, "Results an agent keeps until the head confirms them."),
    k("unacked_max_mb", "Agent limits", 256.0, 1.0, 65_536.0, "MB", Agent, "Memory for unconfirmed results; the oldest go first."),
    k("out_max_mb", "Agent limits", 64.0, 1.0, 4096.0, "MB", Agent, "Program output kept per chunk."),
    k("push_max_kb", "Agent limits", 64.0, 1.0, 65_536.0, "KB", Head, "Largest script `ck run --push` may send."),
    // API
    k("max_body_mb", "API", 1.0, 0.01, 1024.0, "MB", Head, "Largest API request body."),
    k("max_list_body_mb", "API", 64.0, 1.0, 4096.0, "MB", Head, "Largest list-job upload (signed-in operators only)."),
    k("max_items", "API", 1_000_000.0, 1.0, 100_000_000.0, "items", Head, "Most items in one list job."),
    k("max_item_bytes", "API", 4096.0, 16.0, 1_048_576.0, "bytes", Head, "Longest single item in a list job."),
    // security
    k("session_idle_h", "Security", 12.0, 0.1, 720.0, "h", Head, "Sign-out after this long without activity."),
    k("session_max_d", "Security", 7.0, 0.1, 365.0, "days", Head, "Sign-out after this long in any case."),
    k("login_fail_max", "Security", 5.0, 1.0, 100.0, "tries", Head, "Wrong passwords allowed per window before login is blocked."),
    k("login_fail_window_min", "Security", 5.0, 1.0, 1440.0, "min", Head, "Window for counting wrong passwords."),
];

pub fn knob(key: &str) -> Option<&'static Knob> {
    KNOBS.iter().find(|k| k.key == key)
}

#[derive(Default)]
struct State {
    db: HashMap<String, f64>,
    remote: HashMap<String, f64>, // agents: values the head sent
}

fn state() -> &'static RwLock<State> {
    static S: OnceLock<RwLock<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn env_value(key: &str) -> Option<f64> {
    std::env::var(format!("CK_TUNE_{}", key.to_ascii_uppercase())).ok()?.trim().parse().ok()
}

/// Where the current value comes from: "env", "db" (set on the head), "head" (an agent got it from the head), "default".
pub fn source(key: &str) -> &'static str {
    if env_value(key).is_some() {
        return "env";
    }
    let s = state().read().unwrap();
    if s.db.contains_key(key) {
        "db"
    } else if s.remote.contains_key(key) {
        "head"
    } else {
        "default"
    }
}

/// The value now. Unknown keys panic in debug builds (a typo at a call site) and read 0 in release.
pub fn get(key: &str) -> f64 {
    let Some(k) = knob(key) else {
        debug_assert!(false, "unknown tunable {key}");
        return 0.0;
    };
    if let Some(v) = env_value(key) {
        return v.clamp(k.min, k.max);
    }
    let s = state().read().unwrap();
    s.db.get(key).or_else(|| s.remote.get(key)).copied().unwrap_or(k.default)
}

pub fn secs(key: &str) -> Duration {
    Duration::from_secs_f64(get(key).max(0.0))
}

pub fn n(key: &str) -> u64 {
    get(key).max(0.0).round() as u64
}

/// Check one value against the registry.
pub fn validate(key: &str, v: f64) -> Result<(), String> {
    let k = knob(key).ok_or_else(|| format!("unknown setting {key}"))?;
    if !v.is_finite() || v < k.min || v > k.max {
        return Err(format!("{key} must be between {} and {} {}", k.min, k.max, k.unit));
    }
    Ok(())
}

/// Head: load the stored overrides (store meta "tune"). Bad or unknown entries are skipped.
pub fn load(json: Option<&str>) {
    let m: HashMap<String, f64> = json.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
    state().write().unwrap().db = m.into_iter().filter(|(k, v)| validate(k, *v).is_ok()).collect();
}

/// Head: set or reset (None) values; returns the JSON to store in meta "tune". All or nothing.
pub fn set(changes: &[(String, Option<f64>)]) -> Result<String, String> {
    for (k, v) in changes {
        match v {
            Some(v) => validate(k, *v)?,
            None => drop(knob(k).ok_or_else(|| format!("unknown setting {k}"))?),
        }
    }
    let mut s = state().write().unwrap();
    for (k, v) in changes {
        match v {
            Some(v) => s.db.insert(k.clone(), *v),
            None => s.db.remove(k),
        };
    }
    Ok(serde_json::to_string(&s.db).unwrap_or_default())
}

/// Head: the agent-scope values to send in CTRL {"tune": ...} (only ones that differ from the default).
pub fn agent_values() -> serde_json::Map<String, serde_json::Value> {
    KNOBS
        .iter()
        .filter(|k| k.scope != Head)
        .filter_map(|k| {
            let v = get(k.key);
            (v != k.default).then(|| (k.key.to_string(), v.into()))
        })
        .collect()
}

/// Agent: replace the values the head sent (valid agent-scope keys only).
pub fn apply_remote(v: &serde_json::Value) {
    let m = v.as_object().map(|o| {
        o.iter()
            .filter_map(|(k, x)| Some((k.clone(), x.as_f64()?)))
            .filter(|(k, x)| knob(k).is_some_and(|kn| kn.scope != Head) && validate(k, *x).is_ok())
            .collect()
    });
    state().write().unwrap().remote = m.unwrap_or_default();
}

/// For GET /api/v1/tunables.
pub fn listing() -> Vec<serde_json::Value> {
    KNOBS
        .iter()
        .map(|k| {
            serde_json::json!({"key": k.key, "group": k.group, "value": get(k.key), "default": k.default, "min": k.min, "max": k.max,
                               "unit": k.unit, "scope": k.scope, "help": k.help, "source": source(k.key)})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sane() {
        let mut seen = std::collections::HashSet::new();
        for k in KNOBS {
            assert!(seen.insert(k.key), "duplicate {}", k.key);
            assert!(k.min <= k.default && k.default <= k.max, "{} default out of range", k.key);
            assert!(!k.help.is_empty() && !k.group.is_empty());
        }
    }

    #[test]
    fn set_validate_reset_and_agent_values() {
        assert!(set(&[("lost_after_s".into(), Some(0.1))]).is_err(), "below min");
        assert!(set(&[("nope".into(), Some(1.0))]).is_err(), "unknown");
        assert_eq!(get("pipeline"), 2.0);
        let j = set(&[("pipeline".into(), Some(4.0)), ("calib_s".into(), Some(5.0))]).unwrap();
        assert!(j.contains("pipeline"));
        assert_eq!((get("pipeline"), n("pipeline"), source("pipeline")), (4.0, 4, "db"));
        let a = agent_values();
        assert!(a.contains_key("calib_s") && !a.contains_key("pipeline"), "only agent-scope values go to agents");
        set(&[("pipeline".into(), None), ("calib_s".into(), None)]).unwrap();
        assert_eq!((get("pipeline"), source("pipeline")), (2.0, "default"));
        assert_eq!(secs("heartbeat_s"), Duration::from_secs(1));
    }
}
