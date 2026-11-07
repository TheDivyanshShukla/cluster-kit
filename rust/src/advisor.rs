//! Advisor (docs/ADVISOR.md): optimisation suggestions grounded in measured data. Everything here is pure: `advise` turns
//! one observation into suggestions with ranked options, `Engine` adds hysteresis, `track` compares predictions with what
//! was observed later, `auto_pick` decides what the policy lets the advisor do alone. The head side (gathering the
//! observation, applying an action through the normal endpoints, storing `Persist`) lives in api.rs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------- types

/// What one click does. Every variant except `Doc` goes through an existing endpoint (api.rs `apply_action`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Tune { key: String, from: f64, to: f64 },
    NodeState { node_ids: Vec<String>, from: String, to: String },
    HaTarget { from: u32, to: u32 },
    Power { from: String, to: String },
    Doc { link: String },
}

impl Action {
    /// The action that restores the old value (None for `Doc`).
    pub fn inverse(&self) -> Option<Action> {
        Some(match self {
            Action::Tune { key, from, to } => Action::Tune { key: key.clone(), from: *to, to: *from },
            Action::NodeState { node_ids, from, to } => Action::NodeState { node_ids: node_ids.clone(), from: to.clone(), to: from.clone() },
            Action::HaTarget { from, to } => Action::HaTarget { from: *to, to: *from },
            Action::Power { from, to } => Action::Power { from: to.clone(), to: from.clone() },
            Action::Doc { .. } => return None,
        })
    }
    pub fn changes(&self) -> bool {
        !matches!(self, Action::Doc { .. })
    }
}

/// One way to deal with a situation, with what it should do, what it costs and how to tell it worked.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Opt {
    pub id: String,
    pub title: String,
    pub action: Option<Action>,
    /// Manual steps when the system cannot do it (or in addition to the action).
    pub steps: Vec<String>,
    pub expected_effect: String,
    /// Name of the measured number (`metrics`) this should move, from where to where. Checked after the follow-up window.
    pub metric: Option<String>,
    pub metric_from: f64,
    pub metric_to: f64,
    pub cost: String,
    /// low | med | high
    pub risk: String,
    pub reversible: bool,
    /// Only reversible, bounded changes may run under policy `auto`.
    pub auto_safe: bool,
    pub time_to_effect: String,
    /// How the estimate was made: measured / model / rule of thumb, with the input numbers.
    pub confidence: String,
    pub success_check: String,
    pub recommended: bool,
    /// Effect per cost, higher is better; options are ranked by it.
    pub score: f64,
}

impl Opt {
    pub fn new(id: &str, title: &str) -> Opt {
        Opt { id: id.into(), title: title.into(), risk: "low".into(), reversible: true, time_to_effect: "next job".into(), ..Default::default() }
    }
    fn act(mut self, a: Action) -> Self {
        self.action = Some(a);
        self
    }
    fn steps(mut self, s: &[&str]) -> Self {
        self.steps = s.iter().map(|x| x.to_string()).collect();
        self
    }
    fn eff(mut self, s: String) -> Self {
        self.expected_effect = s;
        self
    }
    fn metric(mut self, m: &str, from: f64, to: f64) -> Self {
        (self.metric, self.metric_from, self.metric_to) = (Some(m.into()), from, to);
        self
    }
    fn cost(mut self, s: &str) -> Self {
        self.cost = s.into();
        self
    }
    fn risk(mut self, r: &str, reversible: bool) -> Self {
        (self.risk, self.reversible) = (r.into(), reversible);
        self
    }
    fn auto(mut self) -> Self {
        self.auto_safe = true;
        self
    }
    fn tte(mut self, s: &str) -> Self {
        self.time_to_effect = s.into();
        self
    }
    fn conf(mut self, s: String) -> Self {
        self.confidence = s;
        self
    }
    fn check(mut self, s: &str) -> Self {
        self.success_check = s.into();
        self
    }
    fn score(mut self, s: f64) -> Self {
        self.score = s;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    /// Stable: kind plus target ("flaky_pc:lab-07").
    pub id: String,
    pub kind: String,
    /// info | tip | warn
    pub severity: String,
    pub title: String,
    /// The numbers that triggered it.
    pub why: String,
    pub options: Vec<Opt>,
    /// Copies of the recommended option's effect and action (the one-click summary).
    pub effect: String,
    pub action: Option<Action>,
    pub since_ms: i64,
    /// off | suggest | auto, filled in by the engine.
    #[serde(default)]
    pub policy: String,
    /// Share of past predictions of this rule that held (None = fewer than 3 outcomes).
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(skip)]
    pub hold_s: f64,
}

pub fn do_nothing(consequence: String) -> Opt {
    Opt::new("nothing", "Do nothing").eff(consequence).cost("none").tte("n/a").conf("rule of thumb".into()).check("the situation text above stays or clears by itself")
}

/// Best effect per cost first, "do nothing" always last; the first real option is recommended.
pub fn rank(opts: &mut Vec<Opt>) {
    opts.sort_by(|a, b| (a.id == "nothing").cmp(&(b.id == "nothing")).then(b.score.total_cmp(&a.score)));
    for (i, o) in opts.iter_mut().enumerate() {
        o.recommended = i == 0 && o.id != "nothing";
    }
}

fn sug(id: String, kind: &str, sev: &str, title: String, why: String, hold_s: f64, mut options: Vec<Opt>, nothing: String) -> Suggestion {
    options.push(do_nothing(nothing));
    rank(&mut options);
    let rec = options.iter().find(|o| o.recommended);
    Suggestion {
        id, kind: kind.into(), severity: sev.into(), title, why, effect: rec.map(|o| o.expected_effect.clone()).unwrap_or_default(),
        action: rec.and_then(|o| o.action.clone()), options, since_ms: 0, policy: String::new(), accuracy: None, hold_s,
    }
}

// ---------------------------------------------------------------- observation

#[derive(Clone, Debug, Default)]
pub struct Bd {
    pub wall: f64,
    pub compute: f64,
    pub dispatch: f64,
    pub tail: f64,
    pub recompute: f64,
    pub failover: f64,
    pub other: f64,
    pub chunk_p50: f64,
    pub chunks: u64,
    pub stragglers: Vec<String>,
}

impl Bd {
    pub fn total(&self) -> f64 {
        self.compute + self.dispatch + self.tail + self.recompute + self.failover + self.other
    }
}

#[derive(Clone, Debug, Default)]
pub struct AdvJob {
    pub id: u64,
    pub running: bool,
    pub user: String,
    /// quota | priority | no_pcs
    pub wait: Option<String>,
    pub failed: bool,
    pub error: Option<String>,
    pub finished_ms: i64,
    pub blobs: usize,
    pub blob_mode: String,
    pub bd: Option<Bd>,
}

#[derive(Clone, Debug, Default)]
pub struct AdvNode {
    pub id: String,
    pub host: String,
    pub state: String,
    pub online: bool,
    pub busy: bool,
    pub losses_24h: u32,
}

#[derive(Clone, Debug, Default)]
pub struct AdvObs {
    /// Current value of every tunable.
    pub tun: HashMap<String, f64>,
    pub jobs: Vec<AdvJob>,
    pub nodes: Vec<AdvNode>,
    /// Open alerts: (kind, node id, detail).
    pub alerts: Vec<(String, Option<String>, String)>,
    /// Warn/error log kinds of the last hour: (kind, count, last message).
    pub warns: Vec<(String, u32, String)>,
    /// Counts of head events in the last hour (node.denied, task.timeout).
    pub events: HashMap<String, u32>,
    pub standbys: usize,
    pub target_standbys: usize,
    /// Best eligible PC that is not yet a standby: (node id, host, score).
    pub best_head: Option<(String, String, f64)>,
    /// Head data disk: (used percent, free GB).
    pub disk: Option<(f64, f64)>,
    pub power_mode: String,
    pub keep_awake: u32,
    pub blob_sent_gb: f64,
    pub hier: HierObs,
}

/// What the advisor knows about the two-level hierarchy (docs/HIERARCHY.md).
#[derive(Clone, Debug, Default)]
pub struct HierObs {
    /// hier_mode: 0 off, 1 manual, 2 auto
    pub mode: u8,
    /// (lab, PCs in it) for every lab the root knows PCs of
    pub labs: Vec<(String, usize)>,
    pub heads: usize,
    /// Labs with a pinned lab head that is partitioned, released or absent: (lab, state)
    pub bad: Vec<(String, String)>,
    /// `labhead.leave` / `labhead.partition` events in the last hour
    pub leaves_1h: u32,
    pub partitions_1h: u32,
}

impl AdvObs {
    fn t(&self, k: &str) -> f64 {
        self.tun.get(k).copied().unwrap_or_else(|| crate::tune::knob(k).map_or(0.0, |k| k.default))
    }
    fn alert(&self, kind: &str) -> bool {
        self.alerts.iter().any(|a| a.0 == kind)
    }
    fn online(&self) -> usize {
        self.nodes.iter().filter(|n| n.online && n.state == "active").count()
    }
    fn node(&self, id: &str) -> Option<&AdvNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Bounded, tidy new value for a tunable.
fn bounded(key: &str, v: f64) -> f64 {
    let v = crate::tune::knob(key).map_or(v, |k| v.clamp(k.min, k.max));
    if v >= 100.0 { v.round() } else { (v * 100.0).round() / 100.0 }
}

struct Shares {
    dispatch: f64,
    recompute: f64,
    tail: f64,
    pc_s: f64,
    jobs: usize,
}

fn shares(o: &AdvObs) -> Option<Shares> {
    let (mut t, mut d, mut r, mut l, mut n) = (0.0, 0.0, 0.0, 0.0, 0);
    for b in o.jobs.iter().filter_map(|j| j.bd.as_ref()).filter(|b| b.wall >= 20.0 && b.chunks >= 10) {
        t += b.total();
        d += b.dispatch;
        r += b.recompute;
        l += b.tail;
        n += 1;
    }
    (t > 0.0).then_some(Shares { dispatch: d / t, recompute: r / t, tail: l / t, pc_s: t, jobs: n })
}

/// The numbers follow-ups are checked against (name -> value now).
pub fn metrics(o: &AdvObs) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    if let Some(s) = shares(o) {
        m.insert("dispatch_share".into(), s.dispatch);
        m.insert("recompute_share".into(), s.recompute);
        m.insert("tail_share".into(), s.tail);
    }
    m.insert("idle_pcs".into(), o.nodes.iter().filter(|n| n.online && n.state == "active" && !n.busy).count() as f64);
    m.insert("flaky_nodes".into(), o.nodes.iter().filter(|n| n.losses_24h as f64 > o.t("alert_flaky_losses")).count() as f64);
    m.insert("losses_24h".into(), o.nodes.iter().map(|n| n.losses_24h as f64).sum());
    if let Some((used, _)) = o.disk {
        m.insert("disk_used_pct".into(), used);
    }
    m
}

fn pc(x: f64) -> String {
    format!("{:.0} %", x * 100.0)
}

// ---------------------------------------------------------------- rules

/// Alert kinds research.rs can open; the advisor must have a rule for each (test `every_alert_kind_has_a_rule`).
#[cfg(test)]
pub const ALERT_KINDS: &[&str] = &["slow_pc", "flaky_pc", "head_hot", "standby_behind", "reserve_at_risk", "no_standby"];

/// Job failure reasons: (needle in the error text lower-cased, reason id, title).
const FAIL_REASONS: &[(&str, &str, &str)] = &[
    ("output", "output_cap", "A chunk printed more than the output cap"),
    ("tries", "attempts", "A range failed on every try"),
    ("attempt", "attempts", "A range failed on every try"),
    ("took too long", "timeout", "A chunk ran past its time limit"),
    ("timeout", "timeout", "A chunk ran past its time limit"),
    ("blob", "blob", "A job file (blob) could not be fetched or verified"),
    ("no pc", "no_pcs", "No PC matches the job's selector"),
    ("selector", "no_pcs", "No PC matches the job's selector"),
    ("quota", "quota", "The job hit its owner's quota"),
];

/// Warn/error log kinds and head events: (kind, title, steps, doc). The fix is a manual step the head cannot do itself.
pub const LOG_FIXES: &[(&str, &str, &[&str], &str)] = &[
    ("store.corrupt", "The database failed its integrity check", &["Stop the head, run `ck admin backup` is not possible on a damaged file: copy the newest file from ck-data/backups over head.db", "Start the head; a standby (if any) takes over meanwhile"], "docs/OPERATIONS.md"),
    ("store.missing", "The database file was missing at start", &["Restore the newest backup into CK_DATA/head.db, or fail over to a standby", "Check that CK_DATA points at the same disk as before"], "docs/OPERATIONS.md"),
    ("job.error", "The head could not store a result or job change", &["Check free disk space on the head (see the disk suggestion)", "Results are queued again automatically; nothing is lost while the agent holds them"], "docs/OPERATIONS.md"),
    ("task.timeout", "Chunks took too long and were reissued", &["Use a smaller chunk size for that job so one chunk takes less time", "Check the named PC for load (a person using it, swapping)"], "docs/JOBS.md"),
    ("node.denied", "A PC was refused (bad key or join code)", &["Create a fresh join code (`ck admin join-code`) and re-join the PC", "If the PC is new, approve it in Nodes (state pending -> active)"], "docs/OPERATIONS.md"),
    ("ha.mixed_versions", "Head and PCs run different versions", &["Finish the rolling upgrade: update the remaining PCs to the head's version", "Do not switch over to a standby with an older version"], "docs/OPERATIONS.md"),
    ("blob.corrupt", "A downloaded blob failed its sha256 check", &["Re-upload the file (`PUT /api/v1/blobs`) so the head holds a good copy", "Check the PC's disk and network; the agent re-fetches by itself"], "docs/BULK.md"),
    ("blob.fetch_failed", "A PC could not fetch a blob", &["Check the network between the PC and the head (Network page, probe it)", "Lower blob_peer_fanout if one PC is overloaded serving peers"], "docs/BULK.md"),
    ("blob.port", "The blob port could not be opened", &["Free the TCP port (CK_PORT + 3) or move CK_PORT", "Allow the port through the firewall"], "docs/UBUNTU.md"),
    ("blob.standby_copy_failed", "A standby could not copy blobs", &["Check the standby's disk space and link", "The primary retries every blob_offer_s"], "docs/BULK.md"),
    ("blob.standby_missing", "A standby lacks blobs the primary has", &["Wait one blob_offer_s round; if it stays, check the standby's disk"], "docs/BULK.md"),
    ("power.error", "A power action failed", &["Check wake-on-LAN support on the PC and that the head is on the same network", "Fall back to power mode off for that lab"], "docs/OPERATIONS.md"),
    ("power.sleep_failed", "A PC did not go to sleep", &["Check that the agent runs with rights to suspend", "Set the PC to stay awake (keep_awake) until fixed"], "docs/OPERATIONS.md"),
    ("standby.apply_error", "A standby could not apply a change", &["Let the standby take a full copy: restart it with `ck head --standby`", "Check its disk space"], "docs/ARCHITECTURE.md"),
    ("standby.link_end", "The link to a standby ended", &["Check the standby PC and network; it reconnects by itself"], "docs/ARCHITECTURE.md"),
    ("event.error", "An event could not be stored", &["Check free disk space on the head"], "docs/OPERATIONS.md"),
    ("node.error", "A node record could not be stored", &["Check free disk space on the head"], "docs/OPERATIONS.md"),
];

fn node_state(o: &AdvObs, id: &str, to: &str) -> Option<Action> {
    let n = o.node(id).filter(|n| n.state == "active")?;
    Some(Action::NodeState { node_ids: vec![n.id.clone()], from: n.state.clone(), to: to.into() })
}

pub fn advise(o: &AdvObs, now: i64) -> Vec<Suggestion> {
    let mut out = vec![];
    let sh = shares(o);
    let online = o.online().max(1) as f64;

    // chunks too short / dispatch wait / head hot
    let head_hot = o.alert("head_hot");
    let ds = sh.as_ref().map_or(0.0, |s| s.dispatch);
    if head_hot || ds > 0.15 {
        let (cur, pipe) = (o.t("target_chunk_s"), o.t("pipeline"));
        let new = bounded("target_chunk_s", cur * 2.0);
        let mut opts = vec![];
        let basis = sh.as_ref().map_or("no job with enough chunks yet".to_string(), |s| format!("measured: dispatch wait {} of {:.0} PC-s over {} job(s)", pc(s.dispatch), s.pc_s, s.jobs));
        if new > cur {
            opts.push(Opt::new("chunk_up", &format!("Longer chunks: target_chunk_s {cur} -> {new} s"))
                .act(Action::Tune { key: "target_chunk_s".into(), from: cur, to: new }).auto()
                .eff(format!("dispatch wait {} -> ~{} of PC time, job time about -{:.0} %, head work per number about halves", pc(ds), pc(ds * 0.5), ds * 50.0))
                .metric("dispatch_share", ds, ds * 0.5).cost(&format!("coarser chunks: the tail of each job grows by up to ~{new} s")).tte("jobs started after the change")
                .conf(format!("{basis}; model: wait per number scales with 1/chunk length (rule of thumb)")).check("dispatch_share (breakdown) falls by half within 10 min of work").score(ds * 50.0 + if head_hot { 20.0 } else { 0.0 }));
        }
        let big: Vec<&(String, usize)> = o.hier.labs.iter().filter(|l| l.1 >= o.t("lab_head_min_pcs") as usize).collect();
        if (head_hot || ds > 0.15) && o.hier.mode == 0 && !big.is_empty() {
            let pcs: usize = o.hier.labs.iter().map(|l| l.1).sum();
            let (b1, b2, bl) = crate::sim::hier_model(pcs, big.len(), cur, o.t("lease_s"));
            opts.push(Opt::new("lab_heads_auto", &format!("Lab heads, automatic: hier_mode 0 -> 2 ({} lab(s) of {} PCs or more)", big.len(), o.t("lab_head_min_pcs")))
                .act(Action::Tune { key: "hier_mode".into(), from: 0.0, to: 2.0 })
                .eff(format!("root busy ~{} -> ~{} of one core, each lab head ~{}; the head stops being the limit", pc(b1), pc(b2), pc(bl)))
                .metric("dispatch_share", ds, ds * 0.3).cost("PCs reconnect once (about 2 s); a lab head loss costs ~2 s and a lease of recomputed work; only `--only` jobs pause the hierarchy")
                .risk("med", true).tte("about 10 s").conf(format!("model: sim.rs cost model, {pcs} PCs, chunks {cur} s, lease {} s (docs/sim/HIERARCHY.md); measured on 1 machine in docs/HIERARCHY.md", o.t("lease_s")))
                .check("dispatch_share falls and head_hot clears within 10 min").score(ds * 90.0 + if head_hot { 40.0 } else { 0.0 }));
            opts.push(Opt::new("lab_heads_manual", "Lab heads, by hand: hier_mode 0 -> 1, then pin the PC you want per lab")
                .act(Action::Tune { key: "hier_mode".into(), from: 0.0, to: 1.0 })
                .steps(&["ck admin heads            (best PCs to host a lab head)", "ck admin hier pin LAB NODE_ID   (one per lab)"])
                .eff(format!("same as automatic for the labs you pin: root busy ~{} -> ~{}", pc(b1), pc(b2))).metric("dispatch_share", ds, ds * 0.3)
                .cost("you choose and keep an eye on the lab heads").risk("low", true).tte("minutes").conf("model: sim.rs cost model (docs/sim/HIERARCHY.md)".into())
                .check("ck admin hier shows the lab heads active").score(ds * 60.0));
        }
        if pipe < 16.0 {
            opts.push(Opt::new("pipeline_up", &format!("Queue deeper: pipeline {pipe} -> {}", pipe + 1.0))
                .act(Action::Tune { key: "pipeline".into(), from: pipe, to: pipe + 1.0 }).auto()
                .eff(format!("dispatch wait {} -> ~{} (the next chunk is already on the PC)", pc(ds), pc(ds * 0.7))).metric("dispatch_share", ds, ds * 0.7)
                .cost("one more chunk per PC is lost and recomputed when a PC dies").tte("immediately").conf(format!("{basis}; rule of thumb 30 %")).check("dispatch_share falls by about a third").score(ds * 30.0));
        }
        let why = if head_hot { format!("head CPU is above the alert limit; {}", basis) } else { format!("PCs wait for the head {} of their time; {basis}", pc(ds)) };
        out.push(sug("dispatch_wait".into(), if head_hot { "head_hot" } else { "dispatch_wait" }, "warn", "Chunks are too short for the head".into(), why, 30.0, opts,
            "the head keeps spending its CPU on tiny chunks; PCs keep waiting between them".into()));
        if head_hot {
            out.last_mut().unwrap().id = "head_hot".into();
        }
    }

    // lab heads that are not doing their job, and the lease length
    for (lab, state) in &o.hier.bad {
        let mut opts = vec![];
        let best = o.best_head.as_ref().map(|b| format!("ck admin hier pin {lab} {}   ({}, score {:.2})", b.0, b.1, b.2)).unwrap_or_else(|| format!("ck admin hier pin {lab} NODE_ID   (see ck admin heads)"));
        opts.push(Opt::new("move", "Move the lab head to another PC").steps(&[&best, "the old lab head hands its PCs back and exits; the new one is started on that PC"])
            .eff("the lab's PCs run under a healthy lab head again; until then they work for the root directly".into()).cost("PCs of the lab reconnect once").risk("low", true).tte("about 20 s")
            .conf(format!("measured: the lab head of '{lab}' is {state}")).check("ck admin hier shows the lab 'active'").score(60.0));
        opts.push(Opt::new("unpin", "Unpin it: let the root choose again (auto mode) or leave the lab without a lab head").steps(&[&format!("ck admin hier unpin {lab}")])
            .eff("the lab's PCs stay on the root; with hier_mode auto a new lab head is chosen by the head ranking".into()).cost("none").risk("low", true).tte("seconds").conf("rule of thumb".into()).check("the lab shows a lab head or its PCs at the root").score(40.0));
        out.push(sug(format!("lab_head:{lab}"), "lab_head", "warn", format!("The lab head of '{lab}' is {state}"), format!("pinned lab head for '{lab}' is {state}; its PCs are not served by it"), 20.0, opts,
            "the lab's PCs keep working for the root directly; the root loses the saving".into()));
    }
    {
        let ls = o.t("lease_s");
        if o.hier.mode > 0 && o.hier.leaves_1h > 0 && ls > 60.0 {
            let to = bounded("lease_s", ls / 2.0);
            out.push(sug("lease_long".into(), "lease_length", "tip", format!("Leases are long: {} lab head loss(es) in the last hour", o.hier.leaves_1h),
                format!("lab head lost {} time(s) in the last hour; lease_s is {ls} s; a lost lab head costs about 0.75 s of job time per second of lease (simulated)", o.hier.leaves_1h), 120.0,
                vec![Opt::new("lease_down", &format!("Shorter leases: lease_s {ls} -> {to} s")).act(Action::Tune { key: "lease_s".into(), from: ls, to }).auto()
                    .eff(format!("work redone after a lab head loss falls from ~{ls} s to ~{to} s of the lab's work")).cost("a few more lease messages (root load stays under 2 % in the model)").tte("next lease").conf("model: docs/sim/HIERARCHY.md section 3 (4): loss grows linearly with L".into())
                    .check("the next lab head loss recomputes less (job.breakdown recompute share)").score(35.0)],
                "a lab head loss keeps costing a whole lease of recomputed work".into()));
        }
        if o.hier.mode > 0 && o.hier.partitions_1h > 0 && ls < 60.0 {
            let to = bounded("lease_s", ls * 2.0);
            out.push(sug("lease_short".into(), "lease_length", "tip", format!("Leases are short: {} lab head partition(s) in the last hour", o.hier.partitions_1h),
                format!("a lab head went silent {} time(s) in the last hour (link or root stalls); lease_s is {ls} s, so its PCs idle once the leases run out", o.hier.partitions_1h), 120.0,
                vec![Opt::new("lease_up", &format!("Longer leases: lease_s {ls} -> {to} s")).act(Action::Tune { key: "lease_s".into(), from: ls, to }).auto()
                    .eff(format!("a lab keeps computing through a root outage or partition of up to ~{to} s instead of ~{ls} s")).cost("a lab head loss recomputes up to a lease more").tte("next lease")
                    .conf("model: docs/sim/HIERARCHY.md section 3 (3): work in a partition window grows with L".into()).check("fewer idle PC-seconds during the next partition").score(35.0)],
                "labs run out of leased work during short outages".into()));
        }
    }

    // flaky PCs
    let thr = o.t("alert_flaky_losses");
    let mut flaky: Vec<&AdvNode> = o.nodes.iter().filter(|n| n.losses_24h as f64 > thr).collect();
    for a in o.alerts.iter().filter(|a| a.0 == "flaky_pc") {
        if let Some(n) = a.1.as_deref().and_then(|id| o.node(id)) {
            if !flaky.iter().any(|f| f.id == n.id) {
                flaky.push(n);
            }
        }
    }
    let rs = sh.as_ref().map_or(0.0, |s| s.recompute);
    for n in &flaky {
        let mut opts = vec![];
        let cap = 100.0 / online;
        if let Some(a) = node_state(o, &n.id, "draining") {
            opts.push(Opt::new("drain", "Drain it: finish current chunks, take no new ones").act(a).auto().eff(format!("{} lost-PC events a day stop; recompute share {} falls; fleet capacity -{cap:.1} %", n.losses_24h, pc(rs)))
                .metric("flaky_nodes", flaky.len() as f64, flaky.len() as f64 - 1.0).cost(&format!("{cap:.1} % less capacity until it is fixed")).tte("immediately")
                .conf(format!("measured: {} node.lost in 24 h (limit {thr}); each loss recomputes its in-flight chunks", n.losses_24h)).check("losses_24h for the fleet drops by this PC's count").score(60.0));
        }
        if let Some(a) = node_state(o, &n.id, "cordoned") {
            opts.push(Opt::new("cordon", "Cordon it: no new chunks, keep it connected").act(a).eff("same as drain for new work; the PC stays visible for diagnosis".into())
                .metric("flaky_nodes", flaky.len() as f64, flaky.len() as f64 - 1.0).cost(&format!("{cap:.1} % less capacity")).tte("immediately").conf("rule of thumb".into()).check("no node.lost events for this PC after the change").score(45.0));
        }
        if flaky.len() >= 3 && o.t("lost_after_s") <= 5.0 {
            let (c, to) = (o.t("lost_after_s"), bounded("lost_after_s", o.t("lost_after_s") * 2.0));
            opts.push(Opt::new("lost_after", &format!("Tolerate short silences: lost_after_s {c} -> {to} s")).act(Action::Tune { key: "lost_after_s".into(), from: c, to }).auto()
                .eff(format!("short network gaps stop counting as losses ({} PCs are flaky)", flaky.len())).metric("losses_24h", o.nodes.iter().map(|n| n.losses_24h as f64).sum(), 0.0)
                .cost(&format!("a really dead PC is noticed {to} s late, its chunks wait that long")).tte("immediately").conf("model: gaps shorter than the new limit disappear; rule of thumb".into()).check("losses_24h stops growing").score(30.0));
        }
        opts.push(Opt::new("investigate", "Look at the PC").steps(&["Check cable, Wi-Fi, power saving and sleep settings on the PC", "Read its log: `ck logs --kind node.disconnect`"]).eff("fixes the cause for good".into()).cost("a person's time").tte("hours").conf("rule of thumb".into()).check("no node.lost events in 24 h").score(20.0));
        out.push(sug(format!("flaky_pc:{}", n.id), "flaky_pc", "warn", format!("{} keeps dropping out", n.host), format!("{} node.lost events in 24 h (limit {thr}); recompute is {} of PC time", n.losses_24h, pc(rs)), 0.0, opts,
            "every loss makes its in-flight chunks run again somewhere else".into()));
    }

    // slow PCs
    for a in o.alerts.iter().filter(|a| a.0 == "slow_pc") {
        let Some(id) = a.1.as_deref() else { continue };
        let ts = sh.as_ref().map_or(0.0, |s| s.tail);
        let mut opts = vec![];
        if let Some(act) = node_state(o, id, "cordoned") {
            opts.push(Opt::new("cordon", "Cordon it: no new chunks").act(act).auto().eff(format!("tail idle {} -> ~{} of PC time; jobs end sooner", pc(ts), pc(ts * 0.6))).metric("tail_share", ts, ts * 0.6)
                .cost(&format!("{:.1} % of capacity (but it is slow, so less of the work)", 100.0 / online)).tte("next job end").conf(format!("measured: {}; model: the slowest PC sets the tail (rule of thumb 40 % shorter)", a.2)).check("tail_share falls").score(40.0 + ts * 100.0));
        }
        opts.push(Opt::new("investigate", "Find out why it is slow").steps(&["Check for a person using the PC (Presence) or a hot, throttled CPU", "Re-run calibration by restarting the agent"]).eff("restores its share of the work".into()).cost("a person's time").tte("hours").conf("rule of thumb".into()).check("the slow_pc alert closes").score(20.0));
        out.push(sug(format!("slow_pc:{id}"), "slow_pc", "warn", format!("{} is much slower than the others", o.node(id).map_or(id, |n| n.host.as_str())), a.2.clone(), 0.0, opts,
            format!("the last chunks run twice (speculation) so the tail stays about {} of PC time", pc(ts))));
    }

    // standbys
    let want_new = o.target_standbys == 0 && o.standbys == 0 && o.online() >= 3;
    if o.alert("no_standby") || want_new {
        let best = o.best_head.as_ref().map(|b| format!("best PC now: {} (score {:.2})", b.1, b.2)).unwrap_or_else(|| "no eligible PC right now".into());
        let mut opts = vec![];
        if o.target_standbys == 0 {
            opts.push(Opt::new("add_standby", "Keep one standby head").act(Action::HaTarget { from: 0, to: 1 }).eff("a head crash is taken over in about 1 s instead of stopping all work".into())
                .cost("one PC runs a copy of the database (small CPU and disk)").tte("about 1 minute (it recruits the best PC)").conf(format!("rule of thumb; {best}")).check("standbys count reaches 1 (GET /ha)").score(80.0));
        } else if o.standbys < o.target_standbys {
            opts.push(Opt::new("recruit", "Make an eligible PC available").steps(&["Open Standby heads and check the selector matches at least one idle, non-virtual PC", &format!("{best}")]).eff("the missing standby is recruited automatically".into())
                .cost("a person's time").tte("about 1 minute after").conf(format!("measured: {} of {} standbys; {best}", o.standbys, o.target_standbys)).check("no_standby alert closes").score(60.0));
            opts.push(Opt::new("lower_target", "Lower the target to what exists").act(Action::HaTarget { from: o.target_standbys as u32, to: o.standbys as u32 }).eff("the alert stops; no extra safety".into())
                .cost("less protection against a head crash").risk("med", true).tte("immediately").conf("rule of thumb".into()).check("no_standby alert closes").score(15.0));
        }
        out.push(sug("no_standby".into(), "no_standby", if o.standbys == 0 { "warn" } else { "tip" }, "No standby head to take over".into(), format!("{} standby head(s), target {}; {best}", o.standbys, o.target_standbys), if want_new { 60.0 } else { 0.0 }, opts,
            "if the head dies, work stops until someone restarts it".into()));
    }
    if o.alert("standby_behind") {
        let (c, to) = (o.t("ring_max"), bounded("ring_max", o.t("ring_max") * 2.0));
        let opts = vec![
            Opt::new("ring", &format!("Keep more changes in memory: ring_max {c} -> {to}")).act(Action::Tune { key: "ring_max".into(), from: c, to }).auto()
                .eff("a lagging standby catches up from the log instead of a full copy".into()).cost("some head memory (about 200 bytes a change)").tte("immediately").conf("rule of thumb".into()).check("the standby_behind alert closes").score(40.0),
            Opt::new("link", "Check the standby's link and disk").steps(&["Run a network probe to the standby PC (Network page)", "Check free disk on the standby"]).eff("removes the cause".into()).cost("a person's time").tte("minutes").conf("rule of thumb".into()).check("lag falls below alert_standby_lag").score(30.0),
        ];
        out.push(sug("standby_behind".into(), "standby_behind", "warn", "A standby is falling behind".into(), o.alerts.iter().find(|a| a.0 == "standby_behind").map(|a| a.2.clone()).unwrap_or_default(), 0.0, opts,
            "after a head crash the standby is behind; a full copy delays the takeover".into()));
    }
    if let Some(a) = o.alerts.iter().find(|a| a.0 == "reserve_at_risk") {
        let pending = o.nodes.iter().filter(|n| n.state == "pending").count();
        let mut opts = vec![Opt::new("add_pcs", "Bring PCs that match the reserve online").steps(&["Wake or start PCs that match the reserve selector", "Check Reserves: the selector may be too narrow"]).eff("the reserve is whole again".into()).cost("a person's time").tte("minutes").conf("measured: reserve holds fewer PCs than keep_n".into()).check("reserve_at_risk alert closes").score(50.0)];
        if pending > 0 {
            opts.push(Opt::new("approve", &format!("Approve the {pending} pending PC(s)")).steps(&["Nodes: set state active for PCs you know"]).eff(format!("up to {pending} more PCs can fill the reserve")).cost("trust: only approve PCs you recognise").risk("med", true).tte("immediately").conf("measured: pending PCs exist".into()).check("reserve_at_risk alert closes").score(45.0));
        }
        opts.push(Opt::new("lower_keep", "Lower keep_n of the reserve").steps(&["Reserves: edit the reserve and reduce keep_n"]).eff("the alert stops".into()).cost("less guaranteed capacity").risk("med", true).tte("immediately").conf("rule of thumb".into()).check("alert closes").score(15.0));
        out.push(sug("reserve_at_risk".into(), "reserve_at_risk", "warn", "A reserve holds fewer PCs than promised".into(), a.2.clone(), 0.0, opts, "reserved work may find too few PCs".into()));
    }

    // quotas
    let mut quota: HashMap<&str, usize> = HashMap::new();
    for j in o.jobs.iter().filter(|j| j.running && j.wait.as_deref() == Some("quota")) {
        *quota.entry(j.user.as_str()).or_default() += 1;
    }
    let idle = o.nodes.iter().filter(|n| n.online && n.state == "active" && !n.busy).count();
    for (user, n) in quota.into_iter().filter(|_| idle >= 2) {
        let opts = vec![Opt::new("raise_quota", &format!("Raise {user}'s max_pcs quota")).steps(&[&format!("Users: PUT /api/v1/users/{user}/quota with a higher max_pcs (or 0 for no limit)")]).eff(format!("{idle} idle PCs start working for {n} waiting job(s)"))
            .metric("idle_pcs", idle as f64, 0.0).cost("other users get a smaller fair share while these jobs run").tte("seconds").conf(format!("measured: {idle} PCs idle while {n} job(s) wait on the quota")).check("idle_pcs drops").score(idle as f64 * 5.0)];
        out.push(sug(format!("quota_wait:{user}"), "quota_wait", "tip", format!("{user}'s jobs wait on a quota while {idle} PCs sit idle"), format!("{n} job(s) of {user} wait_reason=quota, {idle} PCs idle"), 60.0, opts,
            "the PCs stay idle (and burn about 25 W each) until the quota frees".into()));
    }

    // disk
    if let Some((used, free)) = o.disk.filter(|(u, f)| *u >= 85.0 || *f < 5.0) {
        let (pk, ek) = (o.t("parts_keep_days"), o.t("events_keep_days"));
        let mut opts = vec![];
        if pk == 0.0 || pk > 30.0 {
            opts.push(Opt::new("parts", "Delete result parts after 30 days: parts_keep_days -> 30").act(Action::Tune { key: "parts_keep_days".into(), from: pk, to: 30.0 }).eff(format!("frees the stored parts of jobs older than 30 days; disk {used:.0} % used now"))
                .metric("disk_used_pct", used, used - 5.0).cost("results of old jobs can no longer be fetched (job rows stay)").risk("med", false).tte("next maintenance run").conf(format!("measured: {used:.0} % used, {free:.1} GB free; size freed unknown")).check("disk_used_pct falls").score(50.0));
        }
        if ek > 7.0 {
            opts.push(Opt::new("events", "Keep events 7 days: events_keep_days -> 7").act(Action::Tune { key: "events_keep_days".into(), from: ek, to: 7.0 }).eff("small saving; shortens the audit trail".into()).cost("audit trail older than 7 days is deleted").risk("med", false).tte("next maintenance run")
                .conf("rule of thumb: events are small".into()).check("disk_used_pct falls a little").score(15.0));
        }
        opts.push(Opt::new("free", "Free space by hand").steps(&["Move CK_DATA/backups and old logs elsewhere", "Add disk space or move CK_DATA to a larger disk"]).eff(format!("{free:.1} GB free now")).cost("a person's time").tte("minutes").conf("measured".into()).check("disk_used_pct falls").score(40.0));
        out.push(sug("disk".into(), "disk", if free < 2.0 || used >= 95.0 { "warn" } else { "tip" }, "The head's disk is filling up".into(), format!("{used:.0} % used, {free:.1} GB free"), 0.0, opts, "when the disk is full results are queued again and jobs stall (job.error)".into()));
    }

    // eco
    let nodes_up = o.nodes.iter().filter(|n| n.online).count();
    if o.power_mode == "off" && nodes_up >= 4 && !o.jobs.iter().any(|j| j.running) {
        let (keep, up) = (o.keep_awake as f64, nodes_up as f64);
        let sleepers = (up - keep).max(0.0);
        let kwh = sleepers * (crate::sim::W_IDLE - crate::sim::W_SLEEP) * 24.0 / 1000.0;
        let opts = vec![Opt::new("eco", "Turn eco mode on").act(Action::Power { from: "off".into(), to: "eco".into() }).auto().eff(format!("about {kwh:.1} kWh a day: {sleepers:.0} idle PCs sleep ({} W -> {} W each)", crate::sim::W_IDLE, crate::sim::W_SLEEP))
            .metric("idle_pcs", up, keep).cost(&format!("a job arriving at night waits ~1-2 min for PCs to wake ({keep:.0} stay awake)")).tte("within idle_min minutes").conf(format!("model: sim.rs power constants x {sleepers:.0} PCs x 24 h")).check("idle_pcs falls to keep_awake").score(kwh.min(100.0))];
        out.push(sug("eco".into(), "eco", "tip", format!("{nodes_up} PCs idle with power saving off"), format!("no job running, {nodes_up} PCs online, power mode off"), 1800.0, opts, format!("{nodes_up} PCs keep drawing ~{} W each while idle", crate::sim::W_IDLE)));
    }

    // blobs sent directly to many PCs
    let direct: Vec<&AdvJob> = o.jobs.iter().filter(|j| j.running && j.blobs > 0 && j.blob_mode != "peer").collect();
    if !direct.is_empty() && o.online() >= 6 {
        let seeds = o.t("blob_seeds");
        let saved = o.blob_sent_gb * (1.0 - seeds / online);
        let opts = vec![Opt::new("peer", "Send files peer to peer").steps(&["Submit the job with blob_mode peer (SDK: blob_mode=\"peer\"); see docs/BULK.md", "Optionally pre-stage the blob before submitting"])
            .eff(format!("the head sends to {seeds:.0} PCs per lab, the rest copy from peers: about {saved:.1} GB less from the head for the {:.1} GB sent so far", o.blob_sent_gb)).cost("first PCs finish downloading a bit later").tte("next job")
            .conf(format!("model: head sends {seeds:.0} of {online:.0} copies; measured: {:.1} GB served", o.blob_sent_gb)).check("head_bytes_sent (GET /net) grows slower per job").score(saved.min(100.0) + 10.0)];
        out.push(sug("blob_peer".into(), "blob_peer", "tip", "Job files go from the head to every PC".into(), format!("{} running job(s) with blobs in direct mode, {online:.0} PCs", direct.len()), 30.0, opts, "the head's uplink limits how fast PCs get their files".into()));
    }

    // failed jobs
    let mut fails: HashMap<&str, (usize, &str, String)> = HashMap::new();
    for j in o.jobs.iter().filter(|j| j.failed) {
        let e = j.error.as_deref().unwrap_or("");
        let lo = e.to_lowercase();
        let (id, title) = FAIL_REASONS.iter().find(|r| lo.contains(r.0)).map_or(("other", "A job failed"), |r| (r.1, r.2));
        let ent = fails.entry(id).or_insert((0, title, e.chars().take(120).collect()));
        ent.0 += 1;
    }
    for (id, (n, title, example)) in fails {
        let mut opts = vec![];
        match id {
            "output_cap" => {
                let (c, to) = (o.t("out_max_mb"), bounded("out_max_mb", o.t("out_max_mb") * 2.0));
                opts.push(Opt::new("out_up", &format!("Allow more output: out_max_mb {c} -> {to}")).act(Action::Tune { key: "out_max_mb".into(), from: c, to }).auto().eff("chunks with larger output succeed".into()).cost("agents keep more output in memory").tte("next chunk").conf("measured: the error names the output cap".into()).check("no new job fails with this error").score(60.0));
                opts.push(Opt::new("trim", "Print less").steps(&["Write results to $CK_OUT files instead of stdout"]).eff("removes the cause".into()).cost("job code change").tte("next run").conf("rule of thumb".into()).check("no failure").score(40.0));
            }
            "attempts" => {
                let (c, to) = (o.t("max_attempts"), bounded("max_attempts", o.t("max_attempts") + 2.0));
                opts.push(Opt::new("retry_up", &format!("Retry more: max_attempts {c} -> {to}")).act(Action::Tune { key: "max_attempts".into(), from: c, to }).auto().eff("jobs that fail on rare, PC-specific errors get through".into()).cost("a program that always fails wastes more PC time before it stops").tte("next failure").conf("rule of thumb".into()).check("fewer failed jobs").score(35.0));
                opts.push(Opt::new("fix_job", "Fix the program").steps(&["Run one chunk by hand with the same input", "Read the error in `ck logs --kind chunk.failed`"]).eff("removes the cause".into()).cost("a person's time").tte("hours").conf("rule of thumb".into()).check("the job passes").score(50.0));
            }
            "timeout" => opts.push(Opt::new("smaller", "Use smaller chunks").steps(&["Submit with a smaller chunk size so one chunk is well under the time limit", "Check the PC named in the task.timeout event for load"]).eff("a chunk no longer outlives its limit".into()).cost("more chunks, more dispatch work").tte("next run").conf("rule of thumb".into()).check("no task.timeout events").score(50.0)),
            "blob" => opts.push(Opt::new("blob", "Re-upload and check the blob").steps(&["PUT the file again so the head holds a good copy", "Look for blob.corrupt / blob.fetch_failed in the log"]).eff("the PCs can fetch and verify it".into()).cost("upload time").tte("minutes").conf("measured: error names a blob".into()).check("the job passes").score(50.0)),
            "no_pcs" => opts.push(Opt::new("selector", "Loosen the selector").steps(&["`ck select 'EXPR'` previews the matches", "Resubmit with a wider selector or bring matching PCs online"]).eff("PCs match and the job runs".into()).cost("job may land on other PCs").tte("next run").conf("measured: no match".into()).check("the job runs").score(50.0)),
            "quota" => opts.push(Opt::new("quota", "Raise the quota").steps(&["PUT /api/v1/users/NAME/quota with a higher max_pcs / max_running"]).eff("the job runs".into()).cost("fair share for others").tte("seconds").conf("measured".into()).check("the job runs").score(50.0)),
            _ => opts.push(Opt::new("read", "Read the failure").steps(&["`ck job show ID` and `ck logs --kind chunk.failed --since 1h`", "Run one chunk by hand"]).eff("shows the cause".into()).cost("a person's time").tte("minutes").conf("rule of thumb".into()).check("the job passes").score(30.0)),
        }
        out.push(sug(format!("job_fail:{id}"), "job_fail", if n >= 3 { "warn" } else { "info" }, title.to_string(), format!("{n} job(s) failed in the last day, e.g. \"{example}\""), 0.0, opts, "the same failure repeats on every resubmit".into()));
    }

    // warnings in the log and head events
    let mut seen: Vec<(String, u32, String)> = o.warns.clone();
    for (k, n) in &o.events {
        if *n > 0 && !seen.iter().any(|s| s.0 == *k) {
            seen.push((k.clone(), *n, String::new()));
        }
    }
    for (kind, n, msg) in seen {
        let Some(f) = LOG_FIXES.iter().find(|f| f.0 == kind) else { continue };
        let mut opts = vec![Opt::new("fix", "Follow the steps").steps(f.2).eff("clears the warning".into()).cost("a person's time").tte("minutes").conf(format!("measured: {n} x {kind} in the last hour")).check(&format!("no new {kind} lines in 1 h")).score(50.0)];
        opts[0].steps.push(format!("Details: {}", f.3));
        out.push(sug(format!("log:{kind}"), "log_event", if n >= 5 { "warn" } else { "info" }, f.1.to_string(), format!("{n} x {kind} in the last hour{}{}", if msg.is_empty() { "" } else { ", last: " }, msg), 0.0, opts, "the warning keeps repeating".into()));
    }

    for s in &mut out {
        s.since_ms = now;
    }
    out
}

// ---------------------------------------------------------------- state, hysteresis, tracking, policy

pub const CLEAR_MS: i64 = 60_000;
pub const FOLLOW_S: i64 = 600;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub observed: Option<f64>,
    pub predicted_gain: f64,
    pub observed_gain: f64,
    pub hit: bool,
    pub ts_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Applied {
    pub id: String,
    pub kind: String,
    pub option: String,
    pub action: Action,
    pub by: String,
    pub auto: bool,
    pub ts_ms: i64,
    pub metric: Option<String>,
    pub before: f64,
    pub predicted: f64,
    pub follow_s: i64,
    pub undone: bool,
    pub outcome: Option<Outcome>,
}

/// Stored in the head database (meta "advisor").
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Persist {
    pub enabled: bool,
    /// kind -> off | suggest | auto
    pub policy: HashMap<String, String>,
    /// suggestion id -> snoozed until (ms)
    pub dismissed: HashMap<String, i64>,
    /// newest first
    pub applied: Vec<Applied>,
}

impl Default for Persist {
    fn default() -> Self {
        Persist { enabled: true, policy: HashMap::new(), dismissed: HashMap::new(), applied: vec![] }
    }
}

impl Persist {
    pub fn policy_of(&self, kind: &str) -> &str {
        self.policy.get(kind).map_or("suggest", String::as_str)
    }
    pub fn snoozed(&self, id: &str, now: i64) -> bool {
        self.dismissed.get(id).is_some_and(|u| *u > now)
    }
    pub fn push(&mut self, a: Applied) {
        self.applied.insert(0, a);
        self.applied.truncate(200);
    }
}

pub const MODES: &[&str] = &["off", "suggest", "auto"];

/// Share of the last 10 judged predictions of `kind` that held; None with fewer than 3.
pub fn accuracy(p: &Persist, kind: &str) -> Option<f64> {
    let v: Vec<bool> = p.applied.iter().filter(|a| a.kind == kind).filter_map(|a| a.outcome.as_ref().map(|o| o.hit)).take(10).collect();
    (v.len() >= 3).then(|| v.iter().filter(|h| **h).count() as f64 / v.len() as f64)
}

/// Judge applied options whose follow-up window ended. Returns the ones just judged.
pub fn track(p: &mut Persist, now: i64, m: &HashMap<String, f64>) -> Vec<Applied> {
    let mut done = vec![];
    for a in p.applied.iter_mut().filter(|a| a.outcome.is_none() && !a.undone && now >= a.ts_ms + a.follow_s * 1000) {
        let obs = a.metric.as_ref().and_then(|k| m.get(k)).copied();
        let (pg, og) = (a.before - a.predicted, obs.map_or(0.0, |x| a.before - x));
        a.outcome = Some(Outcome { observed: obs, predicted_gain: pg, observed_gain: og, hit: a.metric.is_some() && obs.is_some() && pg > 0.0 && og >= 0.5 * pg, ts_ms: now });
        done.push(a.clone());
    }
    done
}

/// What the advisor may do alone for `s`: the recommended option, only under policy auto, only if auto-safe, with a
/// trustworthy rule and not twice in a row.
pub fn auto_pick<'a>(s: &'a Suggestion, p: &Persist, now: i64) -> Option<&'a Opt> {
    if !p.enabled || p.policy_of(&s.kind) != "auto" || accuracy(p, &s.kind).is_some_and(|a| a < 0.5) {
        return None;
    }
    if p.applied.iter().any(|a| a.id == s.id && !a.undone && now - a.ts_ms < FOLLOW_S * 1000) {
        return None;
    }
    s.options.iter().find(|o| o.recommended && o.auto_safe && o.reversible && o.action.as_ref().is_some_and(Action::changes))
}

/// Hysteresis: a suggestion shows once its condition held for `hold_s`, and stays until it was absent for CLEAR_MS.
#[derive(Default)]
pub struct Engine {
    seen: HashMap<String, (i64, i64)>, // id -> (first, last)
}

impl Engine {
    pub fn step(&mut self, now: i64, cands: Vec<Suggestion>, p: &Persist) -> Vec<Suggestion> {
        self.seen.retain(|_, (_, last)| now - *last < CLEAR_MS);
        let mut out = vec![];
        for mut s in cands {
            let e = self.seen.entry(s.id.clone()).or_insert((now, now));
            e.1 = now;
            if now - e.0 < (s.hold_s * 1000.0) as i64 || !p.enabled || p.policy_of(&s.kind) == "off" || p.snoozed(&s.id, now) {
                continue;
            }
            s.since_ms = e.0;
            s.policy = p.policy_of(&s.kind).into();
            s.accuracy = accuracy(p, &s.kind);
            if let Some(a) = s.accuracy {
                for o in &mut s.options {
                    o.confidence = format!("{}; past predictions of this rule held {:.0} % of the time{}", o.confidence, a * 100.0, if a < 0.5 { " (confidence lowered, auto off)" } else { "" });
                }
            }
            out.push(s);
        }
        out.sort_by(|a, b| sev_rank(&b.severity).cmp(&sev_rank(&a.severity)).then(a.id.cmp(&b.id)));
        out
    }
}

fn sev_rank(s: &str) -> u8 {
    match s {
        "warn" => 2,
        "tip" => 1,
        _ => 0,
    }
}

/// What a click would change, without changing it (POST .../preview). `current` reads a tunable.
pub fn preview(a: &Action, current: impl Fn(&str) -> Option<f64>) -> Value {
    use serde_json::json;
    match a {
        Action::Tune { key, from, to } => {
            let valid = crate::tune::validate(key, *to);
            json!({"would_change": [{"setting": key, "from": current(key).unwrap_or(*from), "to": to}], "valid": valid.is_ok(), "error": valid.err()})
        }
        Action::NodeState { node_ids, from, to } => json!({"would_change": node_ids.iter().map(|n| json!({"node": n, "from": from, "to": to})).collect::<Vec<_>>(), "valid": true}),
        Action::HaTarget { from, to } => json!({"would_change": [{"setting": "target_standbys", "from": from, "to": to}], "valid": *to <= 5}),
        Action::Power { from, to } => json!({"would_change": [{"setting": "power.mode", "from": from, "to": to}], "valid": matches!(to.as_str(), "off" | "eco")}),
        Action::Doc { link } => json!({"would_change": [], "valid": true, "doc": link}),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, state: &str, busy: bool, losses: u32) -> AdvNode {
        AdvNode { id: id.into(), host: format!("h-{id}"), state: state.into(), online: true, busy, losses_24h: losses }
    }
    fn bd(dispatch: f64) -> Bd {
        Bd { wall: 100.0, compute: 1000.0 - dispatch, dispatch, chunks: 500, chunk_p50: 0.2, ..Default::default() }
    }
    fn job() -> AdvJob {
        AdvJob { id: 1, running: true, user: "ann".into(), ..Default::default() }
    }
    fn ids(v: &[Suggestion]) -> Vec<&str> {
        v.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn chunks_short_rule_and_numbers() {
        let o = AdvObs { jobs: vec![AdvJob { bd: Some(bd(250.0)), ..job() }], nodes: (0..5).map(|i| node(&i.to_string(), "active", true, 0)).collect(), ..Default::default() };
        let s = advise(&o, 5);
        let d = s.iter().find(|s| s.id == "dispatch_wait").expect("dispatch rule");
        assert!(d.why.contains("25 %"), "{}", d.why);
        let r = d.options.iter().find(|o| o.recommended).unwrap();
        assert_eq!(r.action, Some(Action::Tune { key: "target_chunk_s".into(), from: 1.0, to: 2.0 }));
        assert!(d.options.last().unwrap().id == "nothing");
        assert!(advise(&AdvObs { jobs: vec![AdvJob { bd: Some(bd(50.0)), ..job() }], ..Default::default() }, 5).is_empty(), "5 % wait is fine");
    }

    #[test]
    fn lab_head_rules() {
        let mut o = AdvObs { jobs: vec![AdvJob { bd: Some(bd(250.0)), ..job() }], nodes: (0..5).map(|i| node(&i.to_string(), "active", true, 0)).collect(), ..Default::default() };
        o.hier.labs = vec![("A".into(), 40), ("B".into(), 3)];
        let s = advise(&o, 5);
        let d = s.iter().find(|s| s.id == "dispatch_wait").unwrap();
        let lab = d.options.iter().find(|o| o.id == "lab_heads_auto").expect("offered when a lab has enough PCs");
        assert_eq!(lab.action, Some(Action::Tune { key: "hier_mode".into(), from: 0.0, to: 2.0 }));
        assert!(lab.expected_effect.contains("root busy"), "{}", lab.expected_effect);
        o.hier.mode = 2;
        assert!(!advise(&o, 5).iter().find(|s| s.id == "dispatch_wait").unwrap().options.iter().any(|o| o.id.starts_with("lab_heads")), "already on");
        o.hier.bad = vec![("A".into(), "partitioned".into())];
        o.tun.insert("lease_s".into(), 120.0);
        o.hier.leaves_1h = 2;
        let s = advise(&o, 5);
        assert!(ids(&s).contains(&"lab_head:A") && ids(&s).contains(&"lease_long"), "{:?}", ids(&s));
        assert_eq!(s.iter().find(|s| s.id == "lease_long").unwrap().action, Some(Action::Tune { key: "lease_s".into(), from: 120.0, to: 60.0 }));
        o.tun.insert("lease_s".into(), 10.0);
        o.hier.partitions_1h = 1;
        o.hier.leaves_1h = 0;
        assert!(ids(&advise(&o, 5)).contains(&"lease_short"));
    }

    #[test]
    fn flaky_and_slow_rules_offer_drain_and_cordon() {
        let mut o = AdvObs { target_standbys: 1, standbys: 1, nodes: vec![node("a", "active", true, 5), node("b", "active", true, 0), node("c", "active", true, 0)], ..Default::default() };
        o.alerts.push(("slow_pc".into(), Some("b".into()), "h-b slow".into()));
        let s = advise(&o, 0);
        assert_eq!(ids(&s), ["flaky_pc:a", "slow_pc:b"]);
        let f = &s[0];
        assert!(matches!(f.action, Some(Action::NodeState { ref to, .. }) if to == "draining"));
        assert!(f.options.iter().any(|o| o.id == "cordon") && f.options.iter().any(|o| o.id == "nothing"));
        assert!(f.options.iter().filter(|o| o.recommended).count() == 1);
    }

    #[test]
    fn standby_disk_eco_quota_blob_fail_log_rules() {
        let mut o = AdvObs { nodes: (0..6).map(|i| node(&i.to_string(), "active", false, 0)).collect(), power_mode: "off".into(), keep_awake: 2, disk: Some((92.0, 3.0)), ..Default::default() };
        o.jobs.push(AdvJob { wait: Some("quota".into()), ..job() });
        o.jobs.push(AdvJob { id: 2, failed: true, error: Some("output over the cap".into()), ..Default::default() });
        o.warns.push(("blob.corrupt".into(), 2, "bad".into()));
        let s = advise(&o, 0);
        for want in ["no_standby", "disk", "quota_wait:ann", "job_fail:output_cap", "log:blob.corrupt"] {
            assert!(ids(&s).contains(&want), "{want} in {:?}", ids(&s));
        }
        assert!(!ids(&s).contains(&"eco"), "a job is running, no eco advice");
        o.jobs.clear();
        o.jobs.push(AdvJob { id: 3, running: true, blobs: 1, blob_mode: "direct".into(), ..job() });
        o.jobs.truncate(1);
        o.jobs[0].running = true;
        let s = advise(&o, 0);
        assert!(ids(&s).contains(&"blob_peer"));
        o.jobs.clear();
        assert!(ids(&advise(&o, 0)).contains(&"eco"));
    }

    #[test]
    fn every_alert_kind_has_a_rule() {
        // the kinds research.rs really opens
        let o = crate::research::Obs {
            rates: vec![("a".into(), "a".into(), 1.0), ("b".into(), "b".into(), 100.0), ("c".into(), "c".into(), 100.0), ("d".into(), "d".into(), 100.0)],
            losses: [("a".to_string(), 9u32)].into(), head_cpu: Some(99.0), standby_lag: 10_000_000, standbys: 1, target_standbys: 2, reserves_short: vec!["r".into()],
        };
        let mut kinds: Vec<&str> = crate::research::conditions(&o, &crate::research::Cfg { slow_mad: 3.0, slow_min_s: 1.0, flaky_losses: 3.0, head_cpu_pct: 85.0, standby_lag: 5000.0, no_standby_s: 1.0 }).iter().map(|c| c.kind).collect();
        kinds.sort();
        kinds.dedup();
        let mut known = ALERT_KINDS.to_vec();
        known.sort();
        assert_eq!(kinds, known, "ALERT_KINDS and research::conditions disagree");
        let mut ob = AdvObs { nodes: vec![node("a", "active", true, 0), node("b", "active", true, 0)], target_standbys: 2, standbys: 1, ..Default::default() };
        for k in ALERT_KINDS {
            ob.alerts.push((k.to_string(), Some("a".into()), "detail".into()));
        }
        let s = advise(&ob, 0);
        for k in ALERT_KINDS {
            assert!(s.iter().any(|x| x.kind == *k), "no suggestion for alert kind {k}");
        }
        // every warn log kind in LOG_FIXES yields a suggestion with steps
        for f in LOG_FIXES {
            let o = AdvObs { warns: vec![(f.0.into(), 1, String::new())], ..Default::default() };
            let s = advise(&o, 0);
            assert!(s.len() == 1 && !s[0].options[0].steps.is_empty(), "{}", f.0);
        }
    }

    #[test]
    fn ranking_puts_best_first_and_nothing_last() {
        let mut v = vec![Opt::new("nothing", "n"), Opt::new("a", "a").score(5.0), Opt::new("b", "b").score(9.0)];
        rank(&mut v);
        assert_eq!(v.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["b", "a", "nothing"]);
        assert_eq!(v.iter().map(|o| o.recommended).collect::<Vec<_>>(), [true, false, false]);
    }

    #[test]
    fn hysteresis_hold_clear_dismiss_policy() {
        let mut e = Engine::default();
        let mut p = Persist::default();
        let mk = |hold| Suggestion { hold_s: hold, ..sug("x".into(), "k", "tip", "t".into(), "w".into(), hold, vec![], "n".into()) };
        assert!(e.step(0, vec![mk(30.0)], &p).is_empty(), "not held yet");
        assert_eq!(e.step(31_000, vec![mk(30.0)], &p).len(), 1);
        assert_eq!(e.step(60_000, vec![], &p).len(), 0);
        assert_eq!(e.step(80_000, vec![mk(30.0)], &p).len(), 1, "gap under 60 s keeps the hold");
        assert!(e.step(300_000, vec![], &p).is_empty());
        assert!(e.step(310_000, vec![mk(30.0)], &p).is_empty(), "a long gap restarts the hold");
        p.policy.insert("k".into(), "off".into());
        assert!(e.step(400_000, vec![mk(0.0)], &p).is_empty(), "policy off hides it");
        p.policy.clear();
        p.dismissed.insert("x".into(), 500_000);
        assert!(e.step(410_000, vec![mk(0.0)], &p).is_empty(), "snoozed");
        assert_eq!(e.step(510_000, vec![mk(0.0)], &p).len(), 1, "snooze over");
        p.enabled = false;
        assert!(e.step(520_000, vec![mk(0.0)], &p).is_empty(), "advisor off");
    }

    fn applied(kind: &str, ts: i64, hit: Option<bool>) -> Applied {
        Applied { id: format!("{kind}:1"), kind: kind.into(), option: "o".into(), action: Action::Tune { key: "pipeline".into(), from: 2.0, to: 3.0 }, by: "u".into(), auto: false, ts_ms: ts, metric: Some("dispatch_share".into()), before: 0.3, predicted: 0.15, follow_s: 600,
                  undone: false, outcome: hit.map(|h| Outcome { observed: Some(0.1), predicted_gain: 0.15, observed_gain: 0.2, hit: h, ts_ms: ts }) }
    }

    #[test]
    fn tracker_judges_after_window_and_lowers_trust() {
        let mut p = Persist::default();
        p.push(applied("dispatch_wait", 0, None));
        let m: HashMap<String, f64> = [("dispatch_share".to_string(), 0.10)].into();
        assert!(track(&mut p, 100_000, &m).is_empty(), "window still open");
        let d = track(&mut p, 601_000, &m);
        assert_eq!(d.len(), 1);
        let o = d[0].outcome.as_ref().unwrap();
        assert!(o.hit && (o.observed_gain - 0.2).abs() < 1e-9);
        let bad: HashMap<String, f64> = [("dispatch_share".to_string(), 0.29)].into();
        let mut p2 = Persist::default();
        for i in 0..3 {
            p2.push(applied("k", i, None));
        }
        track(&mut p2, 10_000_000, &bad);
        assert_eq!(accuracy(&p2, "k"), Some(0.0), "three misses");
        assert_eq!(accuracy(&p, "dispatch_wait"), None, "one sample is not enough");
    }

    #[test]
    fn policy_enforcement_for_auto() {
        let mut s = sug("x".into(), "k", "tip", "t".into(), "w".into(), 0.0, vec![Opt::new("t", "t").act(Action::Tune { key: "pipeline".into(), from: 2.0, to: 3.0 }).auto().score(5.0)], "n".into());
        let mut p = Persist::default();
        assert!(auto_pick(&s, &p, 0).is_none(), "default policy is suggest");
        p.policy.insert("k".into(), "auto".into());
        assert_eq!(auto_pick(&s, &p, 0).map(|o| o.id.as_str()), Some("t"));
        p.enabled = false;
        assert!(auto_pick(&s, &p, 0).is_none(), "global off");
        p.enabled = true;
        p.push(applied("k", 0, None));
        p.applied[0].id = "x".into();
        assert!(auto_pick(&s, &p, 1000).is_none(), "not twice in the follow-up window");
        p.applied[0].undone = true;
        assert!(auto_pick(&s, &p, 1000).is_some(), "an undone action may be tried again later");
        for i in 0..3 {
            p.push(applied("k", i, Some(false)));
        }
        assert!(auto_pick(&s, &p, 10_000_000).is_none(), "mispredicting rule loses auto");
        let mut q = Persist::default();
        q.policy.insert("k".into(), "auto".into());
        s.options[0].auto_safe = false;
        assert!(auto_pick(&s, &q, 0).is_none(), "not auto-safe: never alone");
        s.options[0].auto_safe = true;
        s.options[0].action = Some(Action::Doc { link: "x".into() });
        assert!(auto_pick(&s, &q, 0).is_none(), "a doc link is not an action");
    }

    #[test]
    fn undo_inverts_and_preview_validates() {
        let a = Action::Tune { key: "pipeline".into(), from: 2.0, to: 3.0 };
        assert_eq!(a.inverse(), Some(Action::Tune { key: "pipeline".into(), from: 3.0, to: 2.0 }));
        assert_eq!(Action::Doc { link: "x".into() }.inverse(), None);
        assert_eq!(preview(&a, |_| Some(2.0))["valid"], true);
        assert_eq!(preview(&Action::Tune { key: "pipeline".into(), from: 2.0, to: 99.0 }, |_| Some(2.0))["valid"], false);
    }
}
