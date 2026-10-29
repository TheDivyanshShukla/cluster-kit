//! Agent E: typed rule context, temporal alert state, backtests.
//!
//! One inference output (`FrameSummary`) is evaluated by every rule. Time is MEDIA time
//! (`media_time_us`), never frame counts: a hold of 10 s means the condition was true over
//! >= 10 s of elapsed media time regardless of FPS, frame dropping or worker speed.
//!
//! Machine state (`Pending -> Active -> Resolved`) and the human `Decision` are separate:
//! machine updates copy the stored alert and touch only machine fields.
//!
//! Key per state machine: (rule id, source id, media id, zone). `media_id` is part of the key
//! because media time restarts at 0 in every file of one source.
//!
//! Only `Active` and `Resolved` alerts are emitted; `Pending` is internal (it would create
//! rows that never become alerts).

mod lua;
#[cfg(test)]
mod tests;

use crate::watch::config;
use crate::watch::domain::*;
use crate::watch::rules::{self, Compiled};
use lua::LuaRule;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const DEFAULT_MAX_GAP_MS: i64 = 5_000;
const MAX_EVIDENCE: usize = 64;
const MAX_DIAGS: usize = 512;

/// What an Unknown observation (blurry, class not analyzed, relation unsupported, rule error)
/// does to a hold window. Unknown is NEVER treated as false-zero ("count == 0").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownPolicy {
    /// Default. Unknown breaks a pending window and resolves an active alert: absence of
    /// evidence cannot extend a hold.
    #[default]
    Break,
    /// Unknown is tolerated while the gap since the last KNOWN observation is <= `max_gap_ms`
    /// (legacy "frozen frame" behaviour; used by `from_legacy`).
    Hold,
    /// Unknown observations are discarded as if never received.
    Ignore,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    /// Empty = every source.
    #[serde(default)]
    pub source_ids: Vec<String>,
    /// None = every zone (and unzoned frames). Some = only frames with exactly this zone.
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Legacy DSL line (`seen`, `count`, `ratio`, `lua:`) evaluated three-valued over FrameSummary counts.
    Dsl(String),
    /// Typed Lua: `return function(ctx) ... end` or a bare expression over `ctx`.
    Lua(String),
}

fn d_true() -> bool { true }
fn d_gap() -> i64 { DEFAULT_MAX_GAP_MS }
fn d_sev() -> String { "warning".into() }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuleSpec {
    pub id: String,
    /// Identity of the semantics. Changing it resets this rule's state explicitly. Use
    /// `fingerprint()` when the head has no better version.
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub scope: Scope,
    pub condition: Condition,
    /// Condition must hold continuously over >= this much elapsed media time.
    pub hold_ms: i64,
    /// After an alert resolves, no new alert starts for this long.
    #[serde(default)]
    pub cooldown_ms: i64,
    #[serde(default = "d_sev")]
    pub severity: String,
    /// Summaries computed with a lower confidence threshold than this are Unknown for the rule.
    #[serde(default)]
    pub min_confidence: f32,
    #[serde(default)]
    pub unknown_policy: UnknownPolicy,
    /// Observations further apart than this (dropped frames, stalls) break the window.
    #[serde(default = "d_gap")]
    pub max_gap_ms: i64,
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Set by `from_legacy`: the frame-count hold was converted, not measured.
    #[serde(default)]
    pub migration_warning: Option<String>,
}

impl RuleSpec {
    /// MIGRATION: a legacy `hold` is a FRAME count. It is converted as `hold * 1000 ms`
    /// (i.e. assumes ~1 sampled frame per second) and `migration_warning` says so. Review it:
    /// the real duration depends on the sampling rate the legacy rule ran at. Legacy rules froze
    /// on unusable frames, hence `UnknownPolicy::Hold`.
    pub fn from_legacy(r: &config::Rule) -> Result<RuleSpec, ApiError> {
        rules::compile(&r.line).map_err(|e| ApiError::new("invalid_request", format!("rule '{}': {e}", r.id)))?;
        let hold_ms = r.hold as i64 * 1000;
        Ok(RuleSpec {
            id: r.id.clone(),
            version: stable_id("rv", &[&r.line, &r.hold.to_string(), &r.camera]),
            name: r.name.clone(),
            scope: Scope { source_ids: if r.camera.is_empty() { vec![] } else { vec![r.camera.clone()] }, zone: None },
            condition: Condition::Dsl(r.line.clone()),
            hold_ms,
            cooldown_ms: 0,
            severity: d_sev(),
            min_confidence: 0.0,
            unknown_policy: UnknownPolicy::Hold,
            max_gap_ms: DEFAULT_MAX_GAP_MS,
            enabled: r.enabled,
            migration_warning: Some(format!(
                "legacy hold={} frames converted to {} ms assuming 1 frame/s; it was a frame count, not seconds. Review hold_ms.",
                r.hold, hold_ms
            )),
        })
    }

    /// Stable hash of everything that changes evaluation. Suitable as `version`.
    pub fn fingerprint(&self) -> String {
        let c = match &self.condition { Condition::Dsl(s) => format!("dsl:{s}"), Condition::Lua(s) => format!("lua:{s}") };
        stable_id("rv", &[&self.id, &c, &self.hold_ms.to_string(), &self.cooldown_ms.to_string(),
            &self.min_confidence.to_string(), &format!("{:?}", self.unknown_policy), &self.max_gap_ms.to_string(),
            &self.scope.source_ids.join(","), self.scope.zone.as_deref().unwrap_or("")])
    }

    /// Compile-check. Call on save and before a backtest.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.id.is_empty() || self.version.is_empty() {
            return Err(ApiError::new("invalid_request", "rule id and version are required"));
        }
        if self.hold_ms < 0 || self.cooldown_ms < 0 || self.max_gap_ms < 0 {
            return Err(ApiError::new("invalid_request", "hold_ms, cooldown_ms and max_gap_ms must be >= 0"));
        }
        build(&self.condition).map(|_| ()).map_err(|e| ApiError::new("invalid_request", format!("rule '{}': {e}", self.id)))
    }
}

enum Cond {
    Dsl(Compiled),
    Lua(Box<LuaRule>),
    Broken,
}

fn build(c: &Condition) -> Result<Cond, String> {
    match c {
        Condition::Dsl(line) => rules::compile(line).map(Cond::Dsl),
        Condition::Lua(src) => LuaRule::new(src).map(|l| Cond::Lua(Box::new(l))),
    }
}

/// Structured, never-panicking rule problem. Coalesced when identical and consecutive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub rule_id: String,
    pub rule_version: String,
    pub source_id: String,
    pub media_time_us: Us,
    /// "error" (rule failed, fail closed) | "unknown" (input missing/unsupported).
    pub level: String,
    /// "budget_exceeded" | "memory_limit" | "rule_error" | "compile_error" | "unknown" | "unsupported"
    /// | "zero_denominator" | "low_quality" | "low_confidence" | "unknown_input"
    pub code: String,
    pub message: String,
    pub count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Idle,
    Pending,
    Active,
}

/// Serializable per-(rule, source, media, zone) state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyState {
    phase: Phase,
    start_us: Us,
    last_obs_us: Option<Us>,
    last_known_us: Option<Us>,
    last_true_us: Us,
    cooldown_until_us: Option<Us>,
    evidence: Vec<String>,
    alert_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    /// rule id -> version the states were produced under.
    pub versions: BTreeMap<String, String>,
    pub states: Vec<([String; 4], KeyState)>,
    pub alerts: Vec<Alert>,
}

enum Tr {
    Activated { id: String, start: Us, evidence: Vec<String> },
    Resolved { id: String, start: Us, end: Us, evidence: Vec<String> },
}

pub struct Engine {
    specs: Vec<RuleSpec>,
    rt: HashMap<String, (String, Cond)>, // rule id -> (version, compiled)
    states: BTreeMap<[String; 4], KeyState>,
    alerts: BTreeMap<String, Alert>,
    diags: Vec<Diagnostic>,
    clock: fn() -> i64,
}

fn wall_us() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

impl Engine {
    pub fn new(rules: Vec<RuleSpec>) -> Engine {
        let mut e = Engine { specs: vec![], rt: HashMap::new(), states: BTreeMap::new(), alerts: BTreeMap::new(), diags: vec![], clock: wall_us };
        e.set_rules(rules);
        e
    }

    /// Replace `updated_utc_us` source (tests / deterministic backtests).
    pub fn with_clock(mut self, f: fn() -> i64) -> Engine {
        self.clock = f;
        self
    }

    /// Install a new rule set. A rule whose version changed, that was removed or disabled has
    /// its state reset EXPLICITLY: pending windows are dropped, active alerts are resolved
    /// (end = last true observation) and returned for persistence. Unchanged rules keep their
    /// compiled script and state.
    pub fn set_rules(&mut self, rules: Vec<RuleSpec>) -> Vec<Alert> {
        let old: HashMap<String, RuleSpec> = self.specs.drain(..).map(|s| (s.id.clone(), s)).collect();
        let mut reset: Vec<String> = vec![];
        for (id, o) in &old {
            match rules.iter().find(|r| &r.id == id) {
                Some(n) if n.version == o.version && n.enabled => {}
                _ => reset.push(id.clone()),
            }
        }
        let mut out = self.reset_rules(&reset);
        self.rt.retain(|id, _| !reset.contains(id));
        for r in &rules {
            if !self.rt.contains_key(&r.id) {
                let cond = match build(&r.condition) {
                    Ok(c) => c,
                    Err(e) => {
                        self.diag(r, "", 0, "error", "compile_error", e);
                        Cond::Broken
                    }
                };
                self.rt.insert(r.id.clone(), (r.version.clone(), cond));
            }
        }
        self.specs = rules;
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn reset_rules(&mut self, ids: &[String]) -> Vec<Alert> {
        let keys: Vec<[String; 4]> = self.states.keys().filter(|k| ids.contains(&k[0])).cloned().collect();
        let mut out = vec![];
        for k in keys {
            let st = self.states.remove(&k).unwrap();
            if st.phase == Phase::Active {
                if let Some(id) = &st.alert_id {
                    out.extend(self.apply(&k, Tr::Resolved { id: id.clone(), start: st.start_us, end: st.last_true_us, evidence: st.evidence.clone() }, None));
                }
            }
        }
        out
    }

    /// Feed one inference output to every rule. Returns alert state transitions only.
    /// Re-observing an observation (retry, failover replay, restart replay) is a no-op.
    pub fn observe(&mut self, s: &FrameSummary) -> Vec<Alert> {
        let sum = Arc::new(s.clone());
        let specs = std::mem::take(&mut self.specs);
        let mut out = vec![];
        for spec in &specs {
            if !spec.enabled
                || (!spec.scope.source_ids.is_empty() && !spec.scope.source_ids.contains(&s.source_id))
                || spec.scope.zone.as_ref().is_some_and(|z| s.zone.as_ref() != Some(z))
            {
                continue;
            }
            let key = [spec.id.clone(), s.source_id.clone(), s.media_id.clone(), s.zone.clone().unwrap_or_default()];
            if self.states.get(&key).and_then(|st| st.last_obs_us).is_some_and(|l| s.media_time_us <= l) {
                continue; // duplicate or out-of-order: nothing re-evaluated, no diagnostics
            }
            let res = self.evaluate(spec, &sum);
            let st = self.states.entry(key.clone()).or_default();
            for tr in advance(st, spec, s, res) {
                out.extend(self.apply(&key, tr, Some((spec, s))));
            }
        }
        self.specs = specs;
        out
    }

    /// End of stream / graceful stop of a backtest: resolve every active alert at its last true
    /// observation. Do NOT call on a live restart; use `snapshot()` instead.
    pub fn flush(&mut self) -> Vec<Alert> {
        let ids: Vec<String> = self.specs.iter().map(|s| s.id.clone()).collect();
        self.reset_rules(&ids)
    }

    fn evaluate(&mut self, spec: &RuleSpec, s: &Arc<FrameSummary>) -> Option<bool> {
        if s.quality != Quality::Ok {
            self.diag(spec, &s.source_id, s.media_time_us, "unknown", "low_quality", format!("frame quality is {:?}", s.quality));
            return None;
        }
        if s.min_confidence < spec.min_confidence {
            self.diag(spec, &s.source_id, s.media_time_us, "unknown", "low_confidence",
                format!("summary threshold {} is below the rule's {}", s.min_confidence, spec.min_confidence));
            return None;
        }
        let Some((_, cond)) = self.rt.get_mut(&spec.id) else { return None };
        let (res, diags): (Result<Option<bool>, String>, Vec<(String, String)>) = match cond {
            Cond::Broken => return None,
            Cond::Dsl(c) => {
                let r = c.eval_summary(s);
                let mut d = vec![];
                if matches!(r, Ok(None)) {
                    let missing: Vec<&str> = c.classes.iter().filter(|k| s.class(k).known().is_none()).map(String::as_str).collect();
                    d.push(("unknown_input".to_string(), format!("not analyzed or unsupported: {missing:?}")));
                }
                (r, d)
            }
            Cond::Lua(l) => l.eval(s),
        };
        for (code, msg) in diags {
            self.diag(spec, &s.source_id, s.media_time_us, "unknown", &code, msg);
        }
        match res {
            Ok(v) => v,
            Err(e) => {
                let code = match e.as_str() {
                    "budget exceeded" => "budget_exceeded",
                    "memory limit exceeded" => "memory_limit",
                    _ => "rule_error",
                };
                self.diag(spec, &s.source_id, s.media_time_us, "error", code, e);
                None // fail closed: an erroring rule never creates an alert
            }
        }
    }

    fn diag(&mut self, spec: &RuleSpec, source: &str, t: Us, level: &str, code: &str, message: String) {
        if let Some(l) = self.diags.last_mut() {
            if l.rule_id == spec.id && l.rule_version == spec.version && l.code == code && l.message == message {
                l.count += 1;
                l.media_time_us = t;
                return;
            }
        }
        if self.diags.len() >= MAX_DIAGS {
            self.diags.remove(0);
        }
        self.diags.push(Diagnostic {
            rule_id: spec.id.clone(), rule_version: spec.version.clone(), source_id: source.into(), media_time_us: t,
            level: level.into(), code: code.into(), message, count: 1,
        });
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diags
    }
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diags)
    }

    fn apply(&mut self, key: &[String; 4], tr: Tr, ctx: Option<(&RuleSpec, &FrameSummary)>) -> Option<Alert> {
        let now = (self.clock)();
        let spec = ctx.map(|c| c.0).or_else(|| self.specs.iter().find(|s| s.id == key[0]));
        let (id, start, end, evidence, active) = match tr {
            Tr::Activated { id, start, evidence } => (id, start, None, evidence, true),
            Tr::Resolved { id, start, end, evidence } => (id, start, Some(end), evidence, false),
        };
        let mut a = match self.alerts.get(&id) {
            Some(a) => a.clone(), // keeps decision + actor: machine updates never overwrite them
            None => Alert {
                id: id.clone(), rule_id: key[0].clone(),
                rule_version: spec.map(|s| s.version.clone()).unwrap_or_default(),
                source_id: key[1].clone(), media_id: Some(key[2].clone()), start_us: start, end_us: None,
                clock: ClockDomain::Media { media_id: key[2].clone() },
                values: json!({}), severity: spec.map(|s| s.severity.clone()).unwrap_or_default(),
                evidence: vec![], state: AlertState::Active, decision: Decision::Unreviewed, actor: None, updated_utc_us: now,
            },
        };
        if let Some(sp) = spec {
            a.values = json!({
                "rule": sp.name, "hold_ms": sp.hold_ms, "cooldown_ms": sp.cooldown_ms, "unknown_policy": sp.unknown_policy,
                "held_us": end.map(|e| e - start),
                "capture_time_utc_us": ctx.and_then(|c| c.1.capture_time_utc_us),
            });
        }
        a.evidence = evidence;
        a.end_us = end;
        a.state = if active { AlertState::Active } else { AlertState::Resolved };
        a.updated_utc_us = now;
        // ponytail: alerts kept in memory unbounded; head persists via Store and can prune resolved+decided ones.
        self.alerts.insert(id, a.clone());
        Some(a)
    }

    /// Human decision. Stored apart from machine state; later machine updates keep it.
    pub fn decide(&mut self, alert_id: &str, d: Decision, actor: &str) -> Result<Alert, ApiError> {
        let now = (self.clock)();
        let a = self.alerts.get_mut(alert_id).ok_or_else(|| ApiError::new("not_found", format!("alert {alert_id} not found")))?;
        apply_decision(a, d, actor, now)?;
        Ok(a.clone())
    }

    /// Alert known to the engine (live/active or recently resolved).
    pub fn alert(&self, id: &str) -> Option<&Alert> {
        self.alerts.get(id)
    }
    pub fn alerts(&self) -> Vec<Alert> {
        self.alerts.values().cloned().collect()
    }
    /// Seed an alert loaded from the store (after a restart without a snapshot) so `decide` works on it.
    pub fn adopt(&mut self, a: Alert) {
        self.alerts.entry(a.id.clone()).or_insert(a);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            schema: 1,
            versions: self.specs.iter().map(|s| (s.id.clone(), s.version.clone())).collect(),
            states: self.states.iter().filter(|(_, s)| s.phase != Phase::Idle || s.cooldown_until_us.is_some() || s.last_obs_us.is_some())
                .map(|(k, v)| (k.clone(), v.clone())).collect(),
            alerts: self.alerts.values().cloned().collect(),
        }
    }

    /// Restore after restart. State of a rule whose version differs from the snapshot's is
    /// dropped explicitly and its active alerts are resolved and returned. Alerts keep their IDs
    /// and decisions, so replaying uncommitted frames cannot duplicate them.
    pub fn restore(&mut self, snap: Snapshot) -> Vec<Alert> {
        let mut stale: Vec<String> = vec![];
        for (id, v) in &snap.versions {
            if self.specs.iter().find(|s| &s.id == id && s.enabled).map(|s| &s.version) != Some(v) {
                stale.push(id.clone());
            }
        }
        for a in snap.alerts {
            self.alerts.entry(a.id.clone()).or_insert(a);
        }
        for (k, st) in snap.states {
            self.states.insert(k, st);
        }
        self.reset_rules(&stale)
    }
}

/// Pure so the head can apply it to an alert it only holds in the store.
pub fn apply_decision(a: &mut Alert, d: Decision, actor: &str, now_us: Us) -> Result<(), ApiError> {
    if actor.trim().is_empty() {
        return Err(ApiError::new("invalid_request", "a decision needs an actor"));
    }
    a.decision = d;
    a.actor = Some(actor.to_string());
    a.updated_utc_us = now_us;
    Ok(())
}

fn push_ev(st: &mut KeyState, id: &str) {
    if st.evidence.len() < MAX_EVIDENCE {
        st.evidence.push(id.to_string());
    } else {
        *st.evidence.last_mut().unwrap() = id.to_string(); // keep first 63 + latest
    }
}

fn close(st: &mut KeyState, spec: &RuleSpec, out: &mut Vec<Tr>) {
    if st.phase == Phase::Active {
        st.cooldown_until_us = Some(st.last_true_us + spec.cooldown_ms.max(0) * 1000);
        if let Some(id) = st.alert_id.take() {
            out.push(Tr::Resolved { id, start: st.start_us, end: st.last_true_us, evidence: std::mem::take(&mut st.evidence) });
        }
    }
    st.phase = Phase::Idle;
    st.evidence.clear();
    st.alert_id = None;
}

/// The temporal state machine. `res`: Some(true/false) known, None Unknown.
fn advance(st: &mut KeyState, spec: &RuleSpec, s: &FrameSummary, res: Option<bool>) -> Vec<Tr> {
    let t = s.media_time_us;
    let mut out = vec![];
    if res.is_none() && spec.unknown_policy == UnknownPolicy::Ignore {
        return out;
    }
    if st.last_obs_us.is_some_and(|l| t <= l) {
        return out;
    }
    let max_gap = spec.max_gap_ms.max(0) * 1000;
    if st.phase != Phase::Idle && st.last_known_us.is_some_and(|k| t - k > max_gap) {
        close(st, spec, &mut out); // dropped frames / stall: the window is not continuous
    }
    match res {
        Some(true) => {
            if st.phase == Phase::Idle && !st.cooldown_until_us.is_some_and(|c| t < c) {
                *st = KeyState { phase: Phase::Pending, start_us: t, cooldown_until_us: st.cooldown_until_us, ..KeyState::default() };
            }
            if st.phase != Phase::Idle {
                st.last_true_us = t;
                push_ev(st, &s.observation_id);
                if st.phase == Phase::Pending && t - st.start_us >= spec.hold_ms.max(0) * 1000 {
                    st.phase = Phase::Active;
                    let id = stable_id("alr", &[&spec.id, &spec.version, &s.source_id, &s.media_id, s.zone.as_deref().unwrap_or(""), &st.start_us.to_string()]);
                    st.alert_id = Some(id.clone());
                    out.push(Tr::Activated { id, start: st.start_us, evidence: st.evidence.clone() });
                }
            }
            st.last_known_us = Some(t);
        }
        Some(false) => {
            close(st, spec, &mut out);
            st.last_known_us = Some(t);
        }
        None => {
            if spec.unknown_policy == UnknownPolicy::Break {
                close(st, spec, &mut out);
            }
        }
    }
    st.last_obs_us = Some(t);
    out
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BacktestReport {
    pub alerts: Vec<Alert>,
    pub diagnostics: Vec<Diagnostic>,
    pub observations: u64,
}

/// Replay stored summaries (media-time order per media) through a fresh engine. The same
/// state machine as live, so a backtest reproduces live behaviour. Alerts left open at the end
/// of data are closed at their last true observation.
pub fn backtest_report(store: &dyn Store, spec: &RuleSpec, scope: &QueryScope) -> Result<BacktestReport, ApiError> {
    spec.validate()?;
    let mut spec = spec.clone();
    spec.enabled = true;
    let mut sources = scope.source_ids.clone();
    if !spec.scope.source_ids.is_empty() {
        sources = if sources.is_empty() { spec.scope.source_ids.clone() } else { sources.into_iter().filter(|s| spec.scope.source_ids.contains(s)).collect() };
        if sources.is_empty() {
            return Ok(BacktestReport::default()); // scopes do not intersect
        }
    }
    let filter = DetectionFilter {
        source_ids: sources, media_ids: scope.media_ids.clone(),
        utc_from_us: scope.from_utc_us, utc_to_us: scope.to_utc_us, ..Default::default()
    };
    let mut eng = Engine::new(vec![spec]).with_clock(|| 0);
    let mut seen: BTreeMap<String, Alert> = BTreeMap::new();
    let mut after: Option<String> = None;
    let mut n = 0u64;
    loop {
        let (rows, next) = store.summaries(&filter, &Page { after: after.clone(), limit: 1000 })?;
        for r in &rows {
            n += 1;
            for a in eng.observe(r) {
                seen.insert(a.id.clone(), a);
            }
        }
        match next {
            Some(c) if !rows.is_empty() && Some(&c) != after.as_ref() => after = Some(c),
            _ => break,
        }
    }
    for a in eng.flush() {
        seen.insert(a.id.clone(), a);
    }
    let mut alerts: Vec<Alert> = seen.into_values().collect();
    alerts.sort_by(|a, b| (&a.media_id, a.start_us, &a.id).cmp(&(&b.media_id, b.start_us, &b.id)));
    Ok(BacktestReport { alerts, diagnostics: eng.take_diagnostics(), observations: n })
}

pub fn backtest(store: &dyn Store, spec: &RuleSpec, scope: &QueryScope) -> Result<Vec<Alert>, ApiError> {
    backtest_report(store, spec, scope).map(|r| r.alerts)
}
