//! Wave 12 research features, pure logic (docs/API.md "Research features"): per-job time breakdown, alert rules and
//! their open/close state machine, experiment upload parsing, the daily report. The head (head.rs) feeds counters and
//! observations in and writes the results to the store; nothing here touches a lock or a socket.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use crate::store::{AlertRow, ExperimentRow, Store};

// ---------------------------------------------------------------- time breakdown

/// What one connection (worker id) did for one job. Seconds are split at `Acct::tail_at`: [0, tail) = work was still
/// unassigned, [tail, end] = only in-flight work remained.
#[derive(Default, Clone, Debug)]
pub struct PcAcct {
    pub node: String,
    pub host: String,
    pub chunks: u64,
    pub numbers: u64,
    pub good: [f64; 2],  // compute seconds of chunks whose result was kept: [before tail, in tail]
    pub waste: [f64; 2], // compute seconds of chunks that were thrown away (lost PC, losing speculative copy, duplicate)
    pub rate: f64,       // numbers/s of the last kept chunk
}

/// Cheap per-job counters, updated where chunks start (tail marker) and end. No store writes.
#[derive(Default, Debug)]
pub struct Acct {
    pub pcs: HashMap<u64, PcAcct>,
    pub tail_at: Option<f64>, // seconds since job start when the last unassigned range was handed out
    pub secs: Vec<f32>,       // compute time of kept chunks (ring of CHUNK_KEEP)
    pub nsecs: u64,
    pub paused0: f64, // State::paused_s when this job started counting
}

const CHUNK_KEEP: usize = 8192;

/// Split a chunk that ended at `end_t` and took `secs` into (before tail, in tail) seconds.
fn split(secs: f64, end_t: f64, tail_at: Option<f64>) -> [f64; 2] {
    let Some(t) = tail_at else { return [secs, 0.0] };
    let pre = (end_t.min(t) - (end_t - secs).max(0.0)).clamp(0.0, secs);
    [pre, secs - pre]
}

impl Acct {
    pub fn pc(&mut self, wid: u64, who: impl FnOnce() -> (String, String)) -> &mut PcAcct {
        self.pcs.entry(wid).or_insert_with(|| {
            let (node, host) = who();
            PcAcct { node, host, ..Default::default() }
        })
    }
    /// A kept result: `numbers` computed in `secs`, arrived `end_t` seconds after the job started.
    pub fn kept(&mut self, wid: u64, who: impl FnOnce() -> (String, String), numbers: u64, secs: f64, end_t: f64) {
        let sp = split(secs, end_t, self.tail_at);
        let p = self.pc(wid, who);
        p.chunks += 1;
        p.numbers += numbers;
        p.good[0] += sp[0];
        p.good[1] += sp[1];
        p.rate = numbers as f64 / secs.max(1e-9);
        if self.secs.len() < CHUNK_KEEP {
            self.secs.push(secs as f32);
        } else {
            self.secs[(self.nsecs % CHUNK_KEEP as u64) as usize] = secs as f32;
        }
        self.nsecs += 1;
    }
    /// Compute that was thrown away.
    pub fn wasted(&mut self, wid: u64, who: impl FnOnce() -> (String, String), secs: f64, end_t: f64) {
        let sp = split(secs, end_t, self.tail_at);
        let p = self.pc(wid, who);
        p.waste[0] += sp[0];
        p.waste[1] += sp[1];
    }
}

fn pct(v: &[f32], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() - 1) as f64 * q).round() as usize] as f64
}

/// The Breakdown of docs/API.md. Every PC that held the job gets the whole `wall` seconds, split into compute, recompute,
/// dispatch_wait, tail_idle and failover_pause, so the sum is PCs x wall; `other` is the (non-negative) remainder.
pub fn breakdown(job: u64, a: &Acct, wall: f64, paused: f64) -> Value {
    let wall = wall.max(0.0);
    // a PC that reconnected has two worker ids: merge them by node id
    let mut by: BTreeMap<String, PcAcct> = BTreeMap::new();
    for (wid, p) in &a.pcs {
        let key = if p.node.is_empty() { format!("w{wid}") } else { p.node.clone() };
        let m = by.entry(key.clone()).or_insert_with(|| PcAcct { node: key, host: p.host.clone(), ..Default::default() });
        m.chunks += p.chunks;
        m.numbers += p.numbers;
        for i in 0..2 {
            m.good[i] += p.good[i];
            m.waste[i] += p.waste[i];
        }
        m.rate = m.rate.max(p.rate);
    }
    let t = a.tail_at.unwrap_or(wall).clamp(0.0, wall);
    let paused = paused.clamp(0.0, wall);
    let (mut compute, mut recompute, mut dispatch, mut tail, mut failover) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let mut per_pc = vec![];
    for p in by.values() {
        let (bp, bt) = (p.good[0] + p.waste[0], p.good[1] + p.waste[1]);
        let (cp, ct) = (bp.min(t), bt.min(wall - t));
        let (fp, ft) = (if bp > 0.0 { cp / bp } else { 0.0 }, if bt > 0.0 { ct / bt } else { 0.0 });
        compute += p.good[0] * fp + p.good[1] * ft;
        recompute += p.waste[0] * fp + p.waste[1] * ft;
        let (ip, it) = (t - cp, (wall - t) - ct);
        let fo = paused.min(ip + it);
        failover += fo;
        let from_pre = fo.min(ip);
        dispatch += ip - from_pre;
        tail += it - (fo - from_pre);
        per_pc.push(json!({"node_id": p.node, "host": p.host, "chunks": p.chunks, "numbers": p.numbers, "rate": p.rate, "busy_s": cp + ct}));
    }
    let total = by.len() as f64 * wall;
    let other = (total - (compute + recompute + dispatch + tail + failover)).max(0.0);
    // stragglers: kept-chunk rate under 60 % of the median PC
    let mut rates: Vec<f64> = by.values().filter(|p| p.rate > 0.0).map(|p| p.rate).collect();
    rates.sort_by(|x, y| x.total_cmp(y));
    let med = rates.get(rates.len() / 2).copied().unwrap_or(0.0);
    let stragglers: Vec<&str> = by.values().filter(|p| p.rate > 0.0 && p.rate < 0.6 * med).map(|p| p.node.as_str()).collect();
    json!({
        "job": job, "wall_s": wall,
        "pc_seconds": {"compute": compute, "dispatch_wait": dispatch, "tail_idle": tail, "recompute": recompute, "failover_pause": failover, "other": other},
        "per_pc": per_pc,
        "chunk_s": {"p50": pct(&a.secs, 0.5), "p90": pct(&a.secs, 0.9), "max": pct(&a.secs, 1.0)},
        "stragglers": stragglers,
        "efficiency": if total > 0.0 { (compute / total).clamp(0.0, 1.0) } else { 0.0 },
    })
}

// ---------------------------------------------------------------- alerts

/// What the head saw at one tick.
#[derive(Default, Clone)]
pub struct Obs {
    pub rates: Vec<(String, String, f64)>, // (node id, host, numbers/s) of PCs that are working
    pub losses: HashMap<String, u32>,      // node.lost events in the last 24 h
    pub head_cpu: Option<f64>,             // head process CPU, percent of one core
    pub standby_lag: u64,                  // worst standby lag, changes
    pub standbys: usize,
    pub target_standbys: usize,
    pub reserves_short: Vec<String>, // "reserve 3 (lab=A) holds 1 of 2"
}

#[derive(Clone, Copy)]
pub struct Cfg {
    pub slow_mad: f64,
    pub slow_min_s: f64,
    pub flaky_losses: f64,
    pub head_cpu_pct: f64,
    pub standby_lag: f64,
    pub no_standby_s: f64,
}

impl Cfg {
    pub fn now() -> Cfg {
        let g = crate::tune::get;
        Cfg { slow_mad: g("alert_slow_mad"), slow_min_s: g("alert_slow_min_s"), flaky_losses: g("alert_flaky_losses"), head_cpu_pct: g("alert_head_cpu_pct"),
              standby_lag: g("alert_standby_lag"), no_standby_s: g("alert_no_standby_s") }
    }
}

/// A condition that holds now; it becomes an alert once it has held for `hold_s`.
#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub kind: &'static str,
    pub severity: &'static str,
    pub node: Option<String>,
    pub detail: String,
    pub hold_s: f64,
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

/// The alert rules (docs/API.md). Pure.
pub fn conditions(o: &Obs, c: &Cfg) -> Vec<Cond> {
    let mut out = vec![];
    if o.rates.len() >= 3 {
        let mut r: Vec<f64> = o.rates.iter().map(|x| x.2).collect();
        let med = median(&mut r);
        let mut dev: Vec<f64> = o.rates.iter().map(|x| (x.2 - med).abs()).collect();
        // ponytail: identical PCs have MAD ~ 0, so any wobble would flag; floor the spread at 10 % of the median
        let mad = median(&mut dev).max(0.1 * med);
        let limit = med - c.slow_mad * mad;
        for (id, host, rate) in o.rates.iter().filter(|x| x.2 < limit) {
            out.push(Cond { kind: "slow_pc", severity: "warn", node: Some(id.clone()), detail: format!("{host}: {rate:.3e} numbers/s, fleet median {med:.3e}"), hold_s: c.slow_min_s });
        }
    }
    for (id, n) in o.losses.iter().filter(|(_, n)| **n as f64 > c.flaky_losses) {
        out.push(Cond { kind: "flaky_pc", severity: "warn", node: Some(id.clone()), detail: format!("lost {n} times in 24 h"), hold_s: 0.0 });
    }
    if let Some(cpu) = o.head_cpu.filter(|x| *x > c.head_cpu_pct) {
        out.push(Cond { kind: "head_hot", severity: "warn", node: None, detail: format!("head CPU {cpu:.0}% (limit {:.0}%)", c.head_cpu_pct), hold_s: 60.0 });
    }
    if o.standbys > 0 && o.standby_lag as f64 > c.standby_lag {
        out.push(Cond { kind: "standby_behind", severity: "warn", node: None, detail: format!("a standby is {} changes behind (limit {:.0})", o.standby_lag, c.standby_lag), hold_s: 30.0 });
    }
    if !o.reserves_short.is_empty() {
        out.push(Cond { kind: "reserve_at_risk", severity: "warn", node: None, detail: o.reserves_short.join("; "), hold_s: 0.0 });
    }
    if o.target_standbys > 0 && o.standbys < o.target_standbys {
        let sev = if o.standbys == 0 { "bad" } else { "warn" };
        out.push(Cond { kind: "no_standby", severity: sev, node: None, detail: format!("{} standby head(s), target {}", o.standbys, o.target_standbys), hold_s: c.no_standby_s });
    }
    out
}

struct Slot {
    id: Option<u64>,
    since: i64,
    clear: Option<i64>,
    touched: i64,
    detail: String,
}

pub enum Ev {
    Open(AlertRow),
    Update(AlertRow),
    Close(AlertRow),
}

pub const CLEAR_MS: i64 = 60_000;

/// Opens an alert when a condition has held for its hold time, keeps it while the condition holds (one row per kind and
/// node, never a duplicate), closes it after it has been clear for 60 s.
#[derive(Default)]
pub struct Engine {
    slots: HashMap<(&'static str, String), Slot>,
}

impl Engine {
    pub fn step(&mut self, store: &Store, now: i64, conds: &[Cond]) -> Vec<Ev> {
        let mut evs = vec![];
        let mut seen = std::collections::HashSet::new();
        for c in conds {
            let key = (c.kind, c.node.clone().unwrap_or_default());
            seen.insert(key.clone());
            let s = self.slots.entry(key).or_insert(Slot { id: None, since: now, clear: None, touched: now, detail: String::new() });
            s.clear = None;
            match s.id {
                None if now - s.since >= (c.hold_s * 1000.0) as i64 => {
                    if let Ok(row) = store.alert_open(c.kind, c.severity, c.node.as_deref(), &c.detail, s.since.max(now - (c.hold_s * 1000.0) as i64)) {
                        (s.id, s.touched, s.detail) = (Some(row.id), now, c.detail.clone());
                        evs.push(Ev::Open(row));
                    }
                }
                Some(id) if c.detail != s.detail || now - s.touched >= 30_000 => {
                    (s.touched, s.detail) = (now, c.detail.clone());
                    evs.extend(store.alert_touch(id, &c.detail, now).map(Ev::Update));
                }
                _ => {}
            }
        }
        self.slots.retain(|k, s| {
            if seen.contains(k) {
                return true;
            }
            let Some(id) = s.id else { return false }; // cleared before it ever opened
            let since = *s.clear.get_or_insert(now);
            if now - since < CLEAR_MS {
                return true;
            }
            evs.extend(store.alert_close(id, now).map(Ev::Close));
            false
        });
        evs
    }
}

// ---------------------------------------------------------------- experiments

pub const EXPERIMENT_MAX: usize = 16 * 1024 * 1024;

fn csv_fields(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut q) = (vec![], String::new(), false);
    let mut it = line.chars().peekable();
    while let Some(ch) = it.next() {
        match (ch, q) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => q = !q,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

/// Check the files scripts/experiment.py writes and derive the ExperimentRow (id 0 until stored).
pub fn parse_experiment(name: &str, submitter: &str, raw_csv: &str, runs_jsonl: &str, now_ms: i64) -> Result<ExperimentRow, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err("name must be 1 to 100 characters".into());
    }
    if raw_csv.len() + runs_jsonl.len() > EXPERIMENT_MAX {
        return Err("experiment files are over 16 MB".into());
    }
    let mut lines = raw_csv.lines().filter(|l| !l.trim().is_empty());
    let head = csv_fields(lines.next().ok_or("raw_csv is empty")?);
    let col = |n: &str| head.iter().position(|h| h.trim() == n);
    let (Some(ci), Some(ri)) = (col("config"), col("repeat")) else { return Err("raw_csv needs the columns config and repeat".into()) };
    let (pi, vi) = (col("pcs"), col("virtual"));
    let (mut configs, mut repeats, mut pcs_max, mut virt, mut rows) = (std::collections::BTreeSet::new(), 0u64, 0u64, false, 0);
    for l in lines {
        let f = csv_fields(l);
        if f.len() != head.len() {
            return Err(format!("raw_csv row {} has {} columns, the header has {}", rows + 2, f.len(), head.len()));
        }
        rows += 1;
        configs.insert(f[ci].clone());
        repeats = repeats.max(f[ri].trim().parse().map_err(|_| format!("raw_csv row {}: repeat is not a number", rows + 1))?);
        if let Some(p) = pi.and_then(|i| f[i].trim().parse::<u64>().ok()) {
            pcs_max = pcs_max.max(p);
        }
        virt |= vi.is_some_and(|i| matches!(f[i].trim(), "1" | "true" | "True"));
    }
    if rows == 0 {
        return Err("raw_csv has no data rows".into());
    }
    let mut commit = String::new();
    for (i, l) in runs_jsonl.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let v: Value = serde_json::from_str(l).map_err(|e| format!("runs_jsonl line {}: {e}", i + 1))?;
        if !v.is_object() {
            return Err(format!("runs_jsonl line {} is not an object", i + 1));
        }
        if commit.is_empty() {
            commit = v["commit"].as_str().unwrap_or("").to_string();
        }
        virt |= v["virtual"].as_bool() == Some(true);
        if pcs_max == 0 {
            pcs_max = pcs_max.max(v["pcs"].as_array().map_or(0, |a| a.len() as u64));
        }
    }
    Ok(ExperimentRow { id: 0, name: name.into(), submitter: submitter.into(), created_ms: now_ms, configs: configs.len() as u64, repeats, r#virtual: virt, commit, pcs_max })
}

// ---------------------------------------------------------------- daily report

pub fn tz_min() -> i64 {
    std::env::var("CK_TZ_OFFSET_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(330)
}

/// "YYYY-MM-DD" (local) -> [start, end) in Unix ms.
pub fn day_range(day: &str, tz: i64) -> Option<(i64, i64)> {
    let mut p = day.split('-');
    let (y, m, d): (i64, i64, i64) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    if p.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1970..=9999).contains(&y) {
        return None;
    }
    let a = (crate::jobcli::days_from_civil(y, m, d) * 1440 - tz) * 60_000;
    Some((a, a + 86_400_000))
}

pub fn day_of(ms: i64, tz: i64) -> String {
    let (y, m, d) = crate::jobcli::civil_from_days((ms.div_euclid(60_000) + tz).div_euclid(1440));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The DayReport of docs/API.md from the store.
pub fn day_report(store: &Store, day: &str, tz: i64) -> Result<Value, String> {
    let (a, b) = day_range(day, tz).ok_or("day is YYYY-MM-DD")?;
    let samples = store.samples_between(a, b);
    let n = samples.len() as f64;
    let working: Vec<f64> = samples.iter().filter(|s| s.busy > 0).map(|s| s.rate).collect();
    let peak = working.iter().cloned().fold(0.0, f64::max);
    let mean = if working.is_empty() { 0.0 } else { working.iter().sum::<f64>() / working.len() as f64 };
    let (mut numbers, mut items, mut done, mut failed) = (0u64, 0u64, 0u64, 0u64);
    let mut top = vec![];
    for (id, name, kind, from, end, state) in store.jobs_finished_between(a, b) {
        match state.as_str() {
            "done" => done += 1,
            "failed" => failed += 1,
            _ => {}
        }
        if state == "done" {
            if kind == crate::proto::KIND_LIST {
                items += end.saturating_sub(from);
            } else {
                numbers += end.saturating_sub(from);
            }
        }
        if let Some(bd) = store.job_stats(id).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            let h = bd["per_pc"].as_array().map_or(0, |p| p.len()) as f64 * bd["wall_s"].as_f64().unwrap_or(0.0) / 3600.0;
            top.push((h, id, name));
        }
    }
    top.sort_by(|x, y| y.0.total_cmp(&x.0));
    let top_jobs: Vec<Value> = top.into_iter().take(5).map(|(h, id, name)| json!({"id": id, "name": name, "pc_hours": h})).collect();
    let seen = store.nodes().iter().filter(|nd| nd.first_seen_ms < b && nd.last_seen_ms >= a).count();
    let (mut lost, mut recovered, mut failovers) = (0u64, 0u64, 0u64);
    for (kind, detail) in store.events_between(a, b) {
        match kind.as_str() {
            "node.lost" => lost += 1,
            "chunk.recovered" => recovered += detail.split_whitespace().next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0),
            "ha.promoted" => failovers += 1,
            _ => {}
        }
    }
    let joules: f64 = samples.windows(2).map(|w| (w[1].energy_j - w[0].energy_j).max(0.0)).sum();
    let (sb, so) = samples.iter().fold((0u64, 0u64), |t, s| (t.0 + s.busy as u64, t.1 + s.online as u64));
    Ok(json!({
        "day": day, "throughput_peak": peak, "throughput_mean": mean,
        "work_done": {"numbers": numbers, "items": items, "jobs_done": done, "jobs_failed": failed},
        "pcs": {"seen": seen, "max_online": samples.iter().map(|s| s.online).max().unwrap_or(0), "mean_online": if n > 0.0 { samples.iter().map(|s| s.online as f64).sum::<f64>() / n } else { 0.0 }},
        "failures": {"pcs_lost": lost, "recovered_chunks": recovered, "head_failovers": failovers},
        "energy_kwh": if joules > 0.0 { json!(joules / 3.6e6) } else { Value::Null },
        "busy_share": if so > 0 { sb as f64 / so as f64 } else { 0.0 },
        "top_jobs": top_jobs, "alerts": store.alerts_opened_between(a, b),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Sample;

    fn who(n: &str) -> impl FnOnce() -> (String, String) + '_ {
        move || (n.to_string(), format!("host-{n}"))
    }

    #[test]
    fn breakdown_sums_to_pcs_times_wall_and_is_never_negative() {
        let mut a = Acct::default();
        // job runs 100 s on 3 PCs. work is all handed out at t=80 (tail starts), a loses its chunks, b is late
        a.tail_at = Some(80.0);
        for k in 0..8 {
            a.kept(1, who("a"), 1000, 9.0, 10.0 * (k + 1) as f64); // a: 72 s of work, last chunk ends at 80
        }
        a.wasted(1, who("a"), 6.0, 95.0); // a also computed a chunk that was dropped
        a.kept(2, who("b"), 500, 40.0, 70.0); // b: late, slow
        a.kept(2, who("b"), 500, 15.0, 100.0);
        a.kept(3, who("c"), 3000, 60.0, 60.0); // c: one long chunk at the start
        let v = breakdown(7, &a, 100.0, 10.0);
        let p = &v["pc_seconds"];
        let parts: Vec<f64> = ["compute", "dispatch_wait", "tail_idle", "recompute", "failover_pause", "other"].iter().map(|k| p[k].as_f64().unwrap()).collect();
        assert!(parts.iter().all(|x| *x >= 0.0), "{parts:?}");
        assert!((parts.iter().sum::<f64>() - 300.0).abs() < 1e-6, "sum {parts:?}");
        assert!(parts[3] > 0.0 && parts[4] > 0.0 && parts[0] > 0.0);
        assert_eq!(v["per_pc"].as_array().unwrap().len(), 3);
        assert!(v["efficiency"].as_f64().unwrap() > 0.0 && v["efficiency"].as_f64().unwrap() <= 1.0);
        assert_eq!(v["chunk_s"]["max"].as_f64(), Some(60.0));
        // more busy time than wall (clock skew) is clipped, not negative
        let mut b = Acct::default();
        b.kept(1, who("a"), 1, 500.0, 10.0);
        let v = breakdown(1, &b, 10.0, 0.0);
        assert!(v["pc_seconds"].as_object().unwrap().values().all(|x| x.as_f64().unwrap() >= 0.0));
        assert!((v["pc_seconds"]["compute"].as_f64().unwrap() - 10.0).abs() < 1e-9);
        // no PCs
        assert_eq!(breakdown(1, &Acct::default(), 5.0, 0.0)["pc_seconds"]["compute"], 0.0);
    }

    #[test]
    fn split_at_tail() {
        assert_eq!(split(10.0, 30.0, Some(25.0)), [5.0, 5.0]);
        assert_eq!(split(10.0, 20.0, Some(25.0)), [10.0, 0.0]);
        assert_eq!(split(10.0, 40.0, Some(25.0)), [0.0, 10.0]);
        assert_eq!(split(10.0, 40.0, None), [10.0, 0.0]);
    }

    fn cfg() -> Cfg {
        Cfg { slow_mad: 3.0, slow_min_s: 60.0, flaky_losses: 3.0, head_cpu_pct: 85.0, standby_lag: 5000.0, no_standby_s: 60.0 }
    }

    fn kinds(s: &Store, active: bool) -> Vec<String> {
        let mut v: Vec<String> = s.alerts(active, 100).iter().map(|a| format!("{}:{}", a.kind, a.node_id.clone().unwrap_or_default())).collect();
        v.sort();
        v
    }

    #[test]
    fn each_rule_opens_once_and_closes_after_60s_clear() {
        let s = Store::memory();
        let mut e = Engine::default();
        let mut obs = Obs { target_standbys: 2, standbys: 1, head_cpu: Some(99.0), standby_lag: 9000, ..Default::default() };
        obs.rates = (0..8).map(|i| (format!("n{i}"), format!("h{i}"), if i == 7 { 10.0 } else { 100.0 + i as f64 })).collect();
        obs.losses.insert("n3".into(), 5);
        obs.reserves_short = vec!["reserve 1 () holds 1 of 2".into()];
        let t0 = 1_000_000;
        let mut opened = vec![];
        for sec in (0..=130).step_by(5) {
            for ev in e.step(&s, t0 + sec * 1000, &conditions(&obs, &cfg())) {
                if let Ev::Open(r) = ev {
                    opened.push((sec, r.kind));
                }
            }
        }
        let at = |k: &str| opened.iter().find(|o| o.1 == k).map(|o| o.0);
        assert_eq!((at("flaky_pc"), at("reserve_at_risk")), (Some(0), Some(0)));
        assert_eq!(at("standby_behind"), Some(30));
        assert_eq!((at("slow_pc"), at("head_hot"), at("no_standby")), (Some(60), Some(60), Some(60)));
        assert_eq!(opened.len(), 6, "each opens exactly once: {opened:?}");
        assert_eq!(s.alerts(true, 100).len(), 6);
        assert_eq!(kinds(&s, true).iter().filter(|k| k.starts_with("slow_pc:n7")).count(), 1);
        // everything clears: nothing closes for 60 s, then all close once
        let clear = Obs { target_standbys: 2, standbys: 2, ..Default::default() };
        let t1 = t0 + 135_000;
        let mut closed = 0;
        for sec in (0..=100).step_by(5) {
            for ev in e.step(&s, t1 + sec * 1000, &conditions(&clear, &cfg())) {
                if let Ev::Close(r) = ev {
                    assert!(sec >= 60, "closed after only {sec}s clear");
                    assert!(!r.active);
                    closed += 1;
                }
            }
            if sec < 60 {
                assert_eq!(s.alerts(true, 100).len(), 6);
            }
        }
        assert_eq!((closed, s.alerts(true, 100).len(), s.alerts(false, 100).len()), (6, 0, 6));
        // it comes back: a new row, still one
        for sec in [0, 70] {
            e.step(&s, t1 + 200_000 + sec * 1000, &conditions(&obs, &cfg()));
        }
        assert_eq!(s.alerts(true, 100).len(), 6);
        assert_eq!(s.alerts(false, 100).len(), 12);
    }

    #[test]
    fn flapping_condition_below_hold_never_opens() {
        let s = Store::memory();
        let mut e = Engine::default();
        let hot = Obs { head_cpu: Some(99.0), ..Default::default() };
        for i in 0..20 {
            let o = if i % 2 == 0 { hot.clone() } else { Obs::default() };
            e.step(&s, i * 10_000, &conditions(&o, &cfg()));
        }
        assert!(s.alerts(false, 10).is_empty());
        assert!(conditions(&Obs { rates: vec![("a".into(), "a".into(), 1.0), ("b".into(), "b".into(), 100.0)], ..Default::default() }, &cfg()).is_empty(), "under 3 PCs there is no fleet to compare with");
    }

    #[test]
    fn experiment_upload_parse() {
        let csv = "config,repeat,n,seconds,hashes_per_s,hits,pcs,policy,chunk,virtual\n1 PCs,1,100,1.0,100,0,1,adaptive,0,1\n\"4 PCs\",1,400,1.0,400,0,4,adaptive,0,1\n1 PCs,2,100,1.0,100,0,1,adaptive,0,1\n4 PCs,2,400,1.0,400,0,4,adaptive,0,1\n";
        let runs = "{\"config\":\"1 PCs\",\"commit\":\"abc1234\",\"virtual\":true,\"pcs\":[{}]}\n\n{\"config\":\"4 PCs\",\"commit\":\"abc1234\",\"virtual\":true}\n";
        let r = parse_experiment("scaling", "ann", csv, runs, 5).unwrap();
        assert_eq!((r.configs, r.repeats, r.pcs_max, r.r#virtual, r.commit.as_str(), r.created_ms), (2, 2, 4, true, "abc1234", 5));
        let real = csv.replace(",1\n", ",0\n").replace("\"virtual\":true", "\"virtual\":false");
        assert!(!parse_experiment("x", "a", &real, &runs.replace("true", "false"), 1).unwrap().r#virtual);
        for (n, c, j) in [("", csv, runs), ("x", "", runs), ("x", "a,b\n1,2\n", runs), ("x", "config,repeat\n", runs), ("x", "config,repeat\na,1,2\n", runs), ("x", "config,repeat\na,z\n", runs), ("x", csv, "not json"), ("x", csv, "[1]")] {
            assert!(parse_experiment(n, "a", c, j, 1).is_err(), "{n:?} {c:?} {j:?}");
        }
        let big = "x".repeat(EXPERIMENT_MAX);
        assert!(parse_experiment("x", "a", &big, "", 1).is_err());
    }

    #[test]
    fn day_report_on_fixture() {
        let s = Store::memory();
        let tz = 0;
        let (a, _) = day_range("2026-11-05", tz).unwrap();
        assert_eq!(day_of(a, tz), "2026-11-05");
        assert_eq!(day_of(a - 1, tz), "2026-11-04");
        assert_eq!(day_of(a, 330), "2026-11-05");
        assert!(day_range("2026-13-01", 0).is_none() && day_range("junk", 0).is_none());
        let h = 3_600_000;
        for (i, (online, busy, rate, e)) in [(4, 0, 0.0, 0.0), (5, 5, 100.0, 3.6e6), (5, 3, 300.0, 7.2e6), (2, 2, 200.0, 0.0)].iter().enumerate() {
            s.add_sample(&Sample { ts_ms: a + 10 * h + i as i64 * 60_000, online: *online, busy: *busy, rate: *rate, energy_j: *e }).unwrap();
        }
        let spec = |n: &str, kind: u8, end: u64| crate::jobs::JobSpec { name: n.into(), kind, end, chunk: 10, ..Default::default() };
        let j1 = s.create_job(&spec("big", crate::proto::KIND_SYNTH, 1000), a + h).unwrap();
        let j2 = s.create_job(&spec("list", crate::proto::KIND_LIST, 50), a + h).unwrap();
        let j3 = s.create_job(&spec("bad", crate::proto::KIND_SYNTH, 10), a + h).unwrap();
        let j4 = s.create_job(&spec("yesterday", crate::proto::KIND_SYNTH, 77), a - 5 * h).unwrap();
        s.set_state(j1.id, crate::jobs::JobState::Done, a + 12 * h, None).unwrap();
        s.set_state(j2.id, crate::jobs::JobState::Done, a + 13 * h, None).unwrap();
        s.set_state(j3.id, crate::jobs::JobState::Failed, a + 14 * h, Some("x")).unwrap();
        s.set_state(j4.id, crate::jobs::JobState::Done, a - 4 * h, None).unwrap();
        let mut acct = Acct::default();
        acct.kept(1, who("a"), 10, 1.0, 1.0);
        acct.kept(2, who("b"), 10, 1.0, 1.0);
        s.put_job_stats(j1.id, &breakdown(j1.id, &acct, 3600.0, 0.0).to_string()); // 2 PCs x 1 h
        s.log_event(a + h, "node.lost", "head", "x (node n1)").unwrap();
        s.log_event(a + h, "node.lost", "head", "y (node n2)").unwrap();
        s.log_event(a + h, "chunk.recovered", "head", "3 chunks of node n1 requeued").unwrap();
        s.log_event(a + h, "chunk.recovered", "head", "2 chunks of node n2 requeued").unwrap();
        s.log_event(a + 2 * h, "ha.promoted", "head", "x").unwrap();
        s.log_event(a - h, "node.lost", "head", "old (node n9)").unwrap();
        s.upsert_node("n1", "h1", "1.1.1.1", "{}", a + h).unwrap();
        s.upsert_node("n2", "h2", "1.1.1.2", "{}", a - 10 * h).unwrap();
        s.alert_open("no_standby", "warn", None, "d", a + 3 * h).unwrap();
        let r = day_report(&s, "2026-11-05", tz).unwrap();
        assert_eq!(r["work_done"], json!({"numbers": 1000, "items": 50, "jobs_done": 2, "jobs_failed": 1}));
        assert_eq!(r["failures"], json!({"pcs_lost": 2, "recovered_chunks": 5, "head_failovers": 1}));
        assert_eq!((r["throughput_peak"].as_f64(), r["throughput_mean"].as_f64()), (Some(300.0), Some(200.0)));
        assert_eq!((r["pcs"]["max_online"].as_u64(), r["pcs"]["mean_online"].as_f64(), r["pcs"]["seen"].as_u64()), (Some(5), Some(4.0), Some(1)));
        assert!((r["energy_kwh"].as_f64().unwrap() - 2.0).abs() < 1e-9, "{}", r["energy_kwh"]);
        assert!((r["busy_share"].as_f64().unwrap() - 10.0 / 16.0).abs() < 1e-9);
        assert_eq!((r["top_jobs"][0]["id"].as_u64(), r["top_jobs"][0]["pc_hours"].as_f64()), (Some(j1.id), Some(2.0)));
        assert_eq!(r["alerts"], 1);
        let empty = day_report(&s, "2020-01-01", tz).unwrap();
        assert_eq!((empty["energy_kwh"].clone(), empty["work_done"]["numbers"].clone()), (Value::Null, json!(0)));
        assert!(day_report(&s, "nope", tz).is_err());
    }
}
