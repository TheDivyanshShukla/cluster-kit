//! Pure scheduler core (D5, SCH-01, SCH-02, ORG-05, ORG-06). No I/O, no clock, no locks: the head builds the views and
//! asks; the simulator (M5) will drive the same functions. Owner: a2. Contract: keep the pub items.
//!
//! Rules (M2):
//! - Whole-node allocation: a node works for one job at a time. It may switch job only when it has nothing in flight.
//! - Eligible jobs for a node: running, has work left, the node matches the job's selector, the node takes tasks.
//! - Order: lower `priority` value first (0 urgent .. 4 background); then the job with the fewest nodes assigned now
//!   (spreads nodes over equal-priority jobs); then the older job (lower id).
//! - A node already on a job keeps it while that job is eligible, unless a job of strictly higher priority (lower value)
//!   is eligible for it and has fewer than its fair share — ponytail: no preemption mid-chunk, switching happens between chunks.
//! - Reserves: for each reserve, the nodes matching its selector, sorted by node_id, keep the LAST
//!   max(keep_n, ceil(keep_pct% x matching)) of them free (stable choice, so the same PCs stay free for students).
//!   A reserved node gets no job.
//! - Caps: the effective thread cap of a node is the minimum `max_threads` of every cap whose selector matches it.

use crate::select::{Attrs, Selector};

#[derive(Clone, Debug)]
pub struct NodeView {
    pub wid: u64,
    pub node_id: String,
    pub attrs: Attrs,
    /// state allows tasks (jobs::takes_tasks) and the PC is admitted
    pub takes_tasks: bool,
    /// job it currently works for (has chunks in flight), if any
    pub job: Option<u64>,
    pub inflight: usize,
}

#[derive(Clone, Debug)]
pub struct JobView {
    pub id: u64,
    pub priority: u8,
    pub selector: Selector,
    /// has ranges left to hand out
    pub has_work: bool,
    /// submitter (SCH-04: PCs at equal priority split by user first, then by job)
    pub user: String,
}

#[derive(Clone, Debug)]
pub struct ReserveView {
    pub selector: Selector,
    pub keep_n: u32,
    pub keep_pct: u32,
}

#[derive(Clone, Debug)]
pub struct CapView {
    pub selector: Selector,
    pub max_threads: Option<u32>,
}

/// node_ids kept free by the reserves (union over all reserves).
pub fn reserved(nodes: &[NodeView], reserves: &[ReserveView]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for r in reserves {
        let mut ids: Vec<&str> = nodes.iter().filter(|n| r.selector.matches(&n.attrs)).map(|n| n.node_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        let m = ids.len();
        let k = (r.keep_n as usize).max((r.keep_pct as usize * m).div_ceil(100)).min(m);
        out.extend(ids[m - k..].iter().map(|s| s.to_string()));
    }
    out
}

/// The job the node `wid` should take its next chunk from, or None (idle). `nodes` is every connected node.
pub fn pick_job(wid: u64, nodes: &[NodeView], jobs: &[JobView], reserves: &[ReserveView]) -> Option<u64> {
    let n = nodes.iter().find(|n| n.wid == wid)?;
    if !n.takes_tasks || reserved(nodes, reserves).contains(&n.node_id) {
        return None;
    }
    let mut counts: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for o in nodes.iter().filter(|o| o.wid != wid) {
        if let Some(c) = o.job {
            *counts.entry(c).or_default() += 1;
        }
    }
    let busy = counts.values().sum();
    pick_core(n, jobs, &counts, busy, &|sel| nodes.iter().filter(|o| o.takes_tasks && sel.matches(&o.attrs)).count())
}

/// pick_job's decision for node `n` (already known to take tasks and not be reserved) from precomputed facts, so the head
/// can call it without building a NodeView per node: `counts` = nodes other than `n` per job they currently work for,
/// `busy` = their sum, `share(sel)` = nodes that take tasks and match `sel`. O(jobs), the head's one-pass tallies are O(workers).
pub fn pick_core(n: &NodeView, jobs: &[JobView], counts: &std::collections::HashMap<u64, usize>, busy: usize, share: &dyn Fn(&Selector) -> usize) -> Option<u64> {
    let assigned = |id: u64| counts.get(&id).copied().unwrap_or(0);
    // SCH-04: nodes (other than this one) working for a user; a node on a job that is not in `jobs` counts for user ""
    let by_user = |u: &str| jobs.iter().filter(|j| j.user == u).map(|j| assigned(j.id)).sum::<usize>() + if u.is_empty() { busy - jobs.iter().map(|j| assigned(j.id)).sum::<usize>() } else { 0 };
    let key = |j: &JobView| (j.priority, by_user(&j.user), assigned(j.id), j.id);
    let elig: Vec<&JobView> = jobs.iter().filter(|j| j.has_work && j.selector.matches(&n.attrs)).collect();
    let best = elig.iter().min_by_key(|j| key(j))?;
    let Some(cur) = n.job.and_then(|c| elig.iter().find(|j| j.id == c)) else { return Some(best.id) };
    if best.id == cur.id {
        return Some(cur.id);
    }
    if best.priority == cur.priority {
        // equal priority: move at this chunk boundary only if it makes the split strictly more even (user, then job)
        let (kb, kc) = (key(best), key(cur));
        let same_user = best.user == cur.user;
        return Some(if kb.1 < kc.1 || (same_user && kb.2 < kc.2) { best.id } else { cur.id });
    }
    // SCH-09: a strictly higher priority job pulls this node away at its chunk boundary, up to its fair share
    // ponytail: fair share = ceil(matching nodes / has-work jobs of that priority); ignores overlap between selectors
    let share = share(&best.selector);
    let same = jobs.iter().filter(|j| j.has_work && j.priority == best.priority).count().max(1);
    Some(if assigned(best.id) < share.div_ceil(same) { best.id } else { cur.id })
}

/// SCH-04 quotas. `q` = (max_pcs, max_running) of the job's owner, 0 = no limit; `rank` = the owner's older unfinished jobs;
/// `holders` = PCs holding any chunk of the owner's jobs; `holds` = this PC is one of them. A job beyond the owner's
/// max_running waits; a PC that is not yet working for the owner is refused while max_pcs PCs already are.
pub fn quota_ok(q: (u32, u32), rank: usize, holders: usize, holds: bool) -> bool {
    (q.1 == 0 || rank < q.1 as usize) && (q.0 == 0 || holds || holders < q.0 as usize)
}

/// A node that could take a chunk of a job now (matches, can work), for `place`.
#[derive(Clone, Debug)]
pub struct PlaceNode {
    pub node_id: String,
    /// units/s for the job's kind (measured, else calibration)
    pub rate: f64,
    pub threads: u32,
    /// already has chunks of this job in flight
    pub holds: bool,
    /// wave 13: already caches the blobs the job needs (no transfer before it can start): preferred when the job is limited
    pub cached: bool,
}

/// Wave 10b: node ids allowed to take a NEW chunk of one job now. `max_pcs` 0 = no limit. Never more than max_pcs
/// nodes hold the job: a non-holder is refused while max_pcs holders exist, and a holder outside the chosen set only
/// drains what it has. spread: everyone (holders first when limited); pack: holders, then most threads; fastest:
/// highest rate; efficient: the fewest fastest nodes whose summed rate is at least spread's total / 1.2 (so the job
/// takes about 1.2 x as long as spread and the others can sleep). Pure.
pub fn place(mode: &str, max_pcs: u32, nodes: &[PlaceNode]) -> std::collections::BTreeSet<String> {
    let mut v: Vec<&PlaceNode> = nodes.iter().collect();
    let by_rate = |a: &&PlaceNode, b: &&PlaceNode| b.rate.total_cmp(&a.rate).then_with(|| a.node_id.cmp(&b.node_id));
    match mode {
        "pack" => v.sort_by(|a, b| b.holds.cmp(&a.holds).then(b.cached.cmp(&a.cached)).then(b.threads.cmp(&a.threads)).then_with(|| by_rate(a, b))),
        "fastest" | "efficient" => v.sort_by(|a, b| b.cached.cmp(&a.cached).then_with(|| by_rate(a, b))),
        _ => v.sort_by(|a, b| b.holds.cmp(&a.holds).then(b.cached.cmp(&a.cached)).then_with(|| by_rate(a, b))),
    }
    let mut k = if max_pcs == 0 { v.len() } else { (max_pcs as usize).min(v.len()) };
    let total: f64 = v.iter().map(|n| n.rate).sum();
    if mode == "efficient" && total > 0.0 {
        let (mut sum, mut need) = (0.0, 0);
        for n in &v {
            if sum >= total / 1.2 {
                break;
            }
            sum += n.rate;
            need += 1;
        }
        k = k.min(need.max(1));
    }
    let holders = nodes.iter().filter(|n| n.holds).count();
    v.into_iter().take(k).filter(|n| n.holds || max_pcs == 0 || holders < max_pcs as usize).map(|n| n.node_id.clone()).collect()
}

/// Effective thread cap of a node: min over matching caps; None = no cap.
pub fn thread_cap(attrs: &Attrs, caps: &[CapView]) -> Option<u32> {
    caps.iter().filter(|c| c.selector.matches(attrs)).filter_map(|c| c.max_threads).min()
}

// ---------------------------------------------------------------- M3 contract (SCH-03, SCH-06, SCH-08, ORG-07, ORG-08).
// Pure: time comes in as a Clock.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkPolicy {
    /// one size for the whole job: the job's chunk, or auto = total / (16 x nodes) clamped (what the paper measured)
    Fixed,
    /// guided self-scheduling: remaining / (2 x nodes), never below min
    Guided,
    /// node rate x TARGET_SECS clamped to [min, max]; once the remaining work is under 2 x fleet rate x TARGET_SECS, guided
    Adaptive,
}

/// Tunable "target_chunk_s" (default 1 s).
pub fn target_secs() -> f64 {
    crate::tune::get("target_chunk_s")
}

/// "" or "adaptive" -> Adaptive, "fixed", "guided"; anything else None.
pub fn parse_policy(s: &str) -> Option<ChunkPolicy> {
    match s {
        "" | "adaptive" => Some(ChunkPolicy::Adaptive),
        "fixed" => Some(ChunkPolicy::Fixed),
        "guided" => Some(ChunkPolicy::Guided),
        _ => None,
    }
}

/// Length of the next chunk for one node. `fixed_chunk` = the job's chunk field (0 = auto). `node_rate`/`fleet_rate` in
/// units per second (0 = unknown: fall back to Fixed's auto size). Always within [min, remaining] (min clamped down to
/// remaining), and >= 1 when remaining > 0.
pub fn chunk_len(p: ChunkPolicy, fixed_chunk: u64, total: u64, remaining: u64, node_rate: f64, fleet_rate: f64, nodes: usize, min: u64, max: u64) -> u64 {
    if remaining == 0 {
        return 0;
    }
    let n = nodes.max(1) as u64;
    // the paper's rule: total / (16 x PCs) clamped to 10,000..20,000,000
    let fixed = || if fixed_chunk > 0 { fixed_chunk } else { (total / (16 * n)).clamp(10_000, 20_000_000) };
    let guided = || remaining / (2 * n);
    let v = match p {
        ChunkPolicy::Fixed => fixed(),
        ChunkPolicy::Guided => guided(),
        ChunkPolicy::Adaptive if node_rate > 0.0 && fleet_rate > 0.0 => {
            if (remaining as f64) < 2.0 * fleet_rate * target_secs() { guided() } else { (node_rate * target_secs()) as u64 }
        }
        ChunkPolicy::Adaptive => fixed(),
    };
    // a chunk size given with adaptive/guided is a cap: rates measured on earlier, faster jobs must not hand a slow
    // job huge chunks (seen with SDK list jobs after fast ones)
    let v = if fixed_chunk > 0 && p != ChunkPolicy::Fixed { v.min(fixed_chunk) } else { v };
    let v = if p == ChunkPolicy::Fixed { v } else { v.clamp(min, max.max(min)) };
    v.clamp(min.min(remaining).max(1), remaining)
}

/// One chunk in flight, for the speculative tail (SCH-08).
#[derive(Clone, Debug)]
pub struct Inflight {
    pub job: u64,
    pub a: u64,
    pub b: u64,
    pub age_secs: f64,
    /// (b - a) / the holder's measured rate; 0 = unknown (never speculated)
    pub expected_secs: f64,
    /// copies already running (1 = only the original)
    pub copies: u32,
}

/// Chunks worth a second copy on an idle node: the job has no undispatched work, the chunk is older than 2 x expected,
/// it has only 1 copy. Oldest-overdue first, at most `idle_nodes`. First result wins (the head drops the other).
pub fn speculate(inflight: &[Inflight], idle_nodes: usize) -> Vec<(u64, u64, u64)> {
    let mut v: Vec<(f64, (u64, u64, u64))> = inflight
        .iter()
        .filter(|c| c.copies == 1 && c.expected_secs > 0.0 && c.age_secs > 2.0 * c.expected_secs)
        .map(|c| (c.age_secs - 2.0 * c.expected_secs, (c.job, c.a, c.b)))
        .collect();
    v.sort_by(|x, y| y.0.total_cmp(&x.0));
    v.into_iter().take(idle_nodes).map(|x| x.1).collect()
}

/// Seconds left at the fleet's current rate (SCH-03); None when the rate is 0 or unknown.
pub fn eta_secs(remaining: u64, fleet_rate: f64) -> Option<f64> {
    (fleet_rate > 0.0).then(|| remaining as f64 / fleet_rate)
}

/// Local wall clock for time rules: Unix ms, weekday (0 = Monday), minute of the local day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub ms: i64,
    pub weekday: u8,
    pub minute: u32,
}

/// Clock from Unix ms and a UTC offset in minutes (CK_TZ_OFFSET_MIN, default 330 = IST).
pub fn clock(ms: i64, offset_min: i32) -> Clock {
    const DAY: i64 = 86_400_000;
    let local = ms + offset_min as i64 * 60_000;
    // day 0 (1970-01-01) was a Thursday = 3
    Clock { ms, weekday: (local.div_euclid(DAY) + 3).rem_euclid(7) as u8, minute: (local.rem_euclid(DAY) / 60_000) as u32 }
}

#[derive(Clone, Debug)]
pub struct WindowView {
    pub selector: Selector,
    /// bit 0 = Monday .. bit 6 = Sunday
    pub days: u8,
    pub start_min: u32,
    /// may be <= start_min: the window runs past midnight (its days are the days it starts on)
    pub end_min: u32,
    pub block: bool,
    pub cap: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ReservationView {
    pub selector: Selector,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// "mon-fri", "sat,sun", "mon,wed-fri" -> day mask; None when unreadable.
pub fn parse_days(s: &str) -> Option<u8> {
    const NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let day = |t: &str| NAMES.iter().position(|n| *n == t.trim().to_ascii_lowercase());
    let mut mask = 0u8;
    for tok in s.split(',') {
        let (a, b) = match tok.split_once('-') {
            Some((a, b)) => (day(a)?, day(b)?),
            None => (day(tok)?, day(tok)?),
        };
        let mut d = a;
        loop {
            mask |= 1 << d;
            if d == b {
                break;
            }
            d = (d + 1) % 7; // "fri-mon" wraps
        }
    }
    Some(mask)
}

/// "09:30" -> 570; None when unreadable.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60 && m.to_string().len() <= 2 && h.to_string().len() <= 2).then_some(h * 60 + m)
}

/// Time rules for one node now: (blocked, thread cap). Blocked when an active reservation or a "block" window matches;
/// cap = the minimum cap of matching active "cap" windows.
pub fn time_rule(attrs: &Attrs, now: Clock, windows: &[WindowView], reservations: &[ReservationView]) -> (bool, Option<u32>) {
    let mut blocked = reservations.iter().any(|r| r.start_ms <= now.ms && now.ms < r.end_ms && r.selector.matches(attrs));
    let mut cap: Option<u32> = None;
    let (today, yday, m) = (now.weekday, (now.weekday + 6) % 7, now.minute);
    for w in windows.iter().filter(|w| w.selector.matches(attrs)) {
        let on = |d: u8| w.days >> d & 1 == 1;
        let active = if w.start_min < w.end_min {
            on(today) && (w.start_min..w.end_min).contains(&m)
        } else {
            (on(today) && m >= w.start_min) || (on(yday) && m < w.end_min) // overnight (start == end: 24 h)
        };
        if active {
            if w.block {
                blocked = true;
            } else if let Some(c) = w.cap {
                cap = Some(cap.map_or(c, |o| o.min(c)));
            }
        }
    }
    (blocked, cap)
}

#[cfg(test)]
mod tests {

    /// pick_core (what the head calls) agrees with the original per-node-view algorithm on pseudo-random fleets.
    #[test]
    fn pick_core_matches_reference() {
        fn reference(wid: u64, nodes: &[NodeView], jobs: &[JobView]) -> Option<u64> {
            let n = nodes.iter().find(|n| n.wid == wid)?;
            if !n.takes_tasks {
                return None;
            }
            let assigned = |id: u64| nodes.iter().filter(|o| o.wid != wid && o.job == Some(id)).count();
            let user_of = |id: u64| jobs.iter().find(|j| j.id == id).map_or("", |j| j.user.as_str());
            let by_user = |u: &str| nodes.iter().filter(|o| o.wid != wid && o.job.is_some_and(|c| user_of(c) == u)).count();
            let key = |j: &JobView| (j.priority, by_user(&j.user), assigned(j.id), j.id);
            let elig: Vec<&JobView> = jobs.iter().filter(|j| j.has_work && j.selector.matches(&n.attrs)).collect();
            let best = elig.iter().min_by_key(|j| key(j))?;
            let Some(cur) = n.job.and_then(|c| elig.iter().find(|j| j.id == c)) else { return Some(best.id) };
            if best.id == cur.id {
                return Some(cur.id);
            }
            if best.priority == cur.priority {
                let (kb, kc) = (key(best), key(cur));
                let same_user = best.user == cur.user;
                return Some(if kb.1 < kc.1 || (same_user && kb.2 < kc.2) { best.id } else { cur.id });
            }
            let share = nodes.iter().filter(|o| o.takes_tasks && best.selector.matches(&o.attrs)).count();
            let same = jobs.iter().filter(|j| j.has_work && j.priority == best.priority).count().max(1);
            Some(if assigned(best.id) < share.div_ceil(same) { best.id } else { cur.id })
        }
        let mut x = 12345u64;
        let mut r = move |m: u64| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for _ in 0..500 {
            let nj = 1 + r(5);
            let jobs: Vec<JobView> = (1..=nj).map(|id| JobView { id, priority: r(3) as u8, selector: Selector::default(), has_work: r(4) != 0, user: ["", "a", "b"][r(3) as usize].into() }).collect();
            let nodes: Vec<NodeView> = (0..1 + r(12)).map(|w| NodeView { wid: w, node_id: format!("n{w}"), attrs: Default::default(), takes_tasks: r(6) != 0, job: [None, Some(1 + r(7))][r(2) as usize], inflight: 0 }).collect();
            for n in &nodes {
                assert_eq!(pick_job(n.wid, &nodes, &jobs, &[]), reference(n.wid, &nodes, &jobs));
            }
        }
    }
    use super::*;
    use crate::select::parse;

    fn nv(i: u64, attrs: &[(&str, &str)]) -> NodeView {
        NodeView {
            wid: i,
            node_id: format!("pc{i:02}"),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            takes_tasks: true,
            job: None,
            inflight: 0,
        }
    }
    fn job(id: u64, priority: u8, sel: &str) -> JobView {
        JobView { id, priority, selector: parse(sel).unwrap(), has_work: true, user: format!("u{id}") }
    }
    fn res(sel: &str, n: u32, pct: u32) -> ReserveView {
        ReserveView { selector: parse(sel).unwrap(), keep_n: n, keep_pct: pct }
    }
    /// Ask every node in order, committing each answer (as the head does when it hands out a chunk).
    fn settle(nodes: &mut Vec<NodeView>, jobs: &[JobView], rs: &[ReserveView]) {
        for i in 0..nodes.len() {
            let w = nodes[i].wid;
            let p = pick_job(w, nodes, jobs, rs);
            nodes[i].job = p;
            nodes[i].inflight = p.is_some() as usize;
        }
    }
    fn count(nodes: &[NodeView], j: u64) -> usize {
        nodes.iter().filter(|n| n.job == Some(j)).count()
    }

    #[test]
    fn same_priority_splits_evenly() {
        let mut nodes: Vec<_> = (1..=10).map(|i| nv(i, &[])).collect();
        settle(&mut nodes, &[job(1, 2, ""), job(2, 2, "")], &[]);
        assert_eq!((count(&nodes, 1), count(&nodes, 2)), (5, 5));
        // sticky: asking again changes nothing
        let before: Vec<_> = nodes.iter().map(|n| n.job).collect();
        settle(&mut nodes, &[job(1, 2, ""), job(2, 2, "")], &[]);
        assert_eq!(before, nodes.iter().map(|n| n.job).collect::<Vec<_>>());
    }

    #[test]
    fn users_split_equally_not_by_job_count() {
        // ann runs jobs 1 and 2, bob job 3, all priority 2: PCs split 6/6 by user, ann's two jobs share her 6
        let mut jobs = vec![job(1, 2, ""), job(2, 2, ""), job(3, 2, "")];
        jobs[0].user = "ann".into();
        jobs[1].user = "ann".into();
        jobs[2].user = "bob".into();
        let mut nodes: Vec<_> = (1..=12).map(|i| nv(i, &[])).collect();
        settle(&mut nodes, &jobs, &[]);
        assert_eq!(count(&nodes, 3), 6);
        assert_eq!((count(&nodes, 1), count(&nodes, 2)), (3, 3));
        // bob arrives late: ann holds every node, and they rebalance at chunk boundaries
        let mut nodes: Vec<_> = (1..=8).map(|i| nv(i, &[])).collect();
        settle(&mut nodes, &jobs[..1], &[]);
        assert_eq!(count(&nodes, 1), 8);
        let two = [jobs[0].clone(), jobs[2].clone()];
        settle(&mut nodes, &two, &[]);
        assert_eq!((count(&nodes, 1), count(&nodes, 3)), (4, 4));
        settle(&mut nodes, &two, &[]); // stable
        assert_eq!((count(&nodes, 1), count(&nodes, 3)), (4, 4));
    }

    #[test]
    fn urgent_takes_free_nodes_first() {
        let mut nodes: Vec<_> = (1..=4).map(|i| nv(i, &[])).collect();
        settle(&mut nodes, &[job(1, 2, ""), job(2, 0, "")], &[]);
        assert_eq!(count(&nodes, 2), 4);
        assert_eq!(count(&nodes, 1), 0);
        // urgent finished: normal job gets them
        settle(&mut nodes.iter_mut().map(|n| { n.job = None; n.clone() }).collect(), &[job(1, 2, "")], &[]);
        assert_eq!(pick_job(1, &nodes, &[job(1, 2, "")], &[]), Some(1));
    }

    #[test]
    fn busy_node_keeps_job_unless_strictly_higher_priority() {
        let mut nodes: Vec<_> = (1..=4).map(|i| nv(i, &[])).collect();
        for n in &mut nodes {
            n.job = Some(1);
            n.inflight = 2;
        }
        // equal priority newcomer of another user (SCH-04): the nodes even out at their chunk boundaries, 2 and 2
        let jobs = [job(1, 2, ""), job(2, 2, "")];
        settle(&mut nodes, &jobs, &[]);
        assert_eq!((count(&nodes, 1), count(&nodes, 2)), (2, 2));
        for n in &mut nodes {
            n.job = Some(1);
        }
        // lower priority newcomer: nobody moves
        for n in &nodes {
            assert_eq!(pick_job(n.wid, &nodes, &[job(1, 2, ""), job(2, 3, "")], &[]), Some(1));
        }
        // strictly higher priority: nodes move until its fair share (all 4, it is alone at that priority)
        let jobs = [job(1, 2, ""), job(2, 1, "")];
        for i in 0..4 {
            let w = nodes[i].wid;
            let p = pick_job(w, &nodes, &jobs, &[]);
            assert_eq!(p, Some(2));
            nodes[i].job = p;
        }
        // two urgent jobs share 4 nodes: the third node asking sees job 2 already at its share (2) and stays
        let mut nodes: Vec<_> = (1..=4).map(|i| nv(i, &[])).collect();
        for n in &mut nodes {
            n.job = Some(1);
            n.inflight = 1;
        }
        let jobs = [job(1, 2, ""), job(2, 0, ""), job(3, 0, "")];
        settle(&mut nodes, &jobs, &[]);
        assert_eq!((count(&nodes, 2), count(&nodes, 3), count(&nodes, 1)), (2, 2, 0));
    }

    #[test]
    fn selector_mismatch_gets_nothing() {
        let nodes = vec![nv(1, &[("lab", "A")]), nv(2, &[("lab", "B")]), nv(3, &[])];
        let jobs = [job(1, 2, "lab=A")];
        assert_eq!(pick_job(1, &nodes, &jobs, &[]), Some(1));
        assert_eq!(pick_job(2, &nodes, &jobs, &[]), None);
        assert_eq!(pick_job(3, &nodes, &jobs, &[]), None);
        // falls through to a lower priority job it does match
        let jobs = [job(1, 0, "lab=A"), job(2, 3, "")];
        assert_eq!(pick_job(2, &nodes, &jobs, &[]), Some(2));
        // sticky job no longer matching is dropped
        let mut n = nodes.clone();
        n[1].job = Some(1);
        assert_eq!(pick_job(2, &n, &[job(1, 2, "lab=A"), job(2, 2, "")], &[]), Some(2));
        assert_eq!(pick_job(99, &nodes, &jobs, &[]), None);
    }

    #[test]
    fn no_work_or_no_tasks() {
        let mut nodes = vec![nv(1, &[]), nv(2, &[])];
        nodes[1].takes_tasks = false;
        let mut j = job(1, 2, "");
        assert_eq!(pick_job(1, &nodes, &[j.clone()], &[]), Some(1));
        assert_eq!(pick_job(2, &nodes, &[j.clone()], &[]), None);
        j.has_work = false;
        assert_eq!(pick_job(1, &nodes, &[j], &[]), None);
    }

    fn ids(s: &std::collections::BTreeSet<String>) -> Vec<&str> {
        s.iter().map(String::as_str).collect()
    }

    #[test]
    fn reserves_stable_across_joins() {
        let mk = |is: &[u64]| is.iter().map(|&i| nv(i, &[("lab", "A")])).collect::<Vec<_>>();
        let rs = [res("lab=A", 2, 0)];
        let all = mk(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(ids(&reserved(&all, &rs)), ["pc05", "pc06"]);
        // order of the input does not matter
        let mut shuffled = all.clone();
        shuffled.reverse();
        assert_eq!(reserved(&shuffled, &rs), reserved(&all, &rs));
        // lower-id nodes joining or leaving keep the same PCs free
        assert_eq!(ids(&reserved(&mk(&[2, 4, 5, 6]), &rs)), ["pc05", "pc06"]);
        assert_eq!(ids(&reserved(&mk(&[1, 2, 3, 4, 5, 6, 0]), &rs)), ["pc05", "pc06"]);
        // a non-matching node never counts
        let mut mixed = all.clone();
        mixed.push(nv(9, &[("lab", "B")]));
        assert_eq!(ids(&reserved(&mixed, &rs)), ["pc05", "pc06"]);
    }

    #[test]
    fn reserve_pct_rounding_and_union() {
        let nodes: Vec<_> = (1..=10).map(|i| nv(i, &[])).collect();
        assert_eq!(reserved(&nodes, &[res("", 0, 25)]).len(), 3); // ceil(2.5)
        assert_eq!(reserved(&nodes, &[res("", 0, 10)]).len(), 1);
        assert_eq!(reserved(&nodes, &[res("", 0, 11)]).len(), 2); // ceil(1.1)
        assert_eq!(reserved(&nodes, &[res("", 4, 10)]).len(), 4); // max(n, pct)
        assert_eq!(reserved(&nodes, &[res("", 0, 0)]).len(), 0);
        assert_eq!(reserved(&nodes, &[res("", 50, 200)]).len(), 10); // capped at matching
        assert_eq!(reserved(&nodes[..0], &[res("", 3, 0)]).len(), 0);
        let two = reserved(&nodes, &[res("", 1, 0), res("", 2, 0)]);
        assert_eq!(ids(&two), ["pc09", "pc10"]); // overlapping reserves do not stack
    }

    #[test]
    fn reserved_node_gets_none() {
        let nodes: Vec<_> = (1..=4).map(|i| nv(i, &[])).collect();
        let rs = [res("", 1, 0)];
        let jobs = [job(1, 2, "")];
        assert_eq!(pick_job(4, &nodes, &jobs, &rs), None);
        assert_eq!(pick_job(3, &nodes, &jobs, &rs), Some(1));
        let mut n = nodes.clone();
        n[3].job = Some(1); // even a node already on a job is released
        assert_eq!(pick_job(4, &n, &jobs, &rs), None);
    }

    #[test]
    fn caps_min_of_matching() {
        let a: Attrs = [("lab".to_string(), "A".to_string())].into();
        let caps = [
            CapView { selector: parse("lab=A").unwrap(), max_threads: Some(6) },
            CapView { selector: parse("").unwrap(), max_threads: Some(4) },
            CapView { selector: parse("lab=B").unwrap(), max_threads: Some(1) },
            CapView { selector: parse("").unwrap(), max_threads: None },
        ];
        assert_eq!(thread_cap(&a, &caps), Some(4));
        assert_eq!(thread_cap(&a, &caps[2..3]), None);
    }

    // ---- M3
    const DAYMS: i64 = 86_400_000;
    const IST: i32 = 330;
    fn win(sel: &str, days: &str, s: &str, e: &str, block: bool, cap: Option<u32>) -> WindowView {
        WindowView { selector: parse(sel).unwrap(), days: parse_days(days).unwrap(), start_min: parse_hhmm(s).unwrap(), end_min: parse_hhmm(e).unwrap(), block, cap }
    }
    fn at(wd: u8, hhmm: &str) -> Clock {
        Clock { ms: 0, weekday: wd, minute: parse_hhmm(hhmm).unwrap() }
    }

    #[test]
    fn policy_names() {
        assert_eq!(parse_policy(""), Some(ChunkPolicy::Adaptive));
        assert_eq!(parse_policy("adaptive"), Some(ChunkPolicy::Adaptive));
        assert_eq!(parse_policy("fixed"), Some(ChunkPolicy::Fixed));
        assert_eq!(parse_policy("guided"), Some(ChunkPolicy::Guided));
        assert_eq!(parse_policy("Fixed"), None);
        assert_eq!(parse_policy("x"), None);
    }

    #[test]
    fn chunk_policies() {
        use ChunkPolicy::*;
        // (policy, fixed, total, remaining, node_rate, fleet_rate, nodes, min, max) -> len
        let t: &[(ChunkPolicy, u64, u64, u64, f64, f64, usize, u64, u64, u64)] = &[
            // Fixed: job chunk, else the paper's rule
            (Fixed, 5000, 1_000_000, 1_000_000, 0.0, 0.0, 4, 1, 1 << 40, 5000),
            (Fixed, 5000, 1_000_000, 300, 0.0, 0.0, 4, 1, 1 << 40, 300),
            (Fixed, 0, 200_000_000, 200_000_000, 0.0, 0.0, 4, 1, 1 << 40, 3_125_000),
            (Fixed, 0, 1000, 1000, 0.0, 0.0, 4, 1, 1 << 40, 1000), // clamp 10_000 then down to remaining
            (Fixed, 0, 100_000, 100_000, 0.0, 0.0, 4, 1, 1 << 40, 10_000),
            (Fixed, 0, 100_000_000_000, 100_000_000_000, 0.0, 0.0, 1, 1, 1 << 40, 20_000_000),
            (Fixed, 0, 100, 100, 5.0, 9.0, 0, 1, 10, 100), // nodes 0 treated as 1
            // Guided: remaining / (2 x nodes), never below min
            (Guided, 0, 1000, 1000, 0.0, 0.0, 5, 1, 1 << 40, 100),
            (Guided, 0, 1000, 100, 0.0, 0.0, 5, 1, 1 << 40, 10),
            (Guided, 0, 1000, 8, 0.0, 0.0, 5, 3, 1 << 40, 3),
            (Guided, 0, 1000, 2, 0.0, 0.0, 5, 3, 1 << 40, 2), // min clamped to remaining
            (Guided, 0, 1000, 1, 0.0, 0.0, 5, 1, 1 << 40, 1),
            (Guided, 0, 1000, 1000, 0.0, 0.0, 1, 1, 50, 50), // max
            // Adaptive, unknown rate -> Fixed auto
            (Adaptive, 0, 200_000_000, 200_000_000, 0.0, 0.0, 4, 1, 1 << 40, 3_125_000),
            (Adaptive, 7, 1000, 1000, 0.0, 500.0, 4, 1, 1 << 40, 7),
            (Adaptive, 7, 1000, 1000, 100.0, 0.0, 4, 1, 1 << 40, 7),
            // Adaptive start: rate x 1 s clamped
            (Adaptive, 0, 1_000_000, 1_000_000, 2000.0, 8000.0, 4, 100, 10_000, 2000),
            (Adaptive, 0, 1_000_000, 1_000_000, 50.0, 8000.0, 4, 100, 10_000, 100),
            (Adaptive, 0, 1_000_000, 1_000_000, 90_000.0, 8000.0, 4, 100, 10_000, 10_000),
            // tail: remaining < 2 x fleet x 1 s -> guided
            (Adaptive, 0, 1_000_000, 15_999, 2000.0, 8000.0, 4, 10, 10_000, 1999),
            (Adaptive, 0, 1_000_000, 16_000, 2000.0, 8000.0, 4, 10, 10_000, 2000),
            (Adaptive, 0, 1_000_000, 40, 2000.0, 8000.0, 4, 10, 10_000, 10),
            (Adaptive, 0, 1_000_000, 5, 2000.0, 8000.0, 4, 10, 10_000, 5),
            (Adaptive, 0, 10, 0, 2000.0, 8000.0, 4, 10, 10_000, 0),
        ];
        for (i, &(p, fc, total, rem, nr, fr, n, mn, mx, want)) in t.iter().enumerate() {
            let got = chunk_len(p, fc, total, rem, nr, fr, n, mn, mx);
            assert_eq!(got, want, "row {i}");
            if rem > 0 {
                assert!(got >= 1 && got <= rem && got >= mn.min(rem) || p == Fixed, "row {i} bounds");
            }
        }
    }

    fn fl(job: u64, a: u64, age: f64, exp: f64, copies: u32) -> Inflight {
        Inflight { job, a, b: a + 10, age_secs: age, expected_secs: exp, copies }
    }

    #[test]
    fn speculation() {
        let v = [
            fl(1, 0, 5.0, 1.0, 1),  // overdue by 3
            fl(1, 10, 9.0, 1.0, 1), // overdue by 7: first
            fl(1, 20, 2.0, 1.0, 1), // exactly 2x: not older
            fl(1, 30, 50.0, 1.0, 2), // already copied
            fl(2, 40, 50.0, 0.0, 1), // unknown rate
            fl(2, 50, 3.0, 1.0, 1), // overdue by 1
        ];
        assert_eq!(speculate(&v, 10), vec![(1, 10, 20), (1, 0, 10), (2, 50, 60)]);
        assert_eq!(speculate(&v, 2), vec![(1, 10, 20), (1, 0, 10)]);
        assert_eq!(speculate(&v, 0), vec![]);
        assert_eq!(speculate(&[], 3), vec![]);
    }

    #[test]
    fn eta() {
        assert_eq!(eta_secs(100, 20.0), Some(5.0));
        assert_eq!(eta_secs(100, 0.0), None);
        assert_eq!(eta_secs(0, 5.0), Some(0.0));
    }

    #[test]
    fn clock_known_dates() {
        // 1970-01-01 00:00 UTC = Thursday; IST is +5:30
        assert_eq!(clock(0, 0), Clock { ms: 0, weekday: 3, minute: 0 });
        assert_eq!(clock(0, IST), Clock { ms: 0, weekday: 3, minute: 330 });
        // negative offset crosses to Wednesday 19:00
        let c = clock(0, -300);
        assert_eq!((c.weekday, c.minute), (2, 19 * 60));
        assert_eq!(clock(-1, 0).weekday, 2); // before the epoch
        assert_eq!(clock(-1, 0).minute, 1439);
        // 2026-11-04 06:30 UTC = 12:00 IST, a Wednesday (days since epoch 20751)
        let ms = 20_761 * DAYMS + (6 * 60 + 30) * 60_000;
        let c = clock(ms, IST);
        assert_eq!((c.weekday, c.minute), (2, 12 * 60));
        // 2026-11-04 20:00 UTC = 01:30 IST on Thursday
        let c = clock(20_761 * DAYMS + 20 * 3_600_000, IST);
        assert_eq!((c.weekday, c.minute), (3, 90));
        // 2000-01-01 (Saturday) 00:00 UTC
        assert_eq!(clock(946_684_800_000, 0).weekday, 5);
        // a week later is the same weekday
        assert_eq!(clock(ms + 7 * DAYMS, IST), Clock { ms: ms + 7 * DAYMS, ..clock(ms, IST) });
    }

    #[test]
    fn days_and_times() {
        for (s, want) in [
            ("mon-fri", Some(0b0011111)),
            ("sat,sun", Some(0b1100000)),
            ("mon,wed-fri", Some(0b0011101)),
            ("fri-mon", Some(0b1110001)),
            ("Sun", Some(0b1000000)),
            (" tue , thu ", Some(0b0001010)),
            ("mon-sun", Some(0x7f)),
            ("sun-sun", Some(0b1000000)),
            ("", None),
            ("mon-", None),
            ("funday", None),
            ("mon,,tue", None),
            ("1-5", None),
        ] {
            assert_eq!(parse_days(s), want, "{s:?}");
        }
        for (s, want) in [("09:30", Some(570)), ("0:00", Some(0)), ("23:59", Some(1439)), ("24:00", None), ("12:60", None), ("9", None), ("ab:cd", None), ("", None), ("-1:30", None), ("12:5x", None)] {
            assert_eq!(parse_hhmm(s), want, "{s:?}");
        }
    }

    #[test]
    fn time_rules() {
        let lab: Attrs = [("lab".to_string(), "A".to_string())].into();
        let other: Attrs = [("lab".to_string(), "B".to_string())].into();
        let ws = [win("lab=A", "mon-fri", "09:00", "17:00", true, None), win("", "mon-fri", "17:00", "19:00", false, Some(4)), win("lab=A", "mon-fri", "17:30", "18:00", false, Some(2))];
        let rule = |a: &Attrs, c: Clock| time_rule(a, c, &ws, &[]);
        assert_eq!(rule(&lab, at(2, "08:59")), (false, None));
        assert_eq!(rule(&lab, at(2, "09:00")), (true, None));
        assert_eq!(rule(&lab, at(2, "16:59")), (true, None));
        assert_eq!(rule(&lab, at(2, "17:00")), (false, Some(4))); // end exclusive; cap window starts
        assert_eq!(rule(&lab, at(2, "17:45")), (false, Some(2))); // min of caps
        assert_eq!(rule(&other, at(2, "12:00")), (false, None)); // selector
        assert_eq!(rule(&other, at(2, "17:45")), (false, Some(4)));
        assert_eq!(rule(&lab, at(5, "12:00")), (false, None)); // Saturday
        // block wins over a cap in the same minute
        let both = [win("", "mon-sun", "00:00", "23:59", false, Some(1)), win("", "mon-sun", "10:00", "11:00", true, Some(1))];
        assert_eq!(time_rule(&lab, at(0, "10:30"), &both, &[]), (true, Some(1)));
        // cap-less non-block window does nothing
        assert_eq!(time_rule(&lab, at(0, "10:30"), &[win("", "mon", "10:00", "11:00", false, None)], &[]), (false, None));
    }

    #[test]
    fn overnight_windows() {
        let lab: Attrs = Attrs::new();
        // Friday 22:00 -> 06:00 (days = the days it starts on)
        let ws = [win("", "fri", "22:00", "06:00", true, None)];
        let b = |wd, t: &str| time_rule(&lab, at(wd, t), &ws, &[]).0;
        assert!(!b(4, "21:59"));
        assert!(b(4, "22:00"));
        assert!(b(4, "23:59"));
        assert!(b(5, "01:00")); // Saturday 01:00 belongs to Friday's window
        assert!(b(5, "05:59"));
        assert!(!b(5, "06:00"));
        assert!(!b(5, "22:30")); // Saturday night not in days
        assert!(!b(4, "01:00")); // Friday 01:00 belongs to Thursday's window
        // Sunday window wraps into Monday
        let ws = [win("", "sun", "23:00", "02:00", true, None)];
        assert!(time_rule(&lab, at(0, "01:00"), &ws, &[]).0);
        // start == end: all day from the start day
        let ws = [win("", "wed", "08:00", "08:00", true, None)];
        assert!(time_rule(&lab, at(2, "09:00"), &ws, &[]).0);
        assert!(time_rule(&lab, at(3, "07:59"), &ws, &[]).0);
        assert!(!time_rule(&lab, at(3, "08:00"), &ws, &[]).0);
    }

    #[test]
    fn reservations_by_ms() {
        let lab: Attrs = [("lab".to_string(), "A".to_string())].into();
        let rs = [ReservationView { selector: parse("lab=A").unwrap(), start_ms: 1000, end_ms: 2000 }];
        let at_ms = |ms| time_rule(&lab, Clock { ms, weekday: 0, minute: 0 }, &[], &rs).0;
        assert!(!at_ms(999));
        assert!(at_ms(1000));
        assert!(at_ms(1999));
        assert!(!at_ms(2000));
        assert!(!time_rule(&Attrs::new(), Clock { ms: 1500, weekday: 0, minute: 0 }, &[], &rs).0);
    }
}

/// Dynamic heads (HA-10): what a PC brings as a head or standby. All inputs are measured by the head.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadInputs {
    pub calib_mhs: f64,  // FLT-06 speed test; 0 = unknown
    pub cores: u32,
    pub ram_gb: f64,
    pub losses_24h: u32, // times the head lost this PC in the last day
    pub in_use: bool,    // a person is using it (SCH-10)
}

/// Weights from the tunables head_w_speed / head_w_memory / head_w_stability / head_w_free.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadWeights {
    pub speed: f64,
    pub memory: f64,
    pub stability: f64,
    pub free: f64,
}

/// Pure: a 0..100 head-suitability score and its parts (each 0..1 before weighting), relative to the fleet's best
/// speed / cores / RAM so the numbers mean the same at 4 or 400 PCs.
pub fn head_score(x: &HeadInputs, best: &HeadInputs, w: HeadWeights) -> (f64, [f64; 4]) {
    let rel = |a: f64, b: f64| if b > 0.0 { (a / b).clamp(0.0, 1.0) } else { 0.0 };
    let parts = [
        rel(x.calib_mhs, best.calib_mhs),
        0.5 * rel(x.cores as f64, best.cores as f64) + 0.5 * rel(x.ram_gb, best.ram_gb),
        1.0 / (1.0 + x.losses_24h as f64), // 0 losses = 1, 1 = 0.5, 3 = 0.25
        if x.in_use { 0.0 } else { 1.0 },
    ];
    let ws = [w.speed, w.memory, w.stability, w.free];
    let total: f64 = ws.iter().sum();
    let score = if total > 0.0 { 100.0 * parts.iter().zip(ws).map(|(p, w)| p * w).sum::<f64>() / total } else { 0.0 };
    (score, parts)
}

#[cfg(test)]
mod chunk_cap_tests {
    use super::*;

    #[test]
    fn chunk_is_a_cap_for_adaptive() {
        let big = chunk_len(ChunkPolicy::Adaptive, 0, 1_000_000, 1_000_000, 100_000.0, 1_000.0, 4, 1, u64::MAX);
        let capped = chunk_len(ChunkPolicy::Adaptive, 10, 1_000_000, 1_000_000, 100_000.0, 1_000.0, 4, 1, u64::MAX);
        assert!(big > 10 && capped == 10);
        assert_eq!(chunk_len(ChunkPolicy::Fixed, 10, 1_000, 1_000, 0.0, 0.0, 4, 1, u64::MAX), 10);
    }
}

#[cfg(test)]
mod head_score_tests {
    use super::*;

    #[test]
    fn faster_stabler_free_pc_wins() {
        let w = HeadWeights { speed: 1.0, memory: 1.0, stability: 2.0, free: 1.0 };
        let best = HeadInputs { calib_mhs: 20.0, cores: 12, ram_gb: 32.0, ..Default::default() };
        let good = HeadInputs { calib_mhs: 20.0, cores: 12, ram_gb: 32.0, losses_24h: 0, in_use: false };
        let flaky = HeadInputs { losses_24h: 3, ..good.clone() };
        let busy = HeadInputs { in_use: true, ..good.clone() };
        let (g, _) = head_score(&good, &best, w);
        assert!((g - 100.0).abs() < 1e-9);
        assert!(head_score(&flaky, &best, w).0 < g && head_score(&busy, &best, w).0 < g);
        assert!(head_score(&flaky, &best, w).0 < head_score(&busy, &best, w).0, "stability weighs double");
        assert_eq!(head_score(&good, &best, HeadWeights { speed: 0.0, memory: 0.0, stability: 0.0, free: 0.0 }).0, 0.0);
    }
}

#[cfg(test)]
mod place_tests {
    use super::*;

    fn pn(id: &str, rate: f64, threads: u32, holds: bool) -> PlaceNode {
        PlaceNode { node_id: id.into(), rate, threads, holds, cached: false }
    }
    fn ids(s: std::collections::BTreeSet<String>) -> Vec<String> {
        s.into_iter().collect()
    }

    #[test]
    fn spread_is_everyone_unless_limited() {
        let n = [pn("a", 1.0, 4, false), pn("b", 2.0, 8, false), pn("c", 3.0, 2, false)];
        assert_eq!(ids(place("spread", 0, &n)), ["a", "b", "c"]);
        assert_eq!(ids(place("", 0, &n)), ["a", "b", "c"]);
        assert_eq!(ids(place("spread", 2, &n)), ["b", "c"]); // fastest two when nobody holds yet
    }

    #[test]
    fn max_pcs_never_exceeded() {
        // 3 holders at max 3: a newcomer is refused even if fastest
        let n = [pn("a", 1.0, 4, true), pn("b", 1.0, 4, true), pn("c", 1.0, 4, true), pn("z", 9.0, 4, false)];
        assert_eq!(ids(place("spread", 3, &n)), ["a", "b", "c"]);
        assert!(!place("fastest", 3, &n).contains("z"));
        assert!(!place("spread", 3, &n).contains("z"));
        assert!(!place("pack", 3, &n).contains("z"));
        // 2 holders at max 3: one newcomer may join, only one set member beyond holders is possible
        let n = [pn("a", 1.0, 4, true), pn("b", 1.0, 4, true), pn("y", 5.0, 4, false), pn("z", 9.0, 4, false)];
        assert!(place("spread", 3, &n).contains("a") && place("spread", 3, &n).len() == 3);
    }

    #[test]
    fn pack_prefers_holders_then_threads() {
        let n = [pn("a", 5.0, 2, false), pn("b", 1.0, 16, false), pn("c", 1.0, 4, true)];
        assert_eq!(ids(place("pack", 2, &n)), ["b", "c"]);
        assert_eq!(ids(place("pack", 0, &n)), ["a", "b", "c"]);
    }

    #[test]
    fn fastest_takes_highest_calib() {
        let n = [pn("a", 1.0, 4, false), pn("b", 7.0, 4, false), pn("c", 3.0, 4, false), pn("d", 5.0, 4, false)];
        assert_eq!(ids(place("fastest", 2, &n)), ["b", "d"]);
    }

    #[test]
    fn efficient_is_smallest_fast_set() {
        // total 10, need >= 8.33: 6 + 3 = 9 -> two nodes; the 1.0s sleep
        let n = [pn("a", 1.0, 4, false), pn("b", 6.0, 4, false), pn("c", 3.0, 4, false)];
        assert_eq!(ids(place("efficient", 0, &n)), ["b", "c"]);
        assert_eq!(ids(place("efficient", 1, &n)), ["b"]);
        // unknown rates: no basis to leave anyone out
        let z = [pn("a", 0.0, 4, false), pn("b", 0.0, 4, false)];
        assert_eq!(ids(place("efficient", 0, &z)), ["a", "b"]);
    }
}
    #[test]
    fn quota_rules() {
        // no limit
        assert!(quota_ok((0, 0), 9, 99, false));
        // max_running 2: the third job (rank 2) waits
        assert!(quota_ok((0, 2), 1, 0, false) && !quota_ok((0, 2), 2, 0, false));
        // max_pcs 2: a third PC is refused, a PC already working for the user is not
        assert!(quota_ok((2, 0), 0, 1, false) && !quota_ok((2, 0), 0, 2, false) && quota_ok((2, 0), 0, 2, true));
    }

