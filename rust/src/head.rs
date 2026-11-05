//! Head: jobs, chunk dispatch, requeue on disconnect, per-worker stats, discovery beacon.
//! Several jobs run at once (SCH-01): every accepted job is `running` (state Queued exists only in old rows) and waits
//! for eligible nodes; sched::pick_job decides per chunk which job a node serves. Owner: a3.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{self, Read};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::auth::{self, FrameMac};
use crate::jobs::{now_ms, takes_tasks, Ctx, JobRow, JobSpec, JobState, PRIORITIES};
use crate::proto::*;
use crate::sched::{self, CapView, JobView, NodeView, ReserveView};
use crate::select::{self, Attrs, Selector};
use crate::{die, Opts};

pub struct Job {
    pub id: u64,
    pub kind: u8,
    pub arg: Vec<u8>,
    pub next: u64,
    pub end: u64,
    pub chunk: u64,
    pub total: u64,
    pub left: u64,
    pub todo: u64,                // numbers this run has to compute (less than total after a resume)
    pub requeue: Vec<(u64, u64)>, // ranges still to hand out, lowest last; also chunks of workers that disconnected mid-chunk
    pub only: Vec<String>,        // worker addresses allowed to take chunks; empty = all
    pub started: Instant,
    pub selector: Selector,       // which nodes may run it (ORG-04)
    pub priority: u8,
    pub user: String, // submitter (SCH-04 fair share)
    pub policy: sched::ChunkPolicy, // SCH-06
    pub max_pcs: u32,               // wave 10b: at most this many PCs hold its chunks; 0 = no limit
    pub placement: String,          // spread | pack | fastest | efficient
    pub mem_mb: u64,
    pub blobs: Vec<String>, // wave 13: every blob of the job, in env order (CK_BLOB_<i>)
    pub blob_mode: String,
    pub shared: Vec<String>, // job-wide blobs (code): every PC that takes a chunk needs them
    pub shards: Vec<String>, // blobs named by @blob: items; item_sha[i] = 0 or 1 + index into shards
    pub item_sha: Vec<u32>,
}

impl Job {
    fn has_work(&self) -> bool {
        !self.requeue.is_empty() || self.next < self.end
    }
}

/// Summary of the most important running job, for dash.rs / api.rs written for one job. Prefer `State::jobs`.
pub struct JobBrief {
    pub id: u64,
    pub kind: u8,
    pub total: u64,
    pub left: u64,
    pub started: Instant,
}

/// Parsed M2 policy, refreshed by `refresh` (not per chunk).
#[derive(Default)]
pub struct Policy {
    pub reserves: Vec<ReserveView>,
    pub caps: Vec<CapView>,
}

#[derive(Default)]
pub struct Worker {
    pub addr: String,
    pub threads: u32,
    pub chunks: u64,
    pub numbers: u64,
    pub secs: f64, // total compute time, so a client can diff it around a job
    pub rate: f64,
    pub inflight: Vec<(u64, u64, u64)>, // (job, start, end) sent and not yet returned
    pub out: Option<Arc<Mutex<Out>>>,   // writer shared by the feeder (TASK), heartbeat and cancel (CANCEL)
    pub state: String,                  // node state (ORG-09), kept in step with the store by node_state_changed
    pub sent: HashMap<(u64, u64, u64), Instant>, // when each in-flight chunk was sent (per-task timeout)
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub version: u32,        // protocol version from HELLO_W (1 = old worker, no hello)
    pub hello: Option<Hello>, // node_id, host, virtual, inventory (FLT-02)
    pub attrs: Attrs,         // selector attributes, cached by refresh()
    pub cap: Option<u32>,     // thread cap last sent to the agent (CTRL): ORG-05 caps and window caps, minimum
    pub presence: String,     // "busy" (someone uses the PC: no new chunks) or "idle" (SCH-10)
    pub energy_j: f64,        // OBS-03: CPU package joules since the agent started (RAPL), 0 = unknown; real PCs only
    pub blocked: bool,        // a block window or reservation forbids new chunks now (ORG-07/08); not a stored state
    pub calib: f64,           // SHA-256 MH/s from the join benchmark (FLT-06), cached by refresh()
    pub fed: u64,  // TASKs sent on this connection
    pub back: u64, // RESULT / FAILED frames received; a REQ credit is granted only while back <= fed (resent results get none)
    pub told: BTreeSet<u64>, // jobs whose CTRL {job: mem_mb} this agent already got
    pub pending: Vec<(u64, u64, u64, u64)>, // HA-04: stored results waiting to be durable: (log seq, job, a, b); ACKed by durable_upto
    // wave 13 (src/blobs.rs)
    pub have: std::collections::HashSet<String>, // blobs this PC caches (it reports them)
    pub blob_port: u16,                          // its blob port; 0 = it does not serve (or is virtual)
    pub fetching: HashMap<String, crate::blobs::Fetch>,
    pub avoid: std::collections::HashSet<u64>,   // peers whose transfer to this PC failed
    pub told_b: BTreeSet<u64>,                   // jobs whose CTRL {job_blobs} this agent already got
    pub net_mbps: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub probed: Option<Instant>,
    pub probing: Option<Instant>,
    pub probe_now: bool,
    pub probe_burst: bool, // this probe only teaches the head its uplink: all PCs at once share it, so the per-PC speed is not stored
    pub blob_rep: bool,                        // the agent has reported its cache at least once
    pub staged: Option<(u64, u64, u64)>,       // a chunk reserved for this PC until its blobs have arrived (also in inflight)
}

#[derive(Default)]
pub struct State {
    pub jobs: BTreeMap<u64, Job>, // every unfinished job
    pub job: Option<JobBrief>,    // the (priority, id)-first of `jobs`; kept for older readers
    pub last: Option<String>,
    pub next_id: u64,
    pub workers: HashMap<u64, Worker>,
    pub events: VecDeque<(u64, f64, f64)>, // (worker id, chunk start, chunk end) in secs since head start
    pub subs: HashMap<u64, Vec<mpsc::Sender<(u8, Vec<u8>)>>>, // attached clients, by job id
    pub attempts: HashMap<(u64, u64, u64), u8>, // failed runs per (job, start, end) (JOB-06)
    pub policy: Policy,
    pub reserved: BTreeSet<String>, // nodes the reserves keep free now (for the change event)
    pub blocked: usize,             // nodes blocked by time rules at the last refresh (for the change event)
    pub fleet_cache: HashMap<(u64, u8), (Instant, usize, (usize, f64))>, // take(): fleet() per (job, kind), 100 ms stale at most
    pub rates: HashMap<(String, u8), f64>, // EWMA units/s per (node, kind) from RESULT compute time (SCH-07); survives reconnects
    pub halted: bool,                      // HA-06: a head with a higher epoch exists; dispatch nothing
    pub gated: bool,                       // HA-05: no quorum; dispatch nothing
    pub noq_since: Option<Instant>,        // since when connected nodes are not a majority of the membership
    pub cfg: Option<(u64, Instant)>,       // last "config" log entry: hash of its content, when built
    pub acct: HashMap<u64, crate::research::Acct>, // wave 12: per-job time accounting for the Breakdown
    pub paused_s: f64,                     // seconds this head did not dispatch (no quorum / halted), finished stretches
    pub pause_since: Option<Instant>,      // start of the current no-dispatch stretch
    pub quotas: HashMap<String, (u32, u32)>, // user -> (max_pcs, max_running); absent or 0 = no limit (SCH-04, meta "quotas")
}

fn who_of(w: &Worker) -> (String, String) {
    w.hello.as_ref().map_or_else(|| (String::new(), w.addr.clone()), |h| (h.node_id.clone(), h.host.clone()))
}

fn paused_now(st: &State) -> f64 {
    st.paused_s + st.pause_since.map_or(0.0, |t| t.elapsed().as_secs_f64())
}

/// A result that was kept: counters only (two hash lookups, no allocation after the PC's first chunk).
fn acct_kept(st: &mut State, job: u64, wid: u64, numbers: u64, secs: f64) {
    let Some(j) = st.jobs.get(&job) else { return };
    let t = j.started.elapsed().as_secs_f64();
    let (workers, p) = (&st.workers, paused_now_of(st.paused_s, st.pause_since));
    st.acct.entry(job).or_insert_with(|| crate::research::Acct { paused0: p, ..Default::default() }).kept(wid, || workers.get(&wid).map_or_else(Default::default, who_of), numbers, secs, t);
}

fn paused_now_of(s: f64, since: Option<Instant>) -> f64 {
    s + since.map_or(0.0, |t| t.elapsed().as_secs_f64())
}

/// Compute that was thrown away (duplicate, losing speculative copy, chunk of a lost PC).
fn acct_waste(st: &mut State, job: u64, wid: u64, secs: f64, who: Option<(String, String)>) {
    let Some(j) = st.jobs.get(&job) else { return };
    let t = j.started.elapsed().as_secs_f64();
    let (workers, p) = (&st.workers, paused_now_of(st.paused_s, st.pause_since));
    st.acct.entry(job).or_insert_with(|| crate::research::Acct { paused0: p, ..Default::default() }).wasted(wid, || who.unwrap_or_else(|| workers.get(&wid).map_or_else(Default::default, who_of)), secs, t);
}

/// Breakdown of a running job (live counters) or a finished one (stored when it ended).
pub fn breakdown(ctx: &Ctx, id: u64) -> Option<serde_json::Value> {
    let st = ctx.sh.0.lock().unwrap();
    if let (Some(j), Some(a)) = (st.jobs.get(&id), st.acct.get(&id)) {
        return Some(crate::research::breakdown(id, a, j.started.elapsed().as_secs_f64(), paused_now(&st) - a.paused0));
    }
    drop(st);
    ctx.store.job_stats(id).and_then(|t| serde_json::from_str(&t).ok())
}

/// Exit status of a head that heard a higher epoch (HA-06). main/kit restarts it as `ck head --standby --head <new head>`.
pub const FENCED_EXIT: i32 = 75;
static ACKS: AtomicU64 = AtomicU64::new(0);

/// HA-05: may the head dispatch? A candidate (just promoted) needs quorum; a primary that has standbys loses the right
/// after noq_grace_s without quorum. Pure.
fn dispatch_ok(role: &str, quorum: bool, standbys: usize, noq_for: Duration) -> bool {
    if role == "candidate" {
        return quorum;
    }
    !(standbys > 0 && !quorum && noq_for >= crate::tune::secs("noq_grace_s"))
}

/// Frames to write once the State lock is released.
type Sends = Vec<(Arc<Mutex<Out>>, u8, Vec<u8>)>;

fn flush(sends: Sends) {
    for (o, k, p) in sends {
        if k == ACK {
            ACKS.fetch_add(1, Relaxed);
        }
        let _ = o.lock().unwrap().put(k, &p);
    }
}

/// Pending results with seq <= `seq` leave the list; returns their (job, a, b). Pure.
fn drain_durable(pending: &mut Vec<(u64, u64, u64, u64)>, seq: u64) -> Vec<(u64, u64, u64)> {
    let (done, keep) = pending.drain(..).partition::<Vec<_>, _>(|t| t.0 <= seq);
    *pending = keep;
    done.into_iter().map(|t| (t.1, t.2, t.3)).collect()
}

/// HA-04: repl.rs calls this (without holding its own locks) when the log is durable up to `seq`: every pending result
/// at or below it gets an ACK, so the agent drops its copy. With no standby, finish() ACKs at once (repl::durable_seq).
pub fn durable_upto(ctx: &Ctx, seq: u64) {
    let mut sends = vec![];
    for w in ctx.sh.0.lock().unwrap().workers.values_mut() {
        let done = drain_durable(&mut w.pending, seq);
        if let Some(o) = &w.out {
            sends.extend(done.into_iter().map(|(j, a, b)| (o.clone(), ACK, pack(&[j, a, b]))));
        }
    }
    flush(sends);
}

/// Ids of the jobs being worked on (api.rs lists them in /cluster).
pub fn running_jobs(ctx: &Ctx) -> Vec<u64> {
    ctx.sh.0.lock().unwrap().jobs.keys().copied().collect()
}

fn sync_brief(st: &mut State) {
    st.job = st.jobs.values().min_by_key(|j| (j.priority, j.id)).map(|j| JobBrief { id: j.id, kind: j.kind, total: j.total, left: j.left, started: j.started });
}

/// Selector text -> Selector; empty text is "every node" (needs no parser).
pub fn parse_sel(text: &str) -> Result<Selector, String> {
    if text.trim().is_empty() { Ok(Selector::default()) } else { select::parse(text) }
}

/// The CTRL frame to send when a worker's thread cap differs from what the agent was last told.
fn cap_ctrl(sent: Option<u32>, want: Option<u32>) -> Option<Vec<u8>> {
    (sent != want).then(|| serde_json::json!({ "cap_threads": want }).to_string().into_bytes())
}

/// Write half of a worker connection: sealed after admission, plain with CK_FLEET=open.
pub struct Out {
    pub s: Sock,
    pub mac: Option<FrameMac>,
}

impl Out {
    pub fn put(&mut self, kind: u8, p: &[u8]) -> io::Result<()> {
        match &mut self.mac {
            Some(m) => send_sealed(&mut self.s, m, kind, p),
            None => send(&mut self.s, kind, p),
        }
    }
}

pub fn recv_any(r: &mut impl Read, mac: &mut Option<FrameMac>) -> io::Result<(u8, Vec<u8>)> {
    match mac {
        Some(m) => recv_sealed(r, m),
        None => recv(r),
    }
}


/// EWMA with alpha 0.3; the first sample is taken as is.
fn ewma(old: f64, x: f64) -> f64 {
    if old > 0.0 { 0.3 * x + 0.7 * old } else { x }
}

fn nkey(w: &Worker) -> String {
    w.hello.as_ref().map_or_else(|| w.addr.clone(), |h| h.node_id.clone())
}

/// A node may be given new chunks: state allows it, no time rule blocks it, nobody is using it.
pub(crate) fn can_work(w: &Worker) -> bool {
    takes_tasks(&w.state) && !w.blocked && w.presence != "busy"
}

/// Is a blob missing on this node for chunk [a, b) of the job (job-wide blobs plus the shards its items name)?
/// Virtual agents run cmd/list/SDK programs too (sharing their process's cache), so they need the blobs like any PC.
/// An agent that has not reported its cache yet counts as missing.
pub(crate) fn chunk_missing(w: &Worker, j: &Job, a: u64, b: u64) -> bool {
    if j.blobs.is_empty() {
        return false;
    }
    !w.blob_rep || crate::blobs::chunk_blobs(&j.shared, &j.shards, &j.item_sha, a, b).iter().any(|b| !w.have.contains(b))
}

/// A chunk of the job near the front of its unassigned work that `w` already has the blobs for (same length as `len`).
fn cached_chunk(w: &Worker, j: &Job, len: u64) -> Option<(u64, u64)> {
    let mut cand: Vec<(u64, u64)> = vec![];
    for r in j.requeue.iter().rev().take(8) {
        let mut s = r.0;
        while s < r.1 && cand.len() < 64 {
            cand.push((s, (s + len).min(r.1)));
            s += len.max(1);
        }
    }
    let mut s = j.next;
    while s < j.end && cand.len() < 128 {
        cand.push((s, (s + len).min(j.end)));
        s += len.max(1);
    }
    cand.into_iter().find(|c| !chunk_missing(w, j, c.0, c.1))
}

/// Take [s, e) out of the job's unassigned work.
fn cut(j: &mut Job, s: u64, e: u64) {
    if let Some(i) = j.requeue.iter().position(|r| r.0 <= s && e <= r.1) {
        let r = j.requeue.remove(i);
        if e < r.1 {
            j.requeue.push((e, r.1));
        }
        if r.0 < s {
            j.requeue.push((r.0, s));
        }
    } else if s >= j.next && e <= j.end {
        if s > j.next {
            j.requeue.push((j.next, s));
        }
        j.next = e;
    }
}

/// Units per second of a node for a kind: the measured EWMA, else (SHA only) the join benchmark; 0 = unknown.
fn node_rate(st: &State, w: &Worker, kind: u8) -> f64 {
    st.rates.get(&(nkey(w), kind)).copied().filter(|r| *r > 0.0).unwrap_or(if kind == KIND_SHA { w.calib * 1e6 } else { 0.0 })
}

/// (nodes that could work now and match, their summed rate) for a kind.
fn fleet(st: &State, sel: &Selector, kind: u8) -> (usize, f64) {
    st.workers.values().filter(|w| can_work(w) && sel.matches(&w.attrs)).fold((0, 0.0), |(n, r), w| (n + 1, r + node_rate(st, w, kind)))
}

/// Job id -> current fleet rate (units/s) for the nodes that can serve it (SCH-03: eta_ms).
pub fn job_rates(ctx: &Ctx) -> HashMap<u64, f64> {
    let st = ctx.sh.0.lock().unwrap();
    st.jobs.values().map(|j| (j.id, fleet(&st, &j.selector, j.kind).1)).collect()
}

/// (matching nodes that can work now, summed SHA rate or calibration) for /estimate (SCH-03).
pub fn fleet_rate_for(ctx: &Ctx, selector: &Selector) -> (usize, f64) {
    fleet(&ctx.sh.0.lock().unwrap(), selector, KIND_SHA)
}

/// Is a chunk still in flight on some connected node (a speculative copy or the original)?
fn held(st: &State, k: (u64, u64, u64)) -> bool {
    st.workers.values().any(|w| w.inflight.contains(&k))
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Requeue,
    Fail,
}

/// Count one failed run of a chunk; a retryable failure is requeued until max_attempts runs have failed.
fn attempt(m: &mut HashMap<(u64, u64, u64), u8>, key: (u64, u64, u64), retry: bool) -> Verdict {
    let n = m.entry(key).or_insert(0);
    *n += 1;
    if retry && (*n as u64) < crate::tune::n("max_attempts") { Verdict::Requeue } else { Verdict::Fail }
}

/// How long a chunk of `n` numbers may be in flight: 20x what the worker's measured rate predicts, at least 60 s.
fn task_limit(rate: f64, n: u64) -> Duration {
    Duration::from_secs_f64(if rate > 0.0 { (20.0 * n as f64 / rate).clamp(60.0, 86_400.0) } else { 60.0 })
}

enum Plan {
    Challenge(Vec<u8>),
    Join(String), // sha256 hex of the join code
    Deny(&'static str),
}

/// Admission decision for a HELLO_W in a secured fleet: a PC with a stored key must prove it; a new PC needs a code.
fn plan(key: Option<Vec<u8>>, code: Option<&str>) -> Plan {
    match (key, code.map(str::trim).filter(|c| !c.is_empty())) {
        (Some(k), _) => Plan::Challenge(k),
        (None, Some(c)) => Plan::Join(crate::users::sha256_hex(c.to_ascii_uppercase().as_bytes())), // codes are issued upper-case; typing them in lower case works too
        (None, None) => Plan::Deny("join code required"),
    }
}

/// (state a connecting node starts in, whether the head must write it to the store). Open fleets auto-approve.
fn join_state(open: bool, stored: Option<&str>) -> (String, bool) {
    match stored {
        Some(s) if !s.is_empty() && !(open && s == "pending") => (s.into(), false),
        Some(_) if open => ("active".into(), true),
        None if open => ("active".into(), false),
        _ => ("pending".into(), false),
    }
}


static START: OnceLock<Instant> = OnceLock::new();
pub fn now() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub type Shared = Arc<(Mutex<State>, Condvar)>;

/// Next range to hand out: re-queued/resumed ranges first (split to `chunk`), then fresh numbers.
fn next_chunk(requeue: &mut Vec<(u64, u64)>, next: &mut u64, end: u64, chunk: u64) -> Option<(u64, u64)> {
    if let Some((a, b)) = requeue.pop() {
        if b - a > chunk {
            requeue.push((a + chunk, b));
            return Some((a, a + chunk));
        }
        return Some((a, b));
    }
    if *next >= end {
        return None;
    }
    let a = *next;
    *next = (a + chunk).min(end);
    Some((a, *next))
}

/// Ranges of [from,end) not covered by `done` (sorted, merged), ascending.
fn gaps(from: u64, end: u64, done: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let (mut out, mut at) = (vec![], from);
    for &(a, b) in done {
        let (a, b) = (a.max(from), b.min(end));
        if b <= a {
            continue;
        }
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if at < end {
        out.push((at, end));
    }
    out
}

fn node_views(st: &State) -> Vec<NodeView> {
    st.workers
        .iter()
        .map(|(wid, w)| NodeView {
            wid: *wid,
            node_id: w.hello.as_ref().map_or_else(|| w.addr.clone(), |h| h.node_id.clone()),
            attrs: w.attrs.clone(),
            takes_tasks: can_work(w),
            job: w.inflight.last().map(|t| t.0), // the job it was fed last: where the PC is being steered
            inflight: w.inflight.len(),
        })
        .collect()
}

/// Does the job restrict where its chunks go (max_pcs, or a placement other than spread)?
fn limited(j: &Job) -> bool {
    j.max_pcs > 0 || !matches!(j.placement.as_str(), "" | "spread")
}

/// May node `wid` take a new chunk of `j` under its max_pcs and placement? Not limited = yes, without a scan.
fn placed(st: &State, j: &Job, wid: u64) -> bool {
    if !limited(j) {
        return true;
    }
    let nodes: Vec<(u64, sched::PlaceNode)> = st
        .workers
        .iter()
        .filter(|(_, w)| can_work(w) && j.selector.matches(&w.attrs) && (j.only.is_empty() || j.only.contains(&w.addr)))
        .map(|(i, w)| {
            let r = node_rate(st, w, j.kind);
            (*i, sched::PlaceNode { node_id: nkey(w), rate: if r > 0.0 { r } else { w.calib * 1e6 }, threads: w.threads, holds: w.inflight.iter().any(|t| t.0 == j.id), cached: !j.blobs.is_empty() && j.blobs.iter().all(|b| w.have.contains(b)) })
        })
        .collect();
    let (_, me) = match nodes.iter().find(|n| n.0 == wid) {
        Some(n) => n,
        None => return false,
    };
    let me = me.node_id.clone();
    let pn: Vec<sched::PlaceNode> = nodes.into_iter().map(|n| n.1).collect();
    sched::place(&j.placement, j.max_pcs, &pn).contains(&me)
}

/// Workers holding a chunk of any of `user`'s jobs.
fn user_pcs(st: &State, user: &str) -> usize {
    st.workers.values().filter(|w| w.inflight.iter().any(|t| st.jobs.get(&t.0).is_some_and(|j| j.user == user))).count()
}

/// SCH-04 quotas: may `wid` take a new chunk of `j` under its owner's max_pcs and max_running? No quota = yes, without a scan.
fn quota_ok(st: &State, j: &Job, wid: u64) -> bool {
    let Some(&q) = st.quotas.get(&j.user) else { return true };
    let rank = st.jobs.values().filter(|o| o.user == j.user && o.id < j.id).count();
    let holds = st.workers.get(&wid).is_some_and(|w| w.inflight.iter().any(|t| st.jobs.get(&t.0).is_some_and(|o| o.user == j.user)));
    sched::quota_ok(q, rank, user_pcs(st, &j.user), holds)
}

/// Why a running job holds no PC right now: "quota" | "priority" | "no_pcs", None when it is running or about to.
pub fn wait_reason(ctx: &Ctx, id: u64) -> Option<&'static str> {
    let st = ctx.sh.0.lock().unwrap();
    let j = st.jobs.get(&id)?;
    if !j.has_work() || st.workers.values().any(|w| w.inflight.iter().any(|t| t.0 == id)) {
        return None;
    }
    let ok = |w: &Worker| can_work(w) && j.selector.matches(&w.attrs) && (j.only.is_empty() || j.only.contains(&w.addr));
    let free: Vec<&Worker> = st.workers.values().filter(|w| ok(w) && w.inflight.is_empty()).collect();
    if let Some(&q) = st.quotas.get(&j.user) {
        let rank = st.jobs.values().filter(|o| o.user == j.user && o.id < j.id).count();
        if !sched::quota_ok(q, rank, user_pcs(&st, &j.user), false) {
            return Some("quota");
        }
    }
    if free.is_empty() {
        return Some("no_pcs");
    }
    st.jobs.values().any(|o| o.priority < j.priority && o.has_work()).then_some("priority")
}

/// Quota of one user: (max_pcs, max_running), 0 = no limit.
pub fn quota_of(ctx: &Ctx, user: &str) -> (u32, u32) {
    ctx.store.meta("quotas").and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).map_or((0, 0), |v| (v[user]["max_pcs"].as_u64().unwrap_or(0) as u32, v[user]["max_running"].as_u64().unwrap_or(0) as u32))
}

fn load_quotas(ctx: &Ctx, st: &mut State) {
    let v: serde_json::Value = ctx.store.meta("quotas").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    st.quotas = v.as_object().into_iter().flatten().map(|(u, q)| (u.clone(), (q["max_pcs"].as_u64().unwrap_or(0) as u32, q["max_running"].as_u64().unwrap_or(0) as u32))).filter(|(_, q)| *q != (0, 0)).collect();
}

/// api.rs after PUT /api/v1/users/{name}/quota: meta "quotas" changed; dispatch re-reads it at once.
pub fn quotas_changed(ctx: &Ctx) {
    load_quotas(ctx, &mut ctx.sh.0.lock().unwrap());
    ctx.sh.1.notify_all();
}

/// Next chunk for `wid`: sched::pick_job chooses the job, sched::chunk_len the length, then the range comes from the job's list.
/// ponytail: views are rebuilt per chunk, O(workers); cache them if a fleet of thousands shows it.
fn take(st: &mut State, wid: u64) -> Option<(u64, u64, u64, u8, Vec<u8>)> {
    if st.halted || st.gated {
        return None;
    }
    if let Some((sj, a, b)) = st.workers.get(&wid)?.staged {
        // wave 13: a chunk is reserved for this PC while its blobs arrive
        let Some(j) = st.jobs.get(&sj) else {
            let w = st.workers.get_mut(&wid)?;
            w.staged = None;
            w.inflight.retain(|t| *t != (sj, a, b));
            return None;
        };
        if chunk_missing(&st.workers[&wid], j, a, b) {
            return None;
        }
        let out = (sj, a, b, j.kind, j.arg.clone());
        let w = st.workers.get_mut(&wid)?;
        w.staged = None;
        w.inflight.retain(|t| *t != (sj, a, b)); // feed() records it as sent
        return Some(out);
    }
    let addr = st.workers.get(&wid)?.addr.clone();
    let id = if st.policy.reserves.is_empty() && st.jobs.len() == 1 && st.quotas.is_empty() && st.jobs.values().all(|j| !limited(j)) {
        // fast path, same answer as sched::pick_job with one job and no reserves (which is O(workers) with attrs cloned
        // per node, ~1 ms at 1,000 nodes, under the State lock, once per result)
        let (w, j) = (&st.workers[&wid], st.jobs.values().next()?);
        (can_work(w) && j.has_work() && (j.only.is_empty() || j.only.contains(&addr)) && j.selector.matches(&w.attrs)).then_some(j.id)?
    } else if st.policy.reserves.is_empty() {
        // same answer as the branch below (sched::pick_job), without a NodeView (two Strings + attrs clone) per node:
        // one pass tallies how many other nodes work for each job, sched::pick_core decides from the tallies
        let jv: Vec<JobView> = st
            .jobs
            .values()
            .map(|j| JobView { id: j.id, priority: j.priority, user: j.user.clone(), selector: j.selector.clone(), has_work: j.has_work() && (j.only.is_empty() || j.only.contains(&addr)) && placed(st, j, wid) && quota_ok(st, j, wid) })
            .collect();
        let w = st.workers.get(&wid)?;
        if !can_work(w) {
            return None;
        }
        let me = NodeView { wid, node_id: String::new(), attrs: w.attrs.clone(), takes_tasks: true, job: w.inflight.last().map(|t| t.0), inflight: w.inflight.len() };
        let mut counts: HashMap<u64, usize> = HashMap::new();
        let mut busy = 0;
        for (i, o) in st.workers.iter() {
            if *i != wid {
                if let Some(t) = o.inflight.last() {
                    *counts.entry(t.0).or_default() += 1;
                    busy += 1;
                }
            }
        }
        sched::pick_core(&me, &jv, &counts, busy, &|sel| st.workers.values().filter(|o| can_work(o) && sel.matches(&o.attrs)).count())?
    } else {
        let views = node_views(st);
        let jv: Vec<JobView> = st
            .jobs
            .values()
            .map(|j| JobView { id: j.id, priority: j.priority, user: j.user.clone(), selector: j.selector.clone(), has_work: j.has_work() && (j.only.is_empty() || j.only.contains(&addr)) && placed(st, j, wid) && quota_ok(st, j, wid) })
            .collect();
        sched::pick_job(wid, &views, &jv, &st.policy.reserves)?
    };
    let j = st.jobs.get(&id)?;
    let remaining = j.requeue.iter().map(|r| r.1 - r.0).sum::<u64>() + j.end.saturating_sub(j.next);
    let (nodes, fleet_rate) = {
        // fleet() is O(workers) with a String per node; the EWMA rates it sums move slowly, so reuse it for 100 ms
        let key = (id, j.kind);
        match st.fleet_cache.get(&key) {
            Some((t, n, v)) if *n == st.workers.len() && t.elapsed() < Duration::from_millis(100) => *v,
            _ => {
                let v = fleet(st, &j.selector, j.kind);
                st.fleet_cache.insert(key, (Instant::now(), st.workers.len(), v));
                v
            }
        }
    };
    let j = st.jobs.get(&id)?;
    let node_rate = node_rate(st, &st.workers[&wid], j.kind);
    // A PC runs one chunk at a time and splits a list chunk over its threads (kernels::run_list_par), so a chunk smaller
    // than the PC's thread count leaves threads idle (16 x 0.5 s items on 8 threads took 8 s as 16 chunks of 1). Floor it at
    // the thread count, but never above this PC's share of what is left, so several PCs still split a small job.
    let min = if j.kind == KIND_LIST { (st.workers[&wid].threads as u64).min(remaining.div_ceil(nodes.max(1) as u64)).max(1) } else { 1_000 };
    let len = sched::chunk_len(j.policy, j.chunk, j.total, remaining, node_rate, fleet_rate, nodes, min, crate::tune::n("max_chunk")).max(1);
    let j = st.jobs.get_mut(&id)?;
    let (mut a, mut b) = next_chunk(&mut j.requeue, &mut j.next, j.end, len)?;
    if !j.blobs.is_empty() && chunk_missing(&st.workers[&wid], j, a, b) {
        // prefer a chunk whose blobs this PC already holds; else reserve this one and let the head stage its blobs
        if let Some((s, e)) = cached_chunk(&st.workers[&wid], j, len) {
            j.requeue.push((a, b));
            cut(j, s, e);
            (a, b) = (s, e);
        }
        if chunk_missing(&st.workers[&wid], j, a, b) {
            let w = st.workers.get_mut(&wid)?;
            w.staged = Some((id, a, b));
            w.inflight.push((id, a, b));
            return None;
        }
    }
    let j = st.jobs.get_mut(&id)?;
    if !j.has_work() {
        // nothing unassigned is left: from now on idle PCs are in the tail
        let t = j.started.elapsed().as_secs_f64();
        let p = paused_now_of(st.paused_s, st.pause_since);
        st.acct.entry(id).or_insert_with(|| crate::research::Acct { paused0: p, ..Default::default() }).tail_at.get_or_insert(t);
    }
    let j = st.jobs.get(&id)?;
    Some((j.id, a, b, j.kind, j.arg.clone()))
}

/// SCH-08: an idle node with nothing left to take gets a second copy of an overdue chunk. First RESULT wins (finish()
/// drops the other holders' copies and the later duplicate result) and sends them CANCEL_RANGE (job, a, b), which stops
/// that one chunk only; the job-wide CANCEL is sticky for the job id and is never used for this.
fn speculate_take(st: &mut State, wid: u64) -> Option<(u64, u64, u64, u8, Vec<u8>)> {
    if st.halted || st.gated {
        return None;
    }
    let w = st.workers.get(&wid)?;
    if !can_work(w) || !w.inflight.is_empty() {
        return None;
    }
    if !st.policy.reserves.is_empty() && sched::reserved(&node_views(st), &st.policy.reserves).contains(&nkey(w)) {
        return None;
    }
    for j in st.jobs.values().filter(|j| !j.has_work() && j.selector.matches(&w.attrs) && (j.max_pcs == 0 || (st.workers.values().filter(|o| o.inflight.iter().any(|t| t.0 == j.id)).count() as u32) < j.max_pcs) && (j.only.is_empty() || j.only.contains(&w.addr)) && quota_ok(st, j, wid)) {
        let mut infl: Vec<sched::Inflight> = vec![];
        for (_, h) in st.workers.iter().filter(|(i, _)| **i != wid) {
            for k in h.inflight.iter().filter(|k| k.0 == j.id) {
                let r = node_rate(st, h, j.kind);
                infl.push(sched::Inflight {
                    job: j.id, a: k.1, b: k.2, copies: st.workers.values().filter(|o| o.inflight.contains(k)).count() as u32,
                    age_secs: h.sent.get(k).map_or(0.0, |t| t.elapsed().as_secs_f64()),
                    expected_secs: if r > 0.0 { (k.2 - k.1) as f64 / r } else { 0.0 },
                });
            }
        }
        if let Some(&(job, a, b)) = sched::speculate(&infl, 1).first() {
            crate::say!("chunk.speculate", "job {job}: speculative copy of {a}..{b} on {} (original still running after {:.1}s)", w.addr,
                infl.iter().find(|i| (i.a, i.b) == (a, b)).map_or(0.0, |i| i.age_secs));
            return Some((job, a, b, j.kind, j.arg.clone()));
        }
    }
    None
}

pub fn event(ctx: &Ctx, kind: &str, actor: &str, detail: &str) {
    match ctx.store.log_event(now_ms(), kind, actor, detail) {
        Ok(e) => ctx.bus.publish("event", &serde_json::to_string(&e).unwrap_or_default()),
        Err(e) => crate::warnl!("event.error", "event {kind}: {e}"),
    }
}

/// Store write + change-log entry "state" (HA-03).
fn set_state(ctx: &Ctx, id: u64, state: JobState, error: Option<&str>) -> Result<JobRow, String> {
    let ms = now_ms();
    let row = ctx.store.set_state(id, state, ms, error)?;
    crate::repl::record(ctx, "state", serde_json::json!({ "job": id, "state": state.as_str(), "error": error, "ms": ms }));
    Ok(row)
}

fn publish_job(ctx: &Ctx, row: &JobRow) {
    ctx.bus.publish("job", &serde_json::to_string(row).unwrap_or_default());
}

/// Finish a job in the store, tell attached clients, drop it. Caller holds the State lock.
fn end_job(ctx: &Ctx, st: &mut State, id: u64, state: JobState, error: Option<&str>, ev: &str, actor: &str, detail: &str) {
    match set_state(ctx, id, state, error) {
        Ok(row) => publish_job(ctx, &row),
        Err(e) => crate::warnl!("job.error", "job {id}: {e}"),
    }
    event(ctx, ev, actor, detail);
    let frame = match state {
        JobState::Done => (DONE, vec![]),
        _ => (ERR, error.unwrap_or(state.as_str()).as_bytes().to_vec()),
    };
    for tx in st.subs.remove(&id).unwrap_or_default() {
        let _ = tx.send(frame.clone());
    }
    if let (Some(j), Some(a)) = (st.jobs.get(&id), st.acct.remove(&id)) {
        let bd = crate::research::breakdown(id, &a, j.started.elapsed().as_secs_f64(), paused_now(st) - a.paused0);
        ctx.store.put_job_stats(id, &bd.to_string());
    }
    st.acct.remove(&id);
    st.jobs.remove(&id);
    st.attempts.retain(|k, _| k.0 != id);
    sync_brief(st);
    ctx.sh.1.notify_all(); // idle nodes pick another job
}

/// Stop a job (cancel or fail). Returns the writers of nodes that had its chunks, to send CANCEL once the lock is released.
fn stop_job(ctx: &Ctx, st: &mut State, id: u64, state: JobState, error: Option<&str>, ev: &str, actor: &str, detail: &str) -> Vec<Arc<Mutex<Out>>> {
    let mut outs = vec![];
    for w in st.workers.values_mut() {
        if w.inflight.iter().any(|t| t.0 == id) {
            outs.extend(w.out.clone());
        }
        w.inflight.retain(|t| t.0 != id);
        w.sent.retain(|k, _| k.0 != id);
    }
    end_job(ctx, st, id, state, error, ev, actor, detail);
    outs
}

fn cancel_outs(outs: Vec<Arc<Mutex<Out>>>, id: u64) {
    for o in outs {
        let _ = o.lock().unwrap().put(CANCEL, &id.to_le_bytes());
    }
}

/// One run of a chunk failed or timed out: requeue it or fail the job (JOB-06). Caller holds the lock.
fn chunk_failed(ctx: &Ctx, st: &mut State, job: u64, a: u64, b: u64, retry: bool, msg: &str) -> Vec<Arc<Mutex<Out>>> {
    if !st.jobs.contains_key(&job) {
        return vec![]; // stale
    }
    match attempt(&mut st.attempts, (job, a, b), retry) {
        Verdict::Requeue => {
            crate::say!("chunk.requeue", "job {job}: chunk {a}..{b} failed ({msg}), running it again");
            st.jobs.get_mut(&job).unwrap().requeue.push((a, b));
            ctx.sh.1.notify_all();
            vec![]
        }
        Verdict::Fail => {
            let m = format!("chunk {a}..{b} failed: {msg}");
            crate::say!("chunk.failed", "job {job}: {m}");
            stop_job(ctx, st, job, JobState::Failed, Some(&m), "job.failed", "head", &format!("job {job}: {m}"))
        }
    }
}

fn failed(ctx: &Ctx, wid: u64, p: &[u8]) -> io::Result<()> {
    let (job, a, b) = (u64_at(p, 0)?, u64_at(p, 8)?, u64_at(p, 16)?);
    let (retry, msg) = (p.get(24) == Some(&1), String::from_utf8_lossy(p.get(25..).unwrap_or(&[])).into_owned());
    let mut st = ctx.sh.0.lock().unwrap();
    if let Some(w) = st.workers.get_mut(&wid) {
        w.back += 1;
        w.inflight.retain(|&t| t != (job, a, b));
        w.sent.remove(&(job, a, b));
    }
    settle_drain(ctx, &mut st, wid);
    ctx.sh.1.notify_all(); // a node with nothing in flight may switch job
    let outs = if held(&st, (job, a, b)) { vec![] } else { chunk_failed(ctx, &mut st, job, a, b, retry, &msg) }; // a copy of it still runs elsewhere
    drop(st);
    cancel_outs(outs, job);
    Ok(())
}

/// Every 5 s: a chunk in flight longer than task_limit goes back to the queue (a stuck or very slow PC).
/// ponytail: the slow PC keeps its copy; if it finishes later the duplicate result is dropped.
fn watchdog(ctx: Arc<Ctx>) {
    let mut acks = 0;
    loop {
        thread::sleep(Duration::from_secs(5));
        if ACKS.load(Relaxed) != acks {
            acks = ACKS.load(Relaxed);
            println!("ACKs sent to agents: {acks}");
        }
        // safety net: a state changed in the store without a node_state_changed call still reaches the dispatcher
        let seen: Vec<(String, String)> = ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| w.hello.as_ref().map(|h| (h.node_id.clone(), w.state.clone()))).collect();
        for (id, state) in seen {
            if ctx.store.node(&id).is_some_and(|n| n.state != state) {
                node_state_changed(&ctx, &id);
            }
        }
        refresh(&ctx); // labels, pools, caps, reserves, live cpu: re-read every 5 s as a safety net
        let mut st = ctx.sh.0.lock().unwrap();
        let now = Instant::now();
        let mut late = vec![];
        for (wid, w) in st.workers.iter_mut() {
            w.sent.retain(|k, _| w.inflight.contains(k));
            late.extend(w.sent.iter().filter(|(k, t)| now.duration_since(**t) > task_limit(w.rate, k.2 - k.1)).map(|(k, _)| (*wid, *k)));
        }
        let mut outs = vec![];
        for (wid, k) in late {
            let w = st.workers.get_mut(&wid).unwrap();
            w.inflight.retain(|t| *t != k);
            w.sent.remove(&k);
            let who = w.addr.clone();
            event(&ctx, "task.timeout", "head", &format!("job {} chunk {}..{} on {who} took too long", k.0, k.1, k.2));
            if !held(&st, k) {
                outs.push((k.0, chunk_failed(&ctx, &mut st, k.0, k.1, k.2, true, "timed out")));
            }
            settle_drain(&ctx, &mut st, wid);
        }
        drop(st);
        for (job, o) in outs {
            cancel_outs(o, job);
        }
    }
}

fn complete(ctx: &Ctx, st: &mut State, id: u64, todo: u64, secs: f64) {
    let msg = format!("job {id}: {todo} numbers in {secs:.3}s ({:.1} M/s)", todo as f64 / secs.max(1e-9) / 1e6);
    crate::say!("job.complete", "{msg}");
    end_job(ctx, st, id, JobState::Done, None, "job.done", "head", &msg);
    st.last = Some(msg);
}

/// Accept a stored job: compute its remaining ranges and make it runnable. Caller holds the State lock.
fn start_job(ctx: &Ctx, st: &mut State, id: u64) {
    let (Some(spec), Some(row)) = (ctx.store.spec(id), ctx.store.job(id)) else {
        end_job(ctx, st, id, JobState::Failed, Some("cannot load the job"), "job.failed", "head", &format!("job {id}: cannot load the job"));
        return;
    };
    if row.state.finished() || st.jobs.contains_key(&id) {
        return;
    }
    let selector = match parse_sel(&spec.selector) {
        Ok(s) => s,
        Err(e) => {
            let m = format!("bad selector: {e}");
            end_job(ctx, st, id, JobState::Failed, Some(&m), "job.failed", "head", &format!("job {id}: {m}"));
            return;
        }
    };
    let total = spec.end.saturating_sub(spec.from);
    let todo = gaps(spec.from, spec.end, &ctx.store.done_ranges(id));
    let left: u64 = todo.iter().map(|g| g.1 - g.0).sum();
    let pcs = st.workers.values().filter(|w| takes_tasks(&w.state) && (spec.only.is_empty() || spec.only.contains(&w.addr)) && selector.matches(&w.attrs)).count();
    // ~16 chunks per PC for load balancing, capped so cancel / the next job waits under a second.
    // ponytail: after a restart no worker has reconnected yet, so assume 4 PCs.
    let lo = if spec.kind == KIND_LIST { 1 } else { 10_000 }; // list items are few and slow: no 10,000 floor
    let chunk = if spec.chunk > 0 { spec.chunk } else { (total / (pcs.max(if st.workers.is_empty() { 4 } else { 1 }) as u64 * 16)).clamp(lo, 20_000_000) };
    let policy = sched::parse_policy(&spec.policy).unwrap_or(sched::ChunkPolicy::Adaptive);
    if row.state != JobState::Running {
        match set_state(ctx, id, JobState::Running, None) {
            Ok(r) => publish_job(ctx, &r),
            Err(e) => crate::warnl!("job.error", "job {id}: {e}"),
        }
    }
    st.next_id = st.next_id.max(id);
    crate::say!("job.started", "job {id}: {total} numbers ({left} to do), chunk {chunk}, {pcs} matching workers, priority {}, selector '{}', policy {policy:?}", spec.priority, spec.selector);
    if left == 0 {
        complete(ctx, st, id, 0, 0.0);
        return;
    }
    let (mut shared, mut shards, mut item_sha) = (spec.blobs.clone(), vec![], vec![]);
    if spec.kind == KIND_LIST && !spec.blobs.is_empty() {
        let mut idx: HashMap<String, u32> = HashMap::new();
        for it in ctx.store.items(id, 0, spec.end) {
            item_sha.push(crate::blobs::item_blob(&it).map_or(0, |sha| {
                let n = idx.len() as u32;
                *idx.entry(sha.to_string()).or_insert_with(|| {
                    shards.push(sha.to_string());
                    n + 1
                })
            }));
        }
        shared.retain(|b| !idx.contains_key(b));
    }
    st.jobs.insert(id, Job {
        id, kind: spec.kind, arg: spec.arg, next: spec.end, end: spec.end, chunk, total, left, todo: left,
        requeue: todo.into_iter().rev().collect(), only: spec.only, started: Instant::now(), selector, priority: spec.priority, user: spec.submitter.clone(), policy,
        max_pcs: spec.max_pcs, placement: spec.placement, mem_mb: spec.mem_mb, blobs: spec.blobs, blob_mode: spec.blob_mode, shared, shards, item_sha,
    });
    let p = paused_now(st);
    st.acct.insert(id, crate::research::Acct { paused0: p, ..Default::default() });
    sync_brief(st);
    ctx.sh.1.notify_all();
}

/// Publish a node (NodeRow + "online") on the bus.
fn publish_node(ctx: &Ctx, st: &State, node_id: &str) {
    let Some(n) = ctx.store.node(node_id) else { return };
    let mut v = serde_json::to_value(&n).unwrap_or_default();
    let w = st.workers.values().find(|w| w.hello.as_ref().is_some_and(|h| h.node_id == node_id));
    v["online"] = w.is_some().into();
    v["presence"] = w.map_or("idle", |w| if w.presence == "busy" { "busy" } else { "idle" }).into();
    if let Some(i) = v["inventory"].as_str().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        v["inventory"] = i;
    }
    ctx.bus.publish("node", &v.to_string());
}

fn cordon(ctx: &Ctx, node_id: &str) {
    match ctx.store.set_node_state(node_id, "cordoned", "drain finished", "head", now_ms()) {
        Ok(_) => event(ctx, "node.state", "head", &format!("node {node_id} drained, now cordoned")),
        Err(e) => crate::warnl!("node.error", "node {node_id}: {e}"),
    }
}

/// A draining worker with nothing in flight becomes cordoned. Caller holds the lock.
fn settle_drain(ctx: &Ctx, st: &mut State, wid: u64) {
    let Some(w) = st.workers.get_mut(&wid) else { return };
    let Some(id) = w.hello.as_ref().map(|h| h.node_id.clone()) else { return };
    if w.state != "draining" || !w.inflight.is_empty() {
        return;
    }
    w.state = "cordoned".into();
    cordon(ctx, &id);
    publish_node(ctx, st, &id);
}

/// api.rs calls this after it changed a node's state in the store: refresh the connected worker, finish drains, wake the dispatcher.
pub fn node_state_changed(ctx: &Ctx, node_id: &str) {
    let Some(n) = ctx.store.node(node_id) else { return };
    let mut st = ctx.sh.0.lock().unwrap();
    let wids: Vec<u64> = st.workers.iter().filter(|(_, w)| w.hello.as_ref().is_some_and(|h| h.node_id == node_id)).map(|(i, _)| *i).collect();
    for wid in &wids {
        st.workers.get_mut(wid).unwrap().state = n.state.clone();
        settle_drain(ctx, &mut st, *wid);
    }
    if wids.is_empty() && n.state == "draining" {
        cordon(ctx, node_id); // offline: nothing in flight
    }
    publish_node(ctx, &st, node_id);
    ctx.sh.1.notify_all();
    drop(st);
    refresh(ctx);
}

/// refresh() for connect/disconnect: at most one per 500 ms, run off the caller's thread. A refresh is O(workers) under
/// the State lock (0.3-1.7 s at 1,000 nodes), so one per join starved the heartbeats and made agents drop and rejoin.
fn refresh_soon(ctx: &Arc<Ctx>) {
    static PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if PENDING.swap(true, Relaxed) {
        return;
    }
    let c = ctx.clone();
    let spawned = thread::Builder::new().stack_size(2 << 20).spawn(move || {
        thread::sleep(Duration::from_millis(500));
        PENDING.store(false, Relaxed);
        refresh(&c);
    });
    if spawned.is_err() {
        PENDING.store(false, Relaxed); // out of threads: the next call (or the watchdog's 5 s refresh) tries again; do not panic the caller
    }
}

/// Rebuild every node's selector attributes, the parsed caps and reserves; send CTRL to agents whose effective cap
/// changed; log reserve changes; wake the dispatcher. Call after labels, pools, caps, reserves or node states change
/// (api.rs), on connect/disconnect, and every 5 s from the watchdog. Never per chunk.
/// ponytail: re-reads the whole store each time; fine for hundreds of nodes.
pub fn refresh(ctx: &Ctx) {
    let rows: HashMap<String, crate::jobs::NodeRow> = ctx.store.nodes().into_iter().map(|n| (n.node_id.clone(), n)).collect();
    let (labels, pools) = (ctx.store.labels(), ctx.store.pools());
    let labs: HashMap<u64, String> = ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect();
    let mut policy = Policy::default();
    for r in ctx.store.reserves() {
        match parse_sel(&r.selector) {
            Ok(selector) => policy.reserves.push(ReserveView { selector, keep_n: r.keep_n, keep_pct: r.keep_pct }),
            Err(e) => eprintln!("reserve {}: {e}", r.id),
        }
    }
    for c in ctx.store.caps() {
        match parse_sel(&c.selector) {
            Ok(selector) => policy.caps.push(CapView { selector, max_threads: c.max_threads }),
            Err(e) => eprintln!("cap {}: {e}", c.id),
        }
    }
    // ORG-07/08: time rules, re-evaluated on every refresh (the watchdog calls it every 5 s)
    let (mut wins, mut resv) = (vec![], vec![]);
    for w in ctx.store.windows() {
        let (Ok(selector), Some(days), Some(start_min), Some(end_min)) = (parse_sel(&w.selector), sched::parse_days(&w.days), sched::parse_hhmm(&w.start), sched::parse_hhmm(&w.end)) else {
            eprintln!("window {}: unreadable, ignored", w.id);
            continue;
        };
        let block = w.action == "block";
        wins.push(sched::WindowView { selector, days, start_min, end_min, block, cap: if block { None } else { w.cap_threads } });
    }
    let t = now_ms();
    for r in ctx.store.reservations(Some(t)) {
        match parse_sel(&r.selector) {
            Ok(selector) => resv.push(sched::ReservationView { selector, start_ms: r.start_ms, end_ms: r.end_ms }),
            Err(e) => eprintln!("reservation {}: {e}", r.id),
        }
    }
    let tz = std::env::var("CK_TZ_OFFSET_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(330);
    let clock = sched::clock(t, tz);
    let mut ctrl = vec![];
    let changed;
    let mut blocked_now = None;
    {
        let mut st = ctx.sh.0.lock().unwrap();
        for w in st.workers.values_mut() {
            let (node_id, host) = w.hello.as_ref().map_or((w.addr.clone(), w.addr.clone()), |h| (h.node_id.clone(), h.host.clone()));
            let mut row = rows.get(&node_id).cloned().unwrap_or_else(|| crate::jobs::NodeRow {
                node_id: node_id.clone(), host, addr: w.addr.clone(),
                inventory: w.hello.as_ref().map(|h| serde_json::to_string(&h.inv).unwrap_or_default()).unwrap_or_default(), ..Default::default()
            });
            row.state = w.state.clone();
            let lb: Vec<(String, String)> = labels.iter().filter(|l| l.0 == node_id).map(|l| (l.1.clone(), l.2.clone())).collect();
            let pl: Vec<String> = pools.iter().filter(|p| p.members.contains(&node_id)).map(|p| p.name.clone()).collect();
            let lab = row.lab_id.and_then(|i| labs.get(&i)).cloned();
            w.attrs = select::attrs(&row, &lb, &pl, lab.as_deref(), Some((w.threads, w.cpu)));
        }
        let mut wincap: HashMap<u64, Option<u32>> = HashMap::new();
        for (wid, w) in st.workers.iter_mut() {
            let (b, c) = sched::time_rule(&w.attrs, clock, &wins, &resv);
            w.blocked = b;
            wincap.insert(*wid, c);
            w.calib = rows.get(&nkey(w)).and_then(|n| n.calib_mhs).unwrap_or(w.calib);
        }
        let nb = st.workers.values().filter(|w| w.blocked).count();
        if nb != st.blocked {
            blocked_now = Some((nb, st.blocked));
            st.blocked = nb;
        }
        st.policy = policy;
        let views = node_views(&st);
        let now_reserved = sched::reserved(&views, &st.policy.reserves);
        changed = (now_reserved != st.reserved).then(|| (now_reserved.clone(), std::mem::replace(&mut st.reserved, now_reserved)));
        let caps = std::mem::take(&mut st.policy.caps);
        for (wid, w) in st.workers.iter_mut() {
            let want = match (sched::thread_cap(&w.attrs, &caps), wincap.get(wid).copied().flatten()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let (Some(frame), Some(o)) = (cap_ctrl(w.cap, want), w.out.clone()) {
                ctrl.push((o, frame));
                w.cap = want;
            }
        }
        st.policy.caps = caps;
        ctx.sh.1.notify_all();
    }
    for (o, frame) in ctrl {
        let _ = o.lock().unwrap().put(CTRL, &frame);
    }
    if let Some((now, was)) = blocked_now {
        event(ctx, "time.rule", "head", &format!("{now} node(s) take no new chunks because of time windows or reservations (was {was})"));
    }
    if let Some((now, was)) = changed {
        event(ctx, "reserve.changed", "head", &format!("{} node(s) held free by reserves (was {})", now.len(), was.len()));
    }
    emit_config(ctx);
    gate_tick(ctx);
}

/// HA-03: a "config" log entry (org, users, tokens, node keys, membership). Built at most once a second, logged only
/// when it differs from the last one. api.rs reaches this through refresh().
fn emit_config(ctx: &Ctx) {
    {
        let mut st = ctx.sh.0.lock().unwrap();
        if st.cfg.is_some_and(|c| c.1.elapsed() < Duration::from_secs(1)) {
            return;
        }
        st.cfg = Some((st.cfg.map_or(0, |c| c.0), Instant::now()));
    }
    let v = crate::repl::config_entry(ctx);
    let h = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.to_string().hash(&mut h);
        h.finish()
    };
    let mut st = ctx.sh.0.lock().unwrap();
    if st.cfg.is_some_and(|c| c.0 == h) {
        return;
    }
    st.cfg = Some((h, Instant::now()));
    drop(st);
    crate::repl::record(ctx, "config", v);
}

/// HA-05: decide whether this head may dispatch. repl.rs owns role (candidate/primary), membership and connected
/// counts; this adds the 10 s grace of a primary with standbys. Called from refresh() and once a second (no State lock held).
fn gate_tick(ctx: &Ctx) {
    let may = crate::repl::may_dispatch(ctx); // refreshes repl state first
    let (role, standbys) = {
        let s = ctx.repl.status.lock().unwrap();
        (s.role.clone(), s.standbys.len())
    };
    let mut st = ctx.sh.0.lock().unwrap();
    if may {
        st.noq_since = None;
    } else {
        st.noq_since.get_or_insert_with(Instant::now);
    }
    let ok = dispatch_ok(&role, may, standbys, st.noq_since.map_or(Duration::ZERO, |t| t.elapsed()));
    let was_gated = std::mem::replace(&mut st.gated, !ok);
    let dispatching = ok && !st.halted;
    match (dispatching, st.pause_since) {
        (false, None) => st.pause_since = Some(Instant::now()),
        (true, Some(t)) => (st.paused_s, st.pause_since) = (st.paused_s + t.elapsed().as_secs_f64(), None),
        _ => {}
    }
    if ok && was_gated {
        crate::say!("ha.quorum", "quorum back, dispatching");
        st.noq_since = None;
        ctx.sh.1.notify_all();
    } else if !ok && !was_gated {
        crate::say!("ha.quorum", "! no quorum, dispatching paused");
    }
    drop(st);
    ctx.repl.status.lock().unwrap().dispatching = dispatching;
}

/// HA-06: a head with a higher epoch exists. Stop dispatching and drop every agent.
fn halt(ctx: &Ctx) {
    let mut st = ctx.sh.0.lock().unwrap();
    st.halted = true;
    st.pause_since.get_or_insert_with(Instant::now);
    for w in st.workers.values() {
        if let Some(o) = &w.out {
            let _ = o.lock().unwrap().s.0.shutdown(std::net::Shutdown::Both);
        }
    }
    drop(st);
    let mut s = ctx.repl.status.lock().unwrap();
    (s.dispatching, s.role) = (false, "standby".into());
}

/// Epoch of a beacon "ck/1 <name> <ip>:<port> e=<epoch>" from a head of cluster `name` other than `own` (ip:port) that
/// is above `mine`. Pure.
fn higher_epoch(text: &str, name: &str, own: &str, mine: u64) -> Option<u64> {
    let rest = text.strip_prefix("ck/1 ")?.trim();
    let rest = rest.strip_suffix(" relay").unwrap_or(rest).trim_end();
    let (rest, e) = rest.rsplit_once(" e=")?;
    let (n, addr) = rest.rsplit_once(' ')?;
    let e: u64 = e.trim().parse().ok()?;
    (n.trim() == name && addr != own && e > mine).then_some(e)
}

/// Listen for beacons; a higher epoch than ours fences this head: stop, close agents, restart as its standby.
fn watch_epoch(ctx: Arc<Ctx>, name: String) {
    let s = match UdpSocket::bind(("0.0.0.0", disc_port())) {
        Ok(s) => s,
        Err(e) => return eprintln!("UDP {}: {e}; a higher epoch will not be noticed", disc_port()),
    };
    let mut b = [0u8; 512];
    while let Ok((n, _)) = s.recv_from(&mut b) {
        let Ok(t) = std::str::from_utf8(&b[..n]) else { continue };
        let own = local_ip().map_or(String::new(), |i| format!("{i}:{}", port()));
        // only a beacon signed with the cluster's fence key can fence this head (a plain "ck/1" one is forgeable)
        let signed = crate::repl::fence_key(&ctx.store).and_then(|k| crate::repl::fence_check(&k, t)).map(|(n, a, e)| format!("ck/1 {n} {a} e={e}"));
        if let Some(e) = signed.and_then(|t| higher_epoch(&t, &name, &own, crate::repl::epoch(&ctx))) {
            halt(&ctx);
            let addr = t.split_whitespace().nth(2).unwrap_or("").to_string();
            crate::say!("ha.fence", "! FENCED: the head at {addr} has epoch {e} (mine is {}). Dispatching stopped, agents closed; restarting as its standby", crate::repl::epoch(&ctx));
            thread::sleep(Duration::from_millis(300));
            rejoin_as_standby(&addr);
        }
    }
}

/// HA-07 planned switchover: the standby `node` takes over and this head becomes its standby. Pauses dispatch first,
/// so in-flight results are either already replicated or resent by the agents (they keep them until ACK).
pub fn switch_to(ctx: Arc<Ctx>, node: &str, actor: &str) -> Result<String, String> {
    ctx.sh.0.lock().unwrap().halted = true; // no new chunks while the standby catches up
    match crate::repl::hand_over(&ctx, node) {
        Ok(peer) => {
            event(&ctx, "ha.switchover", actor, &format!("head hands over to {node} at {peer}"));
            let to = format!("{peer}:{}", port());
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(200)); // let the HTTP reply go out
                halt(&ctx);
                crate::say!("ha.switchover", "handing over to {to}; restarting as its standby");
                rejoin_as_standby(&to);
            });
            Ok(peer)
        }
        Err(e) => {
            ctx.sh.0.lock().unwrap().halted = false;
            ctx.sh.1.notify_all();
            Err(e)
        }
    }
}

/// HA-06: an old primary becomes a standby of the new head. Re-runs this binary with the same arguments, minus any
/// `--standby` / `--head X`, plus `--standby --head <ip>` (port via CK_PRIMARY_PORT). Exits FENCED_EXIT if that fails.
fn rejoin_as_standby(addr: &str) -> ! {
    let (ip, p) = addr.rsplit_once(':').unwrap_or((addr, ""));
    let mut args: Vec<String> = vec![];
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--standby" => {}
            "--head" => drop(it.next()),
            _ => args.push(a),
        }
    }
    args.extend(["--standby".into(), "--head".into(), ip.to_string()]);
    let mut c = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "ck".into()));
    c.args(&args).env("CK_PRIMARY_PORT", p);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        eprintln!("could not restart as a standby: {}", c.exec());
    }
    #[cfg(not(unix))]
    if c.spawn().is_ok() {
        std::process::exit(0);
    }
    std::process::exit(FENCED_EXIT)
}

fn deny(s: &mut TcpStream, ctx: &Ctx, h: &Hello, addr: &str, why: &str) -> io::Result<()> {
    event(ctx, "node.denied", "head", &format!("{} at {addr} (node {}): {why}", h.host, h.node_id));
    crate::say!("admission.denied", "! denied {addr} node {}: {why}", h.node_id);
    send(s, DENIED, why.as_bytes())
}

/// Secured fleet: returns the node key when the PC is admitted (after CHALLENGE/PROOF or a join code), else the reason.
fn admit(s: &mut TcpStream, ctx: &Ctx, h: &Hello, addr: &str) -> io::Result<Result<Vec<u8>, String>> {
    match plan(ctx.store.node_key(&h.node_id), h.join_code.as_deref()) {
        Plan::Deny(r) => Ok(Err(r.into())),
        Plan::Challenge(key) => {
            let n = auth::nonce();
            send(s, CHALLENGE, &n)?;
            Ok(match recv(s)? {
                (PROOF, p) if auth::verify(&key, &n, &h.node_id, &p) => Ok(key),
                _ => Err("wrong key".into()),
            })
        }
        Plan::Join(hash) => {
            let Some(auto) = ctx.store.use_join_code(&hash, now_ms()) else { return Ok(Err("join code invalid or expired".into())) };
            let key = auth::nonce().to_vec();
            let inv = serde_json::to_string(&h.inv).unwrap_or_default();
            let st = if auto { "active" } else { "pending" };
            let r = ctx.store.upsert_node(&h.node_id, &h.host, addr, &inv, now_ms())
                .and_then(|_| ctx.store.set_node_key(&h.node_id, &key))
                .and_then(|_| ctx.store.set_node_state(&h.node_id, st, "joined with a join code", "head", now_ms()));
            if let Err(e) = r {
                return Ok(Err(format!("head database: {e}")));
            }
            send(s, KEY, &key)?;
            event(ctx, "node.admitted", "head", &format!("{} at {addr} (node {}) joined, {st}", h.host, h.node_id));
            Ok(Ok(key))
        }
    }
}

/// Handle a RESULT; true = give the agent a REQ credit (it was sent a TASK for this; a result resent after a failover was not).
fn finish_credit(ctx: &Ctx, wid: u64, p: &[u8]) -> io::Result<bool> {
    let (sends, credit) = finish_inner(ctx, wid, p)?;
    flush(sends);
    Ok(credit)
}

/// Numbers of [a,b) inside `done` (sorted, merged). Pure.
fn covered(a: u64, b: u64, done: &[(u64, u64)]) -> u64 {
    done.iter().map(|&(s, e)| e.min(b).saturating_sub(s.max(a))).sum()
}

/// `r` without `cut`: zero, one or two ranges. Pure.
fn subtract(r: (u64, u64), cut: (u64, u64)) -> Vec<(u64, u64)> {
    let ((a, b), (c, d)) = (r, cut);
    if d <= a || c >= b {
        return vec![r];
    }
    let mut v = vec![];
    if c > a {
        v.push((a, c));
    }
    if d < b {
        v.push((d, b));
    }
    v
}

/// Put `pieces` (ascending) back in the job's queue, minus what is queued or in flight already.
fn requeue_missing(st: &mut State, job: u64, mut pieces: Vec<(u64, u64)>) {
    let Some(j) = st.jobs.get(&job) else { return };
    let cuts: Vec<(u64, u64)> = st.workers.values().flat_map(|w| w.inflight.iter().filter(|t| t.0 == job).map(|t| (t.1, t.2))).chain(j.requeue.iter().copied()).collect();
    for c in cuts {
        pieces = pieces.into_iter().flat_map(|p| subtract(p, c)).collect();
    }
    let q = &mut st.jobs.get_mut(&job).unwrap().requeue;
    q.extend(pieces);
    q.sort_by(|x, y| y.cmp(x));
}

/// Handle a RESULT; returns the ACK / CANCEL_RANGE frames to send after the lock is released.
fn finish_inner(ctx: &Ctx, wid: u64, p: &[u8]) -> io::Result<(Sends, bool)> {
    let sh = &ctx.sh;
    let (job, a, b) = (u64_at(p, 0)?, u64_at(p, 8)?, u64_at(p, 16)?);
    let (cpu, mem_used, mem_total) = (u64_at(p, 24)?, u64_at(p, 32)?, u64_at(p, 40)?);
    let secs = u64_at(p, 48)? as f64 / 1e6; // compute time measured by the worker (excludes queueing)
    let t1 = now();
    let t0 = t1 - secs;
    crate::lgc!("chunk.done", {"job": job, "a": a, "b": b, "wid": wid, "ms": secs * 1000.0});
    let files = FILES.lock().unwrap().remove(&(job, a, b));
    let mut st = sh.0.lock().unwrap();
    let kind = st.jobs.get(&job).map(|j| j.kind);
    let mut upd = None;
    let mut credit = true;
    if let Some(w) = st.workers.get_mut(&wid) {
        w.back += 1;
        credit = w.back <= w.fed;
        w.chunks += 1;
        w.numbers += b - a;
        w.secs += secs;
        w.rate = (b - a) as f64 / secs.max(1e-9);
        upd = kind.map(|k| ((nkey(w), k), w.rate));
        w.inflight.retain(|&t| t != (job, a, b));
        w.sent.remove(&(job, a, b));
        (w.cpu, w.mem_used, w.mem_total) = (cpu as f64 / 10.0, mem_used, mem_total);
    }
    if let Some((key, x)) = upd {
        let r = st.rates.entry(key).or_insert(0.0);
        *r = ewma(*r, x);
    }
    settle_drain(ctx, &mut st, wid);
    if st.workers.get(&wid).is_some_and(|w| w.inflight.is_empty()) {
        sh.1.notify_all(); // idle node: may switch job
    }
    while st.events.len() >= crate::tune::n("max_events") as usize {
        st.events.pop_front();
    }
    st.events.push_back((wid, t0, t1));
    let ack_now = |st: &State| -> Sends { st.workers.get(&wid).and_then(|w| w.out.clone()).map(|o| (o, ACK, pack(&[job, a, b]))).into_iter().collect() };
    if !st.jobs.contains_key(&job) {
        return Ok((ack_now(&st), credit)); // stale (cancelled or finished job): the agent may drop its copy
    }
    // After a head restart gaps are re-chunked differently, so a resent RESULT can overlap parts already stored.
    let done = ctx.store.done_in(job, a, b);
    let cov = covered(a, b, &done);
    if cov == b - a {
        acct_waste(&mut st, job, wid, secs, None);
        return Ok((ack_now(&st), credit)); // everything in it is stored already
    }
    if cov > 0 {
        // partial overlap: storing it would count numbers twice. Drop it, ACK it, make sure its uncovered pieces get run.
        crate::say!("chunk.overlap", "job {job}: result {a}..{b} overlaps stored parts, dropped; its gaps are queued");
        acct_waste(&mut st, job, wid, secs, None);
        requeue_missing(&mut st, job, gaps(a, b, &done));
        sh.1.notify_all();
        return Ok((ack_now(&st), credit));
    }
    // ponytail: the store write happens under the State lock so ATTACH sees parts and live frames without a gap.
    if !ctx.store.add_part(job, a, b, &p[RESULT_HDR..]).unwrap_or_else(|e| { crate::warnl!("job.error", "job {job}: {e}"); true }) {
        acct_waste(&mut st, job, wid, secs, None);
        return Ok((ack_now(&st), credit)); // already stored (re-run, speculative copy, or an agent resending after a failover, HA-04): ACK at once
    }
    acct_kept(&mut st, job, wid, b - a, secs);
    if let Some(f) = &files {
        ctx.store.put_job_files(job, a, b, f);
    }
    // HA-03/04: log the part; ACK when the standbys have it too. repl::record / durable_seq are called under the State
    // lock (order State -> repl), so repl must call durable_upto without holding its own locks.
    let seq = crate::repl::record_with(ctx, "part", || serde_json::json!({ "job": job, "a": a, "b": b, "body": &p[RESULT_HDR..], "files": files.as_ref().map(|f| f.iter().map(|(n, s, z)| serde_json::json!({"name": n, "sha": s, "size": z})).collect::<Vec<_>>()) }));
    let mut sends: Sends = vec![];
    if seq <= crate::repl::durable_seq(ctx) {
        sends.extend(ack_now(&st));
    } else if let Some(w) = st.workers.get_mut(&wid) {
        w.pending.push((seq, job, a, b));
    }
    let mut wasted = vec![];
    for (lid, w) in st.workers.iter_mut() {
        if w.inflight.contains(&(job, a, b)) {
            w.inflight.retain(|&t| t != (job, a, b)); // lost the race: its result will be dropped as a duplicate
            wasted.extend(w.sent.remove(&(job, a, b)).map(|t| (*lid, t.elapsed().as_secs_f64())));
            sends.extend(w.out.clone().map(|o| (o, CANCEL_RANGE, pack(&[job, a, b])))); // stop just this chunk on the loser
            crate::say!("chunk.speculate_won", "job {job}: chunk {a}..{b} won by worker {wid}, the copy on {} is dropped", w.addr);
        }
    }
    // chunks that overlap the new part without being identical: stop them, queue what is still uncovered
    let mut lost = vec![];
    for (lid, w) in st.workers.iter_mut() {
        let ov: Vec<_> = w.inflight.iter().filter(|t| t.0 == job && t.1 < b && a < t.2).copied().collect();
        for t in ov {
            w.inflight.retain(|x| *x != t);
            wasted.extend(w.sent.remove(&t).map(|s| (*lid, s.elapsed().as_secs_f64())));
            sends.extend(w.out.clone().map(|o| (o, CANCEL_RANGE, pack(&[t.0, t.1, t.2]))));
            lost.push((t.1, t.2));
        }
    }
    for (lid, s) in wasted {
        acct_waste(&mut st, job, lid, s, None);
    }
    if let Some(j) = st.jobs.get_mut(&job) {
        j.requeue = j.requeue.drain(..).flat_map(|r| subtract(r, (a, b))).collect();
        j.requeue.sort_by(|x, y| y.cmp(x)); // lowest last
    }
    for (x, y) in lost {
        if !held(&st, (job, x, y)) {
            requeue_missing(&mut st, job, gaps(x, y, &ctx.store.done_in(job, x, y)));
        }
    }
    sh.1.notify_all();
    let mut part = pack(&[a, b]);
    part.extend_from_slice(&p[RESULT_HDR..]);
    if let Some(v) = st.subs.get_mut(&job) {
        v.retain(|tx| tx.send((PART, part.clone())).is_ok());
    }
    let j = st.jobs.get_mut(&job).unwrap();
    j.left = j.left.saturating_sub(b - a);
    if j.left == 0 {
        let (todo, secs) = (j.todo, j.started.elapsed().as_secs_f64());
        complete(ctx, &mut st, job, todo, secs);
    } else {
        sync_brief(&mut st);
    }
    Ok((sends, credit))
}

fn serve_worker(mut s: TcpStream, ctx: Arc<Ctx>, hello: &[u8], wid: u64) -> io::Result<()> {
    let sh = ctx.sh.clone();
    let (threads, version, hello) = parse_hello(hello);
    let addr = s.peer_addr()?.ip().to_string();
    match &hello {
        Some(h) => crate::say!(
            "node.connect", "+ worker {addr} ({threads} threads) node {} host {}{} v{version} {} {} {} cores {} GB",
            h.node_id, h.host, if h.virtual_node { " (virtual)" } else { "" }, h.inv.os, h.inv.arch, h.inv.cores_logical, h.inv.ram_bytes >> 30
        ),
        None => crate::say!("node.connect", "+ worker {addr} ({threads} threads) v{version}, no hello"),
    }
    // HA-01: a worker silent for LOST_AFTER (pulled cable, frozen process) errors the reader out, which requeues its chunks.
    // ponytail: with --spin the socket is non-blocking and there is no timeout.
    if !SPIN.load(std::sync::atomic::Ordering::Relaxed) {
        s.set_read_timeout(Some(lost_after()))?;
    }
    // Admission (FLT-04, SEC-02): a secured fleet needs a node key or a join code; afterwards every frame is sealed.
    let mut key = None;
    let mut out_mac = None;
    if !fleet_open() {
        let Some(h) = &hello else { return deny(&mut s, &ctx, &Hello::default(), &addr, "agent too old for a secured fleet, upgrade ck") };
        match admit(&mut s, &ctx, h, &addr)? {
            Ok(k) => {
                // sealed HEARTBEAT = "admitted", so the agent can tell a good PROOF from a DENIED
                let mut m = FrameMac::new(&k);
                send_sealed(&mut s, &mut m, HEARTBEAT, &[])?;
                (key, out_mac) = (Some(k), Some(m));
            }
            Err(why) => return deny(&mut s, &ctx, h, &addr, &why),
        }
    }
    let node = hello.as_ref().map(|h| (h.node_id.clone(), h.host.clone()));
    if let Some(h) = &hello {
        let _ = ctx.store.upsert_node(&h.node_id, &h.host, &addr, &serde_json::to_string(&h.inv).unwrap_or_default(), now_ms());
    }
    let stored = node.as_ref().and_then(|n| ctx.store.node(&n.0));
    let (state, write) = join_state(fleet_open(), stored.as_ref().map(|n| n.state.as_str()));
    if let (true, Some(n)) = (write, &node) {
        let _ = ctx.store.set_node_state(&n.0, &state, "open fleet", "head", now_ms());
    }
    let detail = |n: &(String, String)| format!("{} at {addr} (node {})", n.1, n.0); // read by people in the Events page
    if let Some(n) = &node {
        event(&ctx, "node.joined", "head", &detail(n));
    }
    let mut in_mac = key.as_ref().map(|k| FrameMac::new(k));
    sh.0.lock().unwrap().workers.insert(wid, Worker { addr: addr.clone(), threads, version, hello, state, presence: "idle".into(), ..Default::default() });
    if let Some(n) = &node {
        publish_node(&ctx, &sh.0.lock().unwrap(), &n.0);
    }
    if let Some(n) = &node {
        power_arrived(&ctx, &n.0);
    }

    // Reader (this thread) turns each REQ / RESULT into a credit; the feeder thread spends
    // credits on chunks. Two threads so results keep flowing while the feeder waits for work.
    let (mut r, w) = conn(s)?;
    let w = Arc::new(Mutex::new(Out { s: w, mac: out_mac }));
    sh.0.lock().unwrap().workers.get_mut(&wid).unwrap().out = Some(w.clone());
    ctrl_to(&w, tune_msg());
    refresh_soon(&ctx); // attributes and thread cap: coalesced, so a join storm is not O(W^2)
    let (credit, credits) = mpsc::channel();
    let ctx2 = ctx.clone();
    // 512 KiB: feed() -> take() / store.items() / serde_json is shallow; threads are the scarce resource at thousands of workers
    let feeder = thread::Builder::new().stack_size(512 << 10).spawn(move || feed(w, ctx2, wid, credits))?;
    let err = read_results(&mut r, &mut in_mac, &ctx, wid, credit).err();

    let mut st = sh.0.lock().unwrap();
    let gw = st.workers.remove(&wid);
    let gone = gw.as_ref().map(|w| w.inflight.clone()).unwrap_or_default();
    let mut recovered = 0;
    for t in &gone {
        if held(&st, *t) {
            continue; // a speculative copy still runs elsewhere
        }
        if st.jobs.contains_key(&t.0) {
            let spent = gw.as_ref().and_then(|w| w.sent.get(t)).map_or(0.0, |s| s.elapsed().as_secs_f64());
            acct_waste(&mut st, t.0, wid, spent, gw.as_ref().map(who_of));
            st.jobs.get_mut(&t.0).unwrap().requeue.push((t.1, t.2));
            recovered += 1;
        }
    }
    if let (Some(n), true) = (&node, recovered > 0) {
        event(&ctx, "chunk.recovered", "head", &format!("{recovered} chunks of node {} requeued", n.0));
    }
    if let Some(n) = &node {
        publish_node(&ctx, &st, &n.0);
    }
    sh.1.notify_all(); // hands requeued chunks to others and lets our feeder see we are gone
    drop(st);
    refresh_soon(&ctx);
    let _ = r.get_ref().0.shutdown(std::net::Shutdown::Both);
    let _ = feeder.join();
    if let Some(n) = &node {
        event(&ctx, "node.lost", "head", &detail(n));
    }
    crate::say!("node.disconnect", "- worker {addr} ({})", err.map_or("closed".into(), |e| e.to_string()));
    Ok(())
}

/// One thread for all workers (was one per worker). A worker whose Out is busy is mid-send and needs no heartbeat;
/// a dead socket is left to the reader (read timeout / reset), which removes the worker.
/// ponytail: a peer that stops reading while its send buffer is full can stall this thread; add a write timeout then.
fn heartbeat_all(sh: Shared) {
    loop {
        thread::sleep(heartbeat_every());
        let outs: Vec<_> = sh.0.lock().unwrap().workers.values().filter_map(|w| w.out.clone()).collect();
        for o in outs {
            if let Ok(mut g) = o.try_lock() {
                let _ = g.put(HEARTBEAT, &[]);
            }
        }
    }
}

fn read_results(r: &mut impl Read, mac: &mut Option<FrameMac>, ctx: &Ctx, wid: u64, credit: mpsc::Sender<()>) -> io::Result<()> {
    loop {
        let (k, p) = recv_any(r, mac)?;
        match k {
            RESULT => {
                if !finish_credit(ctx, wid, &p)? {
                    continue; // resent after a failover: no TASK was spent on it
                }
            }
            FAILED => failed(ctx, wid, &p)?,
            CTRL => ctrl_in(ctx, wid, &p),
            _ => {}
        }
        if k == REQ || k == RESULT || k == FAILED {
            let _ = credit.send(());
        }
    }
}

/// CTRL from an agent: {"standby_started": true} | {"standby_failed": why} (HA-10), {"presence": "busy"|"idle"} (SCH-10), {"calib_mhs": F} (FLT-06) or {"energy_j": F} (OBS-03).
/// Output files an agent reported for chunks whose RESULT has not arrived yet (JOB-08).
/// ponytail: bounded at 10,000 chunks (cleared when over); a lost RESULT leaves its entry until then.
static FILES: Mutex<BTreeMap<(u64, u64, u64), Vec<(String, String, u64)>>> = Mutex::new(BTreeMap::new());

fn note_files(f: &serde_json::Value) {
    let (Some(j), Some(a), Some(b)) = (f["job"].as_u64(), f["a"].as_u64(), f["b"].as_u64()) else { return };
    let v: Vec<(String, String, u64)> = f["files"].as_array().into_iter().flatten().filter_map(|x| {
        let (n, s) = (x["name"].as_str()?, x["sha"].as_str()?);
        (crate::blobs::valid_sha(s) && !n.is_empty() && n.len() <= 200 && !n.starts_with('.') && !n.contains(['/', '\\'])).then(|| (n.to_string(), s.to_string(), x["size"].as_u64().unwrap_or(0)))
    }).take(1000).collect();
    let mut g = FILES.lock().unwrap();
    if g.len() >= 10_000 {
        g.clear();
    }
    g.insert((j, a, b), v);
}

/// FLT-07: node id -> epoch ms until which the PC shows "identify".
static IDENT: Mutex<BTreeMap<String, i64>> = Mutex::new(BTreeMap::new());

/// Send identify to the connected PCs among `ids`; (identified, offline).
pub fn identify(ctx: &Ctx, ids: &[String], secs: u64) -> (Vec<String>, Vec<String>) {
    let (mut on, mut off, mut outs) = (vec![], vec![], vec![]);
    {
        let st = ctx.sh.0.lock().unwrap();
        for id in ids {
            match st.workers.values().find(|w| w.out.is_some() && w.hello.as_ref().is_some_and(|h| &h.node_id == id)) {
                Some(w) => {
                    outs.push(w.out.clone().unwrap());
                    on.push(id.clone());
                }
                None => off.push(id.clone()),
            }
        }
    }
    for o in &outs {
        ctrl_to(o, serde_json::json!({"identify": secs}));
    }
    let until = now_ms() + secs as i64 * 1000;
    let mut g = IDENT.lock().unwrap();
    g.retain(|_, u| *u > now_ms());
    on.iter().for_each(|id| { g.insert(id.clone(), until); });
    (on, off)
}

/// Epoch ms the PC's identify ends, None when it is not identifying.
pub fn identify_until(node_id: &str) -> Option<i64> {
    IDENT.lock().unwrap().get(node_id).copied().filter(|u| *u > now_ms())
}

fn ctrl_in(ctx: &Ctx, wid: u64, p: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(p) else { return };
    if let Some(f) = v.get("files") {
        note_files(f);
        return;
    }
    let mut st = ctx.sh.0.lock().unwrap();
    if crate::blobs::is_ctrl(&v) {
        crate::blobs::on_ctrl(ctx, &mut st, wid, &v);
    }
    let Some(w) = st.workers.get_mut(&wid) else { return };
    let node = w.hello.as_ref().map(|h| h.node_id.clone());
    if let Some(pr) = v["presence"].as_str().filter(|p| matches!(*p, "busy" | "idle")) {
        w.presence = pr.into();
    }
    if let Some(m) = v["calib_mhs"].as_f64().filter(|m| *m > 0.0) {
        w.calib = m;
        if let Some(id) = &node {
            if let Err(e) = ctx.store.set_node_calib(id, m) {
                eprintln!("node {id}: {e}");
            }
        }
    }
    if let Some(j) = v["energy_j"].as_f64().filter(|_| !w.hello.as_ref().is_some_and(|h| h.virtual_node)) {
        w.energy_j = j;
    }
    if v["standby_started"].as_bool() == Some(true) {
        if let Some(id) = &node {
            event(ctx, "ha.recruit", "head", &format!("node {id} started a standby head"));
        }
    }
    if let Some(why) = v["standby_failed"].as_str() {
        if let Some(id) = &node {
            pw().tried.insert(id.clone(), Instant::now());
            event(ctx, "ha.recruit", "head", &format!("node {id} could not start a standby head: {why}"));
        }
    }
    if let Some(id) = node {
        publish_node(ctx, &st, &id);
    }
    ctx.sh.1.notify_all(); // idle again: chunks flow; busy: the dispatcher re-checks
}

fn feed(w: Arc<Mutex<Out>>, ctx: Arc<Ctx>, wid: u64, credits: mpsc::Receiver<()>) {
    let sh = &ctx.sh;
    while credits.recv().is_ok() {
        let (job, a, b, kind, arg, mem, jb) = {
            let mut st = sh.0.lock().unwrap();
            loop {
                if !st.workers.contains_key(&wid) {
                    return; // disconnected; the reader already requeued our chunks
                }
                if let Some(t) = take(&mut st, wid).or_else(|| speculate_take(&mut st, wid)) {
                    let mem = st.jobs.get(&t.0).map_or(0, |j| j.mem_mb);
                    let blobs = st.jobs.get(&t.0).map(|j| j.blobs.clone()).unwrap_or_default();
                    let wk = st.workers.get_mut(&wid).unwrap();
                    let mem = if mem > 0 && wk.told.insert(t.0) { mem } else { 0 };
                    // cmd and list jobs always get this CTRL once: it carries the job-scoped blob upload token too
                    let jb = ((!blobs.is_empty() || matches!(t.3, KIND_CMD | KIND_LIST)) && wk.told_b.insert(t.0)).then_some(blobs);
                    wk.inflight.push((t.0, t.1, t.2));
                    wk.fed += 1;
                    wk.sent.insert((t.0, t.1, t.2), Instant::now());
                    crate::lgc!("chunk.dispatch", {"job": t.0, "a": t.1, "b": t.2, "wid": wid});
                    break (t.0, t.1, t.2, t.3, t.4, mem, jb);
                }
                // 2 s timeout: an idle node re-checks for an overdue chunk to copy (watchdog also wakes it every 5 s)
                st = sh.1.wait_timeout(st, Duration::from_secs(2)).unwrap().0;
            }
        };
        if mem > 0 {
            ctrl_to(&w, serde_json::json!({"job": {"id": job, "mem_mb": mem}}));
        }
        if let Some(jb) = jb {
            ctrl_to(&w, serde_json::json!({"job_blobs": {"id": job, "blobs": jb, "put": {"port": dash_port(), "token": crate::blobs::put_token(job)}}}));
        }
        let mut p = pack(&[job, a, b]);
        p.push(kind);
        p.extend_from_slice(&arg);
        if kind == KIND_LIST {
            // ponytail: items are read from the store per chunk, not cached; fine for ranges of a few thousand lines
            p.push(0);
            p.extend_from_slice(ctx.store.items(job, a, b).join("\n").as_bytes());
        }
        if w.lock().unwrap().put(TASK, &p).is_err() {
            return; // the reader sees the disconnect and requeues
        }
    }
}

fn job_name(kind: u8, arg: &[u8]) -> String {
    match kind {
        KIND_SHA => format!("sha {}", arg.iter().map(|n| char::from_digit(*n as u32, 16).unwrap_or('?')).collect::<String>()),
        KIND_SYNTH => "synth".into(),
        _ => String::from_utf8_lossy(arg).chars().take(40).collect(),
    }
}

fn client_job(p: &[u8], peer: &str) -> JobSpec {
    let (from, end, chunk) = (u64_at(p, 0).unwrap_or(0), u64_at(p, 8).unwrap_or(0), u64_at(p, 16).unwrap_or(0));
    let kind = *p.get(24).unwrap_or(&KIND_SHA);
    let olen = *p.get(25).unwrap_or(&0) as usize;
    let only = String::from_utf8_lossy(p.get(26..26 + olen).unwrap_or(&[])).split(',').filter(|a| !a.is_empty()).map(String::from).collect();
    let arg = p.get(26 + olen..).unwrap_or(&[]).to_vec();
    JobSpec { name: job_name(kind, &arg), kind, from, end, chunk, arg, only, submitter: format!("cli@{peer}"), ..Default::default() }
}

/// SUBMIT_JSON: a JobSpec as JSON; the head sets the submitter and, if empty, the name.
fn client_job_json(p: &[u8], peer: &str) -> Result<JobSpec, String> {
    let mut spec: JobSpec = serde_json::from_slice(p).map_err(|e| format!("bad job: {e}"))?;
    spec.submitter = format!("cli@{peer}");
    if spec.name.is_empty() {
        spec.name = job_name(spec.kind, &spec.arg);
    }
    Ok(spec)
}

/// A standby head connects with HELLO_S: same admission as an agent, then repl.rs owns the socket (sealed + encrypted frames).
fn serve_standby_conn(mut s: TcpStream, ctx: Arc<Ctx>, p: &[u8]) -> io::Result<()> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad HELLO_S");
    let n = p.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).ok_or_else(bad)?;
    let h: Hello = serde_json::from_slice(p.get(4..4 + n).ok_or_else(bad)?).map_err(|_| bad())?;
    let addr = s.peer_addr()?.ip().to_string();
    let mut sk = None;
    if !fleet_open() {
        match admit(&mut s, &ctx, &h, &addr)? {
            Ok(key) => {
                // sealed HEARTBEAT carrying a fresh salt = "admitted"; the link key is derived from it (repl::Half)
                let salt = auth::nonce();
                send_sealed(&mut s, &mut FrameMac::new(&key), HEARTBEAT, &salt)?;
                sk = Some(crate::repl::link_key(&key, &salt));
            }
            Err(why) => return deny(&mut s, &ctx, &h, &addr, &why),
        }
    }
    crate::say!("ha.standby_join", "+ standby {addr} node {}{}", h.node_id, if sk.is_some() { " (sealed)" } else { "" });
    crate::repl::serve_standby(&ctx, s, p, sk)
}

fn serve_client(s: TcpStream, ctx: Arc<Ctx>) -> io::Result<()> {
    let peer = s.peer_addr().map_or("?".into(), |a| a.ip().to_string());
    let (mut r, mut w) = conn(s)?;
    loop {
        let (k, p) = recv(&mut r)?;
        match k {
            PING => send(&mut w, PONG, &p)?,
            SUBMIT | SUBMIT_JSON => {
                let spec = if k == SUBMIT { Ok(client_job(&p, &peer)) } else { client_job_json(&p, &peer) };
                let (tx, rx) = mpsc::channel();
                let row = match spec.and_then(|spec| submit_with(&ctx, spec, Some(tx))) {
                    Ok(row) => row,
                    Err(e) => return send(&mut w, ERR, e.as_bytes()),
                };
                send(&mut w, ACCEPTED, &row.id.to_le_bytes())?;
                return stream(&mut w, rx); // a client that leaves does not cancel the job (JOB-02)
            }
            ATTACH => return attach(&ctx, &mut w, u64_at(&p, 0)?),
            _ => {}
        }
    }
}

fn stream(w: &mut Sock, rx: mpsc::Receiver<(u8, Vec<u8>)>) -> io::Result<()> {
    for (k, body) in rx {
        send(w, k, &body)?;
        if k == DONE || k == ERR {
            break;
        }
    }
    Ok(())
}

/// Replay stored parts in order, then live ones, then DONE.
fn attach(ctx: &Ctx, w: &mut Sock, id: u64) -> io::Result<()> {
    let (row, parts, rx) = {
        let mut st = ctx.sh.0.lock().unwrap();
        let Some(row) = ctx.store.job(id) else { return send(w, ERR, b"unknown job") };
        let parts = ctx.store.parts(id);
        let rx = (!row.state.finished()).then(|| {
            let (tx, rx) = mpsc::channel();
            st.subs.entry(id).or_default().push(tx);
            rx
        });
        (row, parts, rx)
    };
    for (a, b, body) in parts {
        let mut p = pack(&[a, b]);
        p.extend_from_slice(&body);
        send(w, PART, &p)?;
    }
    match rx {
        Some(rx) => stream(w, rx),
        None if row.state == JobState::Done => send(w, DONE, &[]),
        None => send(w, ERR, row.error.as_deref().unwrap_or(row.state.as_str()).as_bytes()),
    }
}

/// Accept a job (it starts at once and waits for eligible nodes), store it, publish it. Used by the API and by TCP SUBMIT.
pub fn submit(ctx: &Ctx, spec: JobSpec) -> Result<JobRow, String> {
    submit_with(ctx, spec, None)
}

fn submit_with(ctx: &Ctx, mut spec: JobSpec, sub: Option<mpsc::Sender<(u8, Vec<u8>)>>) -> Result<JobRow, String> {
    if spec.kind == KIND_LIST {
        if spec.items.is_empty() {
            return Err("a list job needs items".into());
        }
        (spec.from, spec.end) = (0, spec.items.len() as u64); // tasks are item index ranges
    }
    if !spec.policy.is_empty() && sched::parse_policy(&spec.policy).is_none() {
        return Err(format!("unknown policy '{}': use adaptive, guided or fixed", spec.policy));
    }
    if spec.end <= spec.from {
        return Err("nothing to do: the range is empty".into());
    }
    // wave 13: items may name blobs (@blob:<sha>...); the job needs those too, and every blob must exist on this head
    for it in &spec.items {
        if let Some(sha) = crate::blobs::item_blob(it) {
            if !spec.blobs.iter().any(|b| b == sha) {
                spec.blobs.push(sha.to_string());
            }
        }
    }
    if let Some(b) = spec.blobs.iter().find(|b| !crate::blobs::valid_sha(b) || ctx.store.blob_size(b).is_none()) {
        return Err(format!("unknown blob {b}: upload it first (PUT /api/v1/blobs)"));
    }
    if !spec.blob_mode.is_empty() && !matches!(spec.blob_mode.as_str(), "auto" | "direct" | "peer") {
        return Err("blob_mode must be auto, direct or peer".into());
    }
    for b in &spec.blobs {
        ctx.store.touch_blob(b, now_ms());
    }
    parse_sel(&spec.selector)?;
    if spec.priority as usize >= PRIORITIES.len() {
        return Err(format!("priority must be 0..{} ({})", PRIORITIES.len() - 1, PRIORITIES.join(", ")));
    }
    let row = ctx.store.create_job(&spec, now_ms())?;
    crate::repl::record(ctx, "job", serde_json::json!({ "id": row.id, "spec": spec }));
    event(ctx, "job.submitted", &spec.submitter, &format!("job {}: {} {}..{}", row.id, row.name, spec.from, spec.end));
    publish_job(ctx, &row);
    let mut st = ctx.sh.0.lock().unwrap();
    if let Some(tx) = sub {
        st.subs.entry(row.id).or_default().push(tx);
    }
    start_job(ctx, &mut st, row.id);
    let sends = if spec.preempt { preempt_lower(&mut st, row.id) } else { vec![] };
    drop(st);
    flush(sends);
    Ok(ctx.store.job(row.id).unwrap_or(row))
}

/// SCH-09 `preempt: true`: take back every outstanding chunk of jobs with a lower priority than job `id` at once. The
/// ranges go back on their job's queue (CANCEL_RANGE tells the PC to drop that chunk); a result that still arrives is
/// stored once and subtracted from the queue like a lost speculative race, so every number is credited exactly once.
fn preempt_lower(st: &mut State, id: u64) -> Sends {
    let Some(pri) = st.jobs.get(&id).map(|j| j.priority) else { return vec![] };
    let mut sends = vec![];
    let mut back: Vec<(u64, u64, u64)> = vec![];
    for w in st.workers.values_mut() {
        let lower: Vec<_> = w.inflight.iter().filter(|t| st.jobs.get(&t.0).is_some_and(|j| j.priority > pri)).copied().collect();
        for t in lower {
            w.inflight.retain(|x| *x != t);
            w.sent.remove(&t);
            if w.staged == Some(t) {
                w.staged = None;
            } else {
                sends.extend(w.out.clone().map(|o| (o, CANCEL_RANGE, pack(&[t.0, t.1, t.2]))));
            }
            back.push(t);
        }
    }
    for (j, a, b) in back {
        if let Some(j) = st.jobs.get_mut(&j) {
            j.requeue.push((a, b));
            j.requeue.sort_by(|x, y| y.cmp(x)); // lowest last
        }
    }
    sends
}

/// Cancel a queued or running job. Chunks already running on workers are told to stop.
pub fn cancel_job(ctx: &Ctx, id: u64, actor: &str) -> Result<JobRow, String> {
    let row = ctx.store.job(id).ok_or("no such job")?;
    if row.state.finished() {
        return Err("job already finished".into());
    }
    let mut st = ctx.sh.0.lock().unwrap();
    let outs = stop_job(ctx, &mut st, id, JobState::Cancelled, Some("job cancelled"), "job.cancelled", actor, &format!("job {id} cancelled"));
    crate::say!("job.cancelled", "job {id} cancelled by {actor}");
    drop(st);
    cancel_outs(outs, id);
    ctx.store.job(id).ok_or_else(|| "no such job".into())
}

/// HA-02: after a restart, every unfinished job goes back in the queue; its remaining ranges come from the stored parts.
fn resume(ctx: &Ctx) {
    let mut st = ctx.sh.0.lock().unwrap();
    let mut ids: Vec<u64> = ctx.store.unfinished().iter().map(|r| r.id).collect();
    ids.sort();
    if !ids.is_empty() {
        crate::say!("job.resume", "resuming {} unfinished job(s)", ids.len());
    }
    for id in ids {
        start_job(ctx, &mut st, id);
    }
}

pub fn run(o: &Opts) {
    now(); // start the dashboard clock
    let sh: Shared = Arc::default();
    let store = crate::store::Store::open(&crate::store::default_path()).unwrap_or_else(|e| die(&format!("database: {e}")));
    let ctx = Arc::new(Ctx { sh: sh.clone(), store: Arc::new(store), bus: Default::default(), repl: Default::default() });
    crate::tune::load(ctx.store.meta("tune").as_deref());
    load_quotas(&ctx, &mut ctx.sh.0.lock().unwrap());
    init_ha(&ctx);
    resume(&ctx);
    if !o.no_local && !fleet_open() {
        // the head's own worker joins like any PC, with a one-use code made here
        let id = crate::identity::node_id();
        if !crate::identity::key_path(&id).exists() {
            let code: String = auth::nonce()[..8].iter().map(|b| format!("{b:02X}")).collect(); // upper case: plan() hashes codes upper-cased
            let _ = ctx.store.add_join_code(&crate::users::sha256_hex(code.as_bytes()), 1, now_ms() + 60_000, true, "head", now_ms());
            let _ = crate::agent::LOCAL_CODE.set(code);
        }
    }
    let wd = ctx.clone();
    thread::spawn(move || watchdog(wd));
    let pc = ctx.clone();
    thread::spawn(move || power_loop(pc));
    let rc = ctx.clone();
    thread::spawn(move || research_loop(rc));
    let bc = ctx.clone();
    thread::spawn(move || crate::blobs::head_loop(bc));
    let l = TcpListener::bind(("0.0.0.0", port())).unwrap_or_else(|e| die(&format!("TCP {}: {e}", port())));
    let name = o.name.clone();
    let (cb, cw, n2) = (ctx.clone(), ctx.clone(), o.name.clone());
    thread::spawn(move || beacon(&cb, &name));
    thread::spawn(move || watch_epoch(cw, n2));
    let cg = ctx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        gate_tick(&cg);
    });
    let ctx2 = ctx.clone();
    thread::spawn(move || crate::api::serve(ctx2));
    let shb = ctx.sh.clone();
    thread::spawn(move || heartbeat_all(shb));
    if !o.no_local {
        let (name, threads) = (o.name.clone(), o.threads);
        let use_gpu = o.gpu;
        thread::spawn(move || crate::agent::worker(Some("127.0.0.1".into()), &name, threads, use_gpu));
    }
    let ip = local_ip().map_or("?".into(), |i| i.to_string());
    println!("HEAD ready  tcp://{ip}:{}  dashboard http://{ip}:{}  cluster '{}'", port(), dash_port(), o.name);

    let mut next_conn = 0u64;
    for s in l.incoming().flatten() {
        next_conn += 1;
        let (ctx, wid) = (ctx.clone(), next_conn);
        let _ = thread::Builder::new().stack_size(2 << 20).spawn(move || {
            let mut s = s;
            let _ = s.set_nodelay(true);
            // nobody has vouched for this connection yet: a hello is small and must arrive soon. Without the limits a
            // silent TCP connect held a thread forever and a 256 MiB "hello" was allocated before it was read.
            let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
            let first = crate::proto::recv_max(&mut s, 1 << 20);
            let _ = s.set_read_timeout(None);
            let _ = match first { // hello is read unbuffered so nothing is left in a buffer
                Ok((HELLO_W, p)) => serve_worker(s, ctx, &p, wid),
                Ok((HELLO_C, _)) => serve_client(s, ctx),
                Ok((HELLO_S, p)) => serve_standby_conn(s, ctx, &p),
                _ => Ok(()),
            };
        });
    }
}

/// Candidate (promoted, no quorum yet) dispatches nothing until gate_tick sees quorum.
fn init_ha(ctx: &Ctx) {
    crate::repl::start(ctx);
    if ctx.repl.status.lock().unwrap().role == "candidate" {
        ctx.sh.0.lock().unwrap().gated = true;
        crate::say!("ha.candidate", "candidate: epoch {}, waiting for quorum", crate::repl::epoch(ctx));
        event(ctx, "ha.promoted", "head", &format!("this head took over at epoch {}", crate::repl::epoch(ctx)));
    }
}

fn beacon(ctx: &Ctx, name: &str) {
    let s = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
    s.set_broadcast(true).expect("broadcast");
    // CK_BEACON_PORTS=7951,7956: also tell heads on this machine with another CK_PORT (one-machine tests)
    let extra: Vec<u16> = std::env::var("CK_BEACON_PORTS").unwrap_or_default().split(',').filter_map(|p| p.trim().parse().ok()).collect();
    loop {
        let e = crate::repl::epoch(ctx);
        let msg = match local_ip() {
            Some(ip) => format!("ck/1 {name} {ip}:{} e={e}", port()), // FLT-05: agents connect to this address; M4: head term
            None => format!("ck/1 {name}"),
        };
        // signed copy for heads and standbys (repl::fence_key); agents ignore it
        let signed = local_ip().zip(crate::repl::fence_key(&ctx.store)).map(|(ip, k)| crate::repl::fence_msg(&k, name, &format!("{ip}:{}", port()), e));
        for p in &extra {
            let _ = s.send_to(msg.as_bytes(), ("127.0.0.1", *p));
            if let Some(m) = &signed {
                let _ = s.send_to(m.as_bytes(), ("127.0.0.1", *p));
            }
        }
        let mut targets = vec!["255.255.255.255".to_string()];
        if let Some(IpAddr::V4(ip)) = local_ip() {
            let o = ip.octets();
            targets.push(format!("{}.{}.{}.255", o[0], o[1], o[2])); // ponytail: assumes /24, use --head on other subnets
        }
        for t in &targets {
            let _ = s.send_to(msg.as_bytes(), (t.as_str(), disc_port()));
            if let Some(m) = &signed {
                let _ = s.send_to(m.as_bytes(), (t.as_str(), disc_port()));
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}

// ---- wave 7: power modes (NET-06) and dynamic heads (HA-10) ----
// Lock order: State lock, then PW. Never take the State lock while holding PW.


#[derive(Default)]
struct Pw {
    loaded: bool,
    asleep: BTreeSet<String>,
    waking: HashMap<String, Instant>,
    since: HashMap<String, i64>,          // epoch ms a PC entered its current asleep / waking state (wave 14)
    idle: HashMap<String, Instant>,       // since when a PC has had nothing to do
    tried: HashMap<String, Instant>,      // PCs asked to start a standby (or that failed): not asked again for a while
    last_recruit: Option<Instant>,
}

static PW: OnceLock<Mutex<Pw>> = OnceLock::new();

fn pw() -> std::sync::MutexGuard<'static, Pw> {
    PW.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// PW with the asleep list loaded from store meta "asleep" on first use.
fn pw_ctx(ctx: &Ctx) -> std::sync::MutexGuard<'static, Pw> {
    let mut g = pw();
    if !g.loaded {
        g.loaded = true;
        g.asleep = ctx.store.meta("asleep").and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default().into_iter().collect();
    }
    g
}

fn save_asleep(ctx: &Ctx, g: &Pw) {
    let _ = ctx.store.set_meta("asleep", &serde_json::to_string(&g.asleep).unwrap_or_default());
}

fn power_cfg(ctx: &Ctx) -> crate::jobs::PowerSettings {
    ctx.store.meta("power").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn ha_cfg(ctx: &Ctx) -> crate::jobs::HaSettings {
    // the stored setting wins; without one the target_standbys tunable is the default
    ctx.store.meta("ha_settings").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| crate::jobs::HaSettings { target_standbys: crate::tune::n("target_standbys") as u32, ..Default::default() })
}

/// "awake" | "asleep" | "waking". A PC the head put to sleep stays asleep until it reconnects.
pub fn power_state(ctx: &Ctx, node_id: &str) -> &'static str {
    let g = pw_ctx(ctx);
    if g.waking.get(node_id).is_some_and(|t| t.elapsed() < crate::tune::secs("waking_for_s")) {
        "waking"
    } else if g.asleep.contains(node_id) {
        "asleep"
    } else {
        "awake"
    }
}

/// When the PC entered its current power state (epoch ms); None if this head has not seen it change.
pub fn power_since(ctx: &Ctx, node_id: &str) -> Option<i64> {
    pw_ctx(ctx).since.get(node_id).copied()
}

/// Connected PCs by node id: (has chunks in flight, someone is at the PC). For the lab power estimate.
pub fn load_by_node(ctx: &Ctx) -> HashMap<String, (bool, bool)> {
    let st = ctx.sh.0.lock().unwrap();
    st.workers.values().filter_map(|w| Some((w.hello.as_ref()?.node_id.clone(), (!w.inflight.is_empty(), w.presence == "busy")))).collect()
}

/// serve_worker: a PC connected, so it is awake.
fn power_arrived(ctx: &Ctx, node_id: &str) {
    let mut g = pw_ctx(ctx);
    let was = g.waking.remove(node_id).is_some() | g.asleep.remove(node_id);
    g.since.insert(node_id.to_string(), now_ms());
    if was {
        save_asleep(ctx, &g);
        drop(g);
        event(ctx, "power.wake", "head", &format!("node {node_id} is back"));
    }
}

struct Peer {
    id: String,
    addr: String,
    out: Option<Arc<Mutex<Out>>>,
    virt: bool,
    busy: bool, // chunks in flight or someone at the PC
    state: String,
    attrs: Attrs,
    macs: usize,
}

/// Connected PCs and the jobs that have work left as (selector, chunks left); None while dispatch is stopped.
fn snap(ctx: &Ctx) -> (Vec<Peer>, Vec<(Selector, u64, u32)>) {
    let st = ctx.sh.0.lock().unwrap();
    let peers = st.workers.values().filter_map(|w| {
        let h = w.hello.as_ref()?;
        Some(Peer {
            id: h.node_id.clone(), addr: w.addr.clone(), out: w.out.clone(), virt: h.virtual_node,
            busy: !w.inflight.is_empty() || w.presence == "busy", state: w.state.clone(), attrs: w.attrs.clone(), macs: h.inv.macs.len(),
        })
    });
    let jobs = st.jobs.values().filter(|j| j.has_work()).map(|j| (j.selector.clone(), j.requeue.len() as u64 + (j.end.saturating_sub(j.next)) / j.chunk.max(1), j.max_pcs));
    (peers.collect(), jobs.collect())
}

fn primary(ctx: &Ctx) -> bool {
    let st = ctx.sh.0.lock().unwrap();
    !st.halted && !st.gated && crate::repl::status(ctx).role == "primary"
}

fn ctrl_to(o: &Arc<Mutex<Out>>, v: serde_json::Value) {
    let _ = o.lock().unwrap().put(CTRL, v.to_string().as_bytes());
}

/// Put PCs to sleep: CTRL {"power":"sleep"} to each, remembered as asleep (meta "asleep"). Err if one is not connected.
fn sleep_nodes(ctx: &Ctx, ids: &[String], actor: &str) -> Result<Vec<String>, String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let mut outs = vec![];
    {
        let mut st = ctx.sh.0.lock().unwrap();
        for id in &ids {
            let Some(w) = st.workers.values_mut().find(|w| w.out.is_some() && w.hello.as_ref().is_some_and(|h| &h.node_id == id)) else {
                return Err(format!("node {id} is not connected"));
            };
            w.presence = "busy".into(); // no new chunks while it goes down; a reconnect starts fresh
            outs.push(w.out.clone().unwrap());
        }
    }
    for o in &outs {
        ctrl_to(o, serde_json::json!({"power": "sleep"}));
    }
    let mut g = pw_ctx(ctx);
    for id in &ids {
        g.asleep.insert(id.clone());
        g.waking.remove(id);
        g.since.insert(id.clone(), now_ms());
        g.idle.remove(id);
    }
    save_asleep(ctx, &g);
    drop(g);
    for id in &ids {
        event(ctx, "power.sleep", actor, &format!("node {id} put to sleep"));
    }
    Ok(ids)
}

/// Wake PCs: magic packets from the head plus CTRL wol to one connected PC on the same /24. Err if a PC has no known MAC.
fn wake_nodes(ctx: &Ctx, ids: &[String], actor: &str) -> Result<Vec<String>, String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let mut plan = vec![];
    for id in &ids {
        let row = ctx.store.node(id).ok_or_else(|| format!("unknown node {id}"))?;
        let inv: Inventory = serde_json::from_str(&row.inventory).unwrap_or_default();
        if inv.macs.is_empty() {
            return Err(format!("node {id} has no known MAC address"));
        }
        plan.push((id.clone(), row.addr, inv.macs));
    }
    let (peers, _) = snap(ctx);
    let mut done = vec![];
    for (id, addr, macs) in plan {
        let asleep = pw_ctx(ctx).asleep.contains(&id);
        if peers.iter().any(|p| p.id == id) {
            if asleep {
                power_arrived(ctx, &id); // connected all along (CK_POWER_DRY): it only needs to leave the list
                done.push(id);
            }
            continue;
        }
        crate::agent::send_wol(&macs);
        let net = |a: &str| a.parse::<std::net::Ipv4Addr>().ok().map(|i| { let o = i.octets(); [o[0], o[1], o[2]] });
        let helper = peers.iter().filter(|p| !p.virt && p.id != id && !pw_ctx(ctx).asleep.contains(&p.id)).find(|p| net(&p.addr).is_some() && net(&p.addr) == net(&addr));
        if let Some(o) = helper.and_then(|p| p.out.as_ref()) {
            ctrl_to(o, serde_json::json!({"wol": macs}));
        }
        {
            let mut g = pw_ctx(ctx);
            g.waking.insert(id.clone(), Instant::now());
            g.since.insert(id.clone(), now_ms());
        }
        event(ctx, "power.wake", actor, &format!("node {id}: Wake-on-LAN sent ({} MAC)", macs.len()));
        done.push(id);
    }
    Ok(done)
}

/// API entry (POST /api/v1/nodes/power): action "sleep" or "wake"; returns the node ids acted on.
pub fn power(ctx: &Ctx, ids: &[String], action: &str, actor: &str) -> Result<Vec<String>, String> {
    match action {
        "sleep" => sleep_nodes(ctx, ids, actor),
        "wake" => wake_nodes(ctx, ids, actor),
        _ => Err(format!("action must be sleep or wake, not {action}")),
    }
}

/// api.rs after PUT /api/v1/power. Leaving eco wakes everything the head put to sleep.
pub fn power_settings_changed(ctx: &Ctx) {
    pw_ctx(ctx).idle.clear();
    if power_cfg(ctx).mode != "eco" {
        let ids: Vec<String> = pw_ctx(ctx).asleep.iter().cloned().collect();
        if !ids.is_empty() {
            if let Err(e) = wake_nodes(ctx, &ids, "head") {
                crate::warnl!("power.error", "power: {e}");
            }
        }
    }
}

/// api.rs after PUT /api/v1/ha/settings: look at the standby count now.
pub fn ha_settings_changed(ctx: &Ctx) {
    pw().last_recruit = None;
    recruit_tick(ctx);
}

/// Who to sleep: not protected, idle long enough, longest idle first, never below `keep` awake PCs. Pure.
fn choose_sleep(c: &[(String, u64, bool)], idle_min: u32, awake: usize, keep: u32) -> Vec<String> {
    let mut v: Vec<_> = c.iter().filter(|x| !x.2 && x.1 >= idle_min as u64 * 60).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().take(awake.saturating_sub(keep as usize)).map(|x| x.0.clone()).collect()
}

/// Who to wake: per demand (selector, how many), asleep PCs that match, each once. Pure.
fn choose_wake(asleep: &[(String, Attrs)], demand: &[(Selector, usize)]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (sel, need) in demand {
        let mut n = *need;
        for (id, a) in asleep {
            if n > 0 && !out.contains(id) && sel.matches(a) {
                out.push(id.clone());
                n -= 1;
            }
        }
    }
    out
}

/// Whom to ask for a standby: None when `have` reaches `target`; else a candidate (id, lab) from a lab without one first. Pure.
fn choose_recruit(have: usize, target: usize, cands: &[(String, String, f64)], taken_labs: &std::collections::HashSet<String>) -> Option<String> {
    if have >= target {
        return None;
    }
    // a lab without a standby first, then the highest head score, then the id
    cands.iter().min_by(|a, b| taken_labs.contains(&a.1).cmp(&taken_labs.contains(&b.1)).then(b.2.total_cmp(&a.2)).then(a.0.cmp(&b.0))).map(|c| c.0.clone())
}

fn idle_update(ctx: &Ctx) {
    let (peers, _) = snap(ctx);
    let mut g = pw_ctx(ctx);
    g.idle.retain(|k, _| peers.iter().any(|p| &p.id == k));
    for p in &peers {
        if p.busy {
            g.idle.remove(&p.id);
        } else {
            g.idle.entry(p.id.clone()).or_insert_with(Instant::now);
        }
    }
}

fn eco_tick(ctx: &Ctx) {
    let cfg = power_cfg(ctx);
    if cfg.mode != "eco" || !primary(ctx) {
        return;
    }
    let (peers, jobs) = snap(ctx);
    let head = crate::identity::node_id();
    let standbys: std::collections::HashSet<String> = crate::repl::standby_nodes(ctx).into_iter().collect();
    let (asleep, idle) = {
        let g = pw_ctx(ctx);
        (g.asleep.clone(), g.idle.clone())
    };
    if cfg.wake_on_demand {
        let free = |s: &Selector| peers.iter().filter(|p| !asleep.contains(&p.id) && !p.busy && takes_tasks(&p.state) && s.matches(&p.attrs)).count();
        let demand: Vec<(Selector, usize)> = jobs.iter().filter(|(s, ..)| free(s) == 0).map(|(s, n, m)| {
            let mut k = (*n).min(crate::tune::n("wake_max")) as usize;
            if *m > 0 {
                // a limited job needs only max_pcs PCs in all, counting the matching ones already awake
                k = k.min((*m as usize).saturating_sub(peers.iter().filter(|p| !asleep.contains(&p.id) && s.matches(&p.attrs)).count()));
            }
            (s.clone(), k)
        }).filter(|d| d.1 > 0).collect();
        if !demand.is_empty() {
            let (rows, labels, pools) = (ctx.store.nodes(), ctx.store.labels(), ctx.store.pools());
            let labs: HashMap<u64, String> = ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect();
            let waking: Vec<String> = pw_ctx(ctx).waking.iter().filter(|(_, t)| t.elapsed() < crate::tune::secs("wake_retry_s")).map(|(k, _)| k.clone()).collect();
            let cands: Vec<(String, Attrs)> = rows.iter().filter(|r| asleep.contains(&r.node_id) && !waking.contains(&r.node_id)).map(|r| {
                let lb: Vec<(String, String)> = labels.iter().filter(|l| l.0 == r.node_id).map(|l| (l.1.clone(), l.2.clone())).collect();
                let pl: Vec<String> = pools.iter().filter(|p| p.members.contains(&r.node_id)).map(|p| p.name.clone()).collect();
                (r.node_id.clone(), select::attrs(r, &lb, &pl, r.lab_id.and_then(|i| labs.get(&i)).map(|s| s.as_str()), None))
            }).collect();
            let ids = choose_wake(&cands, &demand);
            if !ids.is_empty() {
                crate::lg!(Info, "power.eco", {"action": "wake", "nodes": ids});
                if let Err(e) = wake_nodes(ctx, &ids, "eco") {
                    crate::warnl!("power.error", "power: {e}");
                }
            }
        }
    }
    let awake = peers.iter().filter(|p| !p.virt && !asleep.contains(&p.id)).count();
    let cands: Vec<(String, u64, bool)> = peers.iter().filter(|p| !p.virt && !asleep.contains(&p.id)).map(|p| {
        let protected = p.id == head || standbys.contains(&p.id) || p.busy || !takes_tasks(&p.state) || p.macs == 0 || jobs.iter().any(|(s, ..)| s.matches(&p.attrs));
        (p.id.clone(), idle.get(&p.id).map_or(0, |t| t.elapsed().as_secs()), protected)
    }).collect();
    let ids = choose_sleep(&cands, cfg.idle_min, awake, cfg.keep_awake);
    if !ids.is_empty() {
        crate::lg!(Info, "power.eco", {"action": "sleep", "nodes": ids, "awake": awake});
        if let Err(e) = sleep_nodes(ctx, &ids, "eco") {
            crate::warnl!("power.error", "power: {e}");
        }
    }
}

fn tune_msg() -> serde_json::Value {
    serde_json::json!({"tune": crate::tune::agent_values()})
}

/// api.rs after PUT /api/v1/tunables (and a changed value on the head itself): agents get the agent-scope values now,
/// the standby count is looked at again.
pub fn tune_changed(ctx: &Ctx) {
    let outs: Vec<_> = ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| w.out.clone()).collect();
    let m = tune_msg();
    for o in &outs {
        ctrl_to(o, m.clone());
    }
    crate::say!("tune.sent", "tune: sent to {} agents: {}", outs.len(), m["tune"]);
    pw().last_recruit = None;
}

/// One PC as a head candidate (HA-10). `lab` is "" without a seat; `why` is "" when it can be a standby.
struct Cand {
    id: String,
    host: String,
    lab: String,
    score: f64,
    parts: [f64; 4],
    role: &'static str,
    why: String,
}

/// `node.lost` events of the last 24 h per node id. The event detail ends with "(node <id>)".
fn losses_24h(ctx: &Ctx) -> HashMap<String, u32> {
    let (since, mut before, mut m) = (now_ms() - 86_400_000, None, HashMap::new());
    for _ in 0..40 {
        let page = ctx.store.events(500, before);
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        for e in page.iter().filter(|e| e.kind == "node.lost" && e.ts_ms >= since) {
            if let Some(id) = e.detail.rsplit("(node ").next().and_then(|t| t.strip_suffix(')')) {
                *m.entry(id.to_string()).or_insert(0) += 1;
            }
        }
        if last.ts_ms < since {
            break;
        }
    }
    m
}

fn weights() -> sched::HeadWeights {
    let g = crate::tune::get;
    sched::HeadWeights { speed: g("head_w_speed"), memory: g("head_w_memory"), stability: g("head_w_stability"), free: g("head_w_free") }
}

/// Every known PC scored as a head. `sel` is the standby selector from the HA settings.
fn cand_list(ctx: &Ctx, sel: &Selector) -> Vec<Cand> {
    let rows = ctx.store.nodes();
    let labs: HashMap<u64, String> = ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect();
    let (peers, _) = snap(ctx);
    let live: HashMap<String, (f64, bool)> = {
        let st = ctx.sh.0.lock().unwrap();
        st.workers.values().filter_map(|w| Some((w.hello.as_ref()?.node_id.clone(), (w.calib, w.presence == "busy")))).collect()
    };
    let (head, standbys) = (crate::identity::node_id(), crate::repl::standby_nodes(ctx));
    let asleep = pw_ctx(ctx).asleep.clone();
    let lost = losses_24h(ctx);
    let inputs: Vec<sched::HeadInputs> = rows.iter().map(|r| {
        let inv: Inventory = serde_json::from_str(&r.inventory).unwrap_or_default();
        let (calib, busy) = live.get(&r.node_id).copied().unwrap_or((0.0, false));
        sched::HeadInputs {
            calib_mhs: r.calib_mhs.unwrap_or(0.0).max(calib), cores: inv.cores_logical, ram_gb: inv.ram_bytes as f64 / (1u64 << 30) as f64,
            losses_24h: lost.get(&r.node_id).copied().unwrap_or(0), in_use: busy,
        }
    }).collect();
    let best = sched::HeadInputs {
        calib_mhs: inputs.iter().map(|x| x.calib_mhs).fold(0.0, f64::max), cores: inputs.iter().map(|x| x.cores).max().unwrap_or(0),
        ram_gb: inputs.iter().map(|x| x.ram_gb).fold(0.0, f64::max), ..Default::default()
    };
    rows.iter().zip(&inputs).map(|(r, x)| {
        let p = peers.iter().find(|p| p.id == r.node_id);
        let role = if r.node_id == head { "head" } else if standbys.contains(&r.node_id) { "standby" } else { "candidate" };
        let why = match p {
            None => "not connected",
            Some(p) if p.virt => "virtual PC",
            Some(p) if p.out.is_none() => "not connected",
            Some(p) if !takes_tasks(&p.state) => "not active",
            Some(_) if asleep.contains(&r.node_id) => "asleep",
            Some(p) if !sel.matches(&p.attrs) => "does not match the standby selector",
            _ => "",
        };
        let (score, parts) = sched::head_score(x, &best, weights());
        Cand {
            id: r.node_id.clone(), host: r.host.clone(), lab: r.lab_id.and_then(|i| labs.get(&i).cloned()).unwrap_or_default(),
            score, parts, role, why: why.into(),
        }
    }).collect()
}

/// GET /api/v1/ha/candidates: every PC scored as a head, best first.
pub fn candidates(ctx: &Ctx) -> Vec<serde_json::Value> {
    let sel = parse_sel(&ha_cfg(ctx).selector).unwrap_or_default();
    let mut v = cand_list(ctx, &sel);
    v.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    v.into_iter().map(|c| serde_json::json!({
        "node_id": c.id, "host": c.host, "lab": (!c.lab.is_empty()).then_some(c.lab), "score": c.score,
        "parts": {"speed": c.parts[0], "memory": c.parts[1], "stability": c.parts[2], "free": c.parts[3]},
        "role": c.role, "eligible": c.why.is_empty(), "why": c.why,
    })).collect()
}

fn recruit_tick(ctx: &Ctx) {
    let ha = ha_cfg(ctx);
    let target = ha.target_standbys as usize;
    let have = crate::repl::standby_count(ctx);
    if have >= target || !primary(ctx) || pw().last_recruit.is_some_and(|t| t.elapsed() < crate::tune::secs("recruit_every_s")) {
        return;
    }
    let Ok(sel) = parse_sel(&ha.selector) else { return };
    let Some(ip) = local_ip() else { return };
    let tried: std::collections::HashSet<String> = pw().tried.iter().filter(|(_, t)| t.elapsed() < crate::tune::secs("recruit_retry_s")).map(|(k, _)| k.clone()).collect();
    let all = cand_list(ctx, &sel);
    let taken: std::collections::HashSet<String> = all.iter().filter(|c| c.role == "standby").map(|c| c.lab.clone()).collect();
    let cands: Vec<(String, String, f64)> = all.iter().filter(|c| c.role == "candidate" && c.why.is_empty() && !tried.contains(&c.id)).map(|c| (c.id.clone(), c.lab.clone(), c.score)).collect();
    let Some(id) = choose_recruit(have, target, &cands, &taken) else { return };
    let Some(o) = snap(ctx).0.into_iter().find(|p| p.id == id).and_then(|p| p.out) else { return };
    ctrl_to(&o, serde_json::json!({"standby_of": format!("{ip}:{}", port())}));
    {
        let mut g = pw();
        g.last_recruit = Some(Instant::now());
        g.tried.insert(id.clone(), Instant::now());
    }
    event(ctx, "ha.recruit", "head", &format!("asked node {id} to start a standby head (have {have}, want {target})"));
}

/// Eco and recruit ticks at the eco_tick_s / recruit_tick_s tunables; idle tracking every 5 s.
fn power_loop(ctx: Arc<Ctx>) {
    let (mut idle, mut eco, mut rec) = (Instant::now(), Instant::now(), Instant::now());
    loop {
        thread::sleep(Duration::from_secs(1));
        if idle.elapsed() >= Duration::from_secs(5) {
            idle = Instant::now();
            idle_update(&ctx);
        }
        if eco.elapsed() >= crate::tune::secs("eco_tick_s") {
            eco = Instant::now();
            eco_tick(&ctx);
        }
        if rec.elapsed() >= crate::tune::secs("recruit_tick_s") {
            rec = Instant::now();
            recruit_tick(&ctx);
        }
    }
}

#[cfg(test)]
mod power_tests {
    use super::*;

    fn sel(s: &str) -> Selector {
        parse_sel(s).unwrap()
    }
    fn attrs(kv: &[(&str, &str)]) -> Attrs {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn sleep_chooser() {
        let c = |id: &str, idle: u64, prot: bool| (id.to_string(), idle, prot);
        let v = vec![c("a", 900, false), c("b", 1200, false), c("c", 5000, true), c("d", 100, false), c("e", 900, false)];
        // 10 awake, keep 8: two may go, longest idle first; c is protected, d not idle long enough
        assert_eq!(choose_sleep(&v, 15, 10, 8), vec!["b", "a"]);
        assert!(choose_sleep(&v, 15, 4, 4).is_empty(), "keep_awake floor");
        assert_eq!(choose_sleep(&v, 15, 5, 4), vec!["b"]);
        assert!(choose_sleep(&v, 100, 10, 0).iter().all(|i| i != "c"));
    }

    #[test]
    fn wake_chooser() {
        let asleep = vec![("x".to_string(), attrs(&[("lab", "l1")])), ("y".to_string(), attrs(&[("lab", "l2")])), ("z".to_string(), attrs(&[("lab", "l2")]))];
        assert_eq!(choose_wake(&asleep, &[(sel("lab=l2"), 1)]), vec!["y"]);
        assert_eq!(choose_wake(&asleep, &[(sel("lab=l2"), 5), (sel("lab=l1"), 1)]), vec!["y", "z", "x"]);
        assert!(choose_wake(&asleep, &[(sel("lab=nope"), 3)]).is_empty());
        assert!(choose_wake(&asleep, &[]).is_empty());
    }

    #[test]
    fn recruit_chooser() {
        let c = vec![("n1".to_string(), "A".to_string(), 50.0), ("n2".to_string(), "B".to_string(), 50.0), ("n3".to_string(), "B".to_string(), 90.0)];
        let none = std::collections::HashSet::new();
        assert_eq!(choose_recruit(0, 2, &c, &none).as_deref(), Some("n3"), "best score first");
        assert_eq!(choose_recruit(2, 2, &c, &none), None);
        let taken: std::collections::HashSet<String> = ["A".to_string()].into();
        assert_eq!(choose_recruit(1, 2, &c, &taken).as_deref(), Some("n3"), "a lab without a standby first, best score in it");
        let all: std::collections::HashSet<String> = ["A".to_string(), "B".to_string()].into();
        assert_eq!(choose_recruit(1, 2, &c, &all).as_deref(), Some("n3"), "all labs taken: any");
    }

    #[test]
    fn candidates_rank_faster_stabler_first() {
        let c = Ctx { sh: Arc::default(), store: Arc::new(crate::store::Store::memory()), bus: Default::default(), repl: Default::default() };
        let inv = |cores: u32, gb: u64| serde_json::to_string(&Inventory { cores_logical: cores, ram_bytes: gb << 30, ..Default::default() }).unwrap();
        for (id, cores, gb, mhs) in [("slow", 4, 8, 5.0), ("fast", 12, 32, 20.0), ("flaky", 12, 32, 20.0)] {
            c.store.upsert_node(id, id, "10.0.0.1", &inv(cores, gb), now_ms()).unwrap();
            c.store.set_node_calib(id, mhs).unwrap();
        }
        for _ in 0..3 {
            event(&c, "node.lost", "head", "flaky at 10.0.0.1 (node flaky)");
        }
        let v = candidates(&c);
        let ids: Vec<&str> = v.iter().map(|x| x["node_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["fast", "slow", "flaky"], "three losses outweigh the speed gap");
        assert_eq!(v[0]["why"], "not connected");
        assert_eq!(v[2]["parts"]["stability"], 0.25);
    }

    #[test]
    fn tune_reaches_a_connected_agent() {
        let c = Ctx { sh: Arc::default(), store: Arc::new(crate::store::Store::memory()), bus: Default::default(), repl: Default::default() };
        tune_changed(&c); // nobody connected: must not panic
        assert!(tune_msg()["tune"].is_object());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn range_math_and_order() {
        assert_eq!(gaps(10, 100, &[]), vec![(10, 100)]);
        assert_eq!(gaps(10, 100, &[(0, 20), (30, 40), (90, 200)]), vec![(20, 30), (40, 90)]);
        assert_eq!(gaps(10, 100, &[(10, 100)]), vec![]);
        let (mut rq, mut next) = (vec![(50, 60), (20, 30)], 100u64); // lowest last
        let mut got = vec![];
        while let Some(c) = next_chunk(&mut rq, &mut next, 100, 4) {
            got.push(c);
        }
        assert_eq!(got[..3], [(20, 24), (24, 28), (28, 30)]);
        assert_eq!(got.iter().map(|c| c.1 - c.0).sum::<u64>(), 20);
    }

    fn ctx() -> Ctx {
        Ctx { sh: Arc::default(), store: Arc::new(Store::memory()), bus: Default::default(), repl: Default::default() }
    }
    fn spec(n: u64) -> JobSpec {
        JobSpec { name: "t".into(), kind: KIND_SYNTH, from: 0, end: n, chunk: 10, arg: vec![0; 8], submitter: "test".into(), ..Default::default() }
    }
    fn result(job: u64, a: u64, b: u64) -> Vec<u8> {
        let mut p = pack(&[job, a, b, 0, 0, 0, 1000]);
        p.extend_from_slice(&(b - a).to_le_bytes());
        p
    }

    #[test]
    fn admission_and_failure_logic() {
        assert!(matches!(plan(Some(vec![1]), Some("x")), Plan::Challenge(_)), "known PC proves itself, a code does not help");
        assert!(matches!(plan(None, Some(" ABCD ")), Plan::Join(h) if h == crate::users::sha256_hex(b"ABCD")));
        assert!(matches!(plan(None, Some("abcd-efgh")), Plan::Join(h) if h == crate::users::sha256_hex(b"ABCD-EFGH")));
        assert!(matches!(plan(None, Some("")), Plan::Deny(_)) && matches!(plan(None, None), Plan::Deny(_)));
        assert_eq!(join_state(true, None), ("active".into(), false));
        assert_eq!(join_state(true, Some("pending")), ("active".into(), true));
        assert_eq!(join_state(true, Some("cordoned")), ("cordoned".into(), false));
        assert_eq!(join_state(false, Some("pending")), ("pending".into(), false));
        assert_eq!(join_state(false, None), ("pending".into(), false));
        let mut m = HashMap::new();
        assert_eq!([attempt(&mut m, (1, 0, 5), true), attempt(&mut m, (1, 0, 5), true), attempt(&mut m, (1, 0, 5), true)], [Verdict::Requeue, Verdict::Requeue, Verdict::Fail]);
        assert_eq!(attempt(&mut m, (1, 5, 10), false), Verdict::Fail, "no retry asked");
        assert_eq!(task_limit(0.0, 100), Duration::from_secs(60));
        assert_eq!(task_limit(1e6, 1000), Duration::from_secs(60));
        assert_eq!(task_limit(10.0, 1000), Duration::from_secs(2000));
    }

    #[test]
    fn failed_frames_retry_then_fail_job() {
        let c = ctx();
        let j = submit(&c, spec(20)).unwrap();
        let fail = |a: u64, b: u64, retry: u8| {
            let mut p = pack(&[j.id, a, b]);
            p.push(retry);
            p.extend_from_slice(b"exit 75");
            p
        };
        for _ in 0..2 {
            failed(&c, 1, &fail(0, 10, 1)).unwrap();
            assert_eq!(c.store.job(j.id).unwrap().state, JobState::Running);
        }
        assert_eq!(c.sh.0.lock().unwrap().jobs[&j.id].requeue[1..], [(0, 10), (0, 10)]);
        failed(&c, 1, &fail(0, 10, 1)).unwrap();
        let row = c.store.job(j.id).unwrap();
        assert_eq!((row.state, row.error.as_deref()), (JobState::Failed, Some("chunk 0..10 failed: exit 75")));
        assert!(c.sh.0.lock().unwrap().jobs.is_empty() && c.sh.0.lock().unwrap().attempts.is_empty());
        let j2 = submit(&c, spec(20)).unwrap();
        let mut p = pack(&[j2.id, 0, 10]);
        p.extend_from_slice(&[0]);
        failed(&c, 1, &p).unwrap(); // no retry: the job fails at once
        assert_eq!(c.store.job(j2.id).unwrap().state, JobState::Failed);
    }

    #[test]
    fn cap_ctrl_only_on_change_and_per_job_ranges() {
        assert_eq!(cap_ctrl(None, None), None);
        assert_eq!(cap_ctrl(None, Some(1)).unwrap(), br#"{"cap_threads":1}"#);
        assert_eq!(cap_ctrl(Some(1), None).unwrap(), br#"{"cap_threads":null}"#);
        assert_eq!(cap_ctrl(Some(2), Some(2)), None);
        // two jobs keep separate range books; a disconnect puts a chunk back into its own job only
        let c = ctx();
        let list = |n| JobSpec { kind: KIND_LIST, items: vec!["x".into(); n as usize], ..spec(n) }; // list jobs: chunk floor 1
        let (j1, j2) = (submit(&c, list(20)).unwrap(), submit(&c, list(30)).unwrap());
        {
            let mut st = c.sh.0.lock().unwrap();
            st.workers.insert(1, Worker { addr: "a".into(), state: "active".into(), ..Default::default() });
            let (id, a, b, ..) = take(&mut st, 1).unwrap();
            assert_eq!((id, a, b), (j1.id, 0, 10), "same priority: older job first");
            st.workers.get_mut(&1).unwrap().inflight.push((id, a, b));
            let t = take(&mut st, 1).unwrap();
            assert_eq!((t.0, t.1, t.2), (j1.id, 10, 20), "node stays on its job");
            assert_eq!(st.jobs[&j2.id].next, 30, "other job untouched");
            assert!(st.workers.get(&2).is_none() && take(&mut st, 2).is_none());
        }
        let _ = (j1, j2);
    }

    #[test]
    fn preempt_takes_back_lower_priority_chunks_once() {
        let c = ctx();
        let lo = submit(&c, JobSpec { priority: 4, ..spec(100) }).unwrap();
        {
            let mut st = c.sh.0.lock().unwrap();
            st.workers.insert(1, Worker { addr: "a".into(), state: "active".into(), ..Default::default() });
            let (id, a, b, ..) = take(&mut st, 1).unwrap();
            st.workers.get_mut(&1).unwrap().inflight.push((id, a, b));
        }
        let hi = submit(&c, JobSpec { priority: 0, preempt: true, ..spec(100) }).unwrap();
        let mut st = c.sh.0.lock().unwrap();
        assert!(st.workers[&1].inflight.is_empty(), "outstanding low-priority chunk was taken back");
        assert_eq!(st.jobs[&lo.id].requeue.len(), 1 + 0, "and queued again, once");
        let t = take(&mut st, 1).unwrap();
        assert_eq!(t.0, hi.id, "the urgent job now owns the PC");
    }

    #[test]
    fn queue_cancel_finish_resume() {
        let c = ctx();
        let (j1, j2, j3) = (submit(&c, spec(20)).unwrap(), submit(&c, spec(20)).unwrap(), submit(&c, spec(20)).unwrap());
        assert_eq!((j1.state, j2.state), (JobState::Running, JobState::Running), "all accepted jobs run");
        assert_eq!(running_jobs(&c), vec![j1.id, j2.id, j3.id]);
        assert_eq!(cancel_job(&c, j2.id, "t").unwrap().state, JobState::Cancelled);
        assert_eq!(running_jobs(&c), vec![j1.id, j3.id]);
        assert!(submit(&c, JobSpec { end: 0, ..spec(0) }).is_err());
        // finish job 1 via results (the second RESULT for (0,10) is a duplicate and must not count)
        finish_credit(&c, 1, &result(j1.id, 0, 10)).unwrap();
        finish_credit(&c, 1, &result(j1.id, 0, 10)).unwrap();
        assert_eq!(c.sh.0.lock().unwrap().jobs[&j1.id].left, 10);
        finish_credit(&c, 1, &result(j1.id, 10, 20)).unwrap();
        assert_eq!(c.store.job(j1.id).unwrap().state, JobState::Done);
        assert_eq!(running_jobs(&c), vec![j3.id], "j3 kept its own state");
        assert_eq!(cancel_job(&c, j1.id, "t").unwrap_err(), "job already finished");
        // attach replays the stored parts and then DONE
        assert_eq!(c.store.parts(j1.id).len(), 2);
        // restart: a fresh head over the same store resumes j3 with only the missing range
        finish_credit(&c, 1, &result(j3.id, 10, 20)).unwrap();
        let c2 = Ctx { sh: Arc::default(), store: c.store.clone(), bus: Default::default(), repl: Default::default() };
        resume(&c2);
        let st = c2.sh.0.lock().unwrap();
        let j = &st.jobs[&j3.id];
        assert_eq!((j.id, j.left, j.requeue.clone()), (j3.id, 10, vec![(0, 10)]));
    }

    #[test]
    fn max_pcs_and_fastest_placement_in_take() {
        let c = ctx();
        let mut sp = spec(100_000);
        sp.chunk = 1000;
        sp.max_pcs = 3;
        sp.placement = "fastest".into();
        sp.policy = "fixed".into();
        let j = submit(&c, sp).unwrap();
        let mut st = c.sh.0.lock().unwrap();
        for i in 1..=8u64 {
            st.workers.insert(i, Worker { addr: format!("w{i}"), state: "active".into(), presence: "idle".into(), calib: i as f64, threads: 4, ..Default::default() });
        }
        let mut holders = BTreeSet::new();
        for round in 0..3 {
            for i in 1..=8u64 {
                if let Some((jj, a, b, ..)) = take(&mut st, i) {
                    st.workers.get_mut(&i).unwrap().inflight.push((jj, a, b));
                    holders.insert(i);
                }
            }
            assert!(holders.len() <= 3, "round {round}: {holders:?}");
        }
        assert_eq!(holders, BTreeSet::from([6, 7, 8]), "the three highest calib");
        assert_eq!(st.jobs[&j.id].max_pcs, 3);
    }

    #[test]
    fn user_quota_caps_pcs_and_running_jobs() {
        let c = ctx();
        let mk = |n: u64| { let mut sp = spec(n); sp.chunk = 1000; sp.policy = "fixed".into(); sp.submitter = "ann".into(); sp };
        let (j1, j2, j3) = (submit(&c, mk(100_000)).unwrap(), submit(&c, mk(100_000)).unwrap(), submit(&c, mk(100_000)).unwrap());
        let mut st = c.sh.0.lock().unwrap();
        for i in 1..=8u64 {
            st.workers.insert(i, Worker { addr: format!("w{i}"), state: "active".into(), presence: "idle".into(), threads: 4, ..Default::default() });
        }
        st.quotas.insert("ann".into(), (2, 2));
        let mut holders = BTreeSet::new();
        let mut jobs = BTreeSet::new();
        for _ in 0..3 {
            for i in 1..=8u64 {
                if let Some((jj, a, b, ..)) = take(&mut st, i) {
                    st.workers.get_mut(&i).unwrap().inflight.push((jj, a, b));
                    holders.insert(i);
                    jobs.insert(jj);
                }
            }
        }
        assert_eq!(holders.len(), 2, "max_pcs 2 holds on 8 PCs: {holders:?}");
        assert_eq!(jobs, BTreeSet::from([j1.id, j2.id]), "the third job waits (max_running 2)");
        drop(st);
        assert_eq!(wait_reason(&c, j3.id), Some("quota"));
        assert_eq!(wait_reason(&c, j1.id), None, "it holds PCs");
        c.sh.0.lock().unwrap().quotas.clear();
        assert_eq!(wait_reason(&c, j3.id), None);
        assert!(note_files_roundtrip());
    }

    fn note_files_roundtrip() -> bool {
        note_files(&serde_json::json!({"job": 9, "a": 0, "b": 5, "files": [{"name": "a.txt", "sha": "ab".repeat(32), "size": 3}, {"name": "../x", "sha": "ab".repeat(32), "size": 1}, {"name": "ok", "sha": "zz", "size": 1}]}));
        FILES.lock().unwrap().remove(&(9, 0, 5)).is_some_and(|v| v.len() == 1 && v[0].0 == "a.txt")
    }

    #[test]
    fn chunks_need_only_their_own_shard_blobs() {
        let c = ctx();
        let shas: Vec<String> = (1..=4u8).map(|i| crate::blobs::hex(&[i; 32])).collect();
        for s in &shas {
            c.store.put_blob(s, 10, 0);
        }
        let code = crate::blobs::hex(&[9u8; 32]);
        c.store.put_blob(&code, 10, 0);
        let items: Vec<String> = shas.iter().map(|s| format!("@blob:{s}")).collect();
        let sp = JobSpec { kind: KIND_LIST, items, chunk: 1, blobs: vec![code.clone()], arg: b"x".to_vec(), ..spec(4) };
        let j = submit(&c, sp).unwrap();
        let mut st = c.sh.0.lock().unwrap();
        assert_eq!((st.jobs[&j.id].shared.clone(), st.jobs[&j.id].shards.len()), (vec![code.clone()], 4));
        let mk = |have: &[&String]| Worker { addr: "a".into(), state: "active".into(), presence: "idle".into(), blob_rep: true, hello: Some(Hello { node_id: "n".into(), ..Default::default() }), have: have.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        // a PC holding the code and shard 3 is given the chunk of shard 3 first, although chunk 0 is next in line
        st.workers.insert(1, mk(&[&code, &shas[2]]));
        let t = take(&mut st, 1).expect("ready chunk");
        assert_eq!((t.1, t.2), (2, 3), "locality: the chunk whose shard it caches");
        st.workers.get_mut(&1).unwrap().inflight.push((t.0, t.1, t.2));
        // a PC with nothing gets a chunk reserved, not dispatched; only that chunk's blobs are wanted
        st.workers.insert(2, mk(&[]));
        assert!(take(&mut st, 2).is_none());
        let (_, a, b) = st.workers[&2].staged.expect("staged");
        let need = crate::blobs::chunk_blobs(&st.jobs[&j.id].shared, &st.jobs[&j.id].shards, &st.jobs[&j.id].item_sha, a, b);
        assert_eq!(need.len(), 2, "code + one shard, not all four: {need:?}");
        assert!(st.workers[&2].inflight.contains(&(j.id, a, b)), "a lost PC would requeue it");
        // once the blobs arrive the same chunk goes out
        let w = st.workers.get_mut(&2).unwrap();
        need.iter().for_each(|n| {
            w.have.insert(n.clone());
        });
        let t = take(&mut st, 2).expect("now ready");
        assert_eq!((t.1, t.2), (a, b));
        assert!(st.workers[&2].staged.is_none() && !st.workers[&2].inflight.contains(&(j.id, a, b)));
    }

    #[test]
    fn ewma_and_variable_chunk_bookkeeping() {
        assert_eq!(ewma(0.0, 100.0), 100.0);
        assert!((ewma(100.0, 200.0) - 130.0).abs() < 1e-9);
        // lengths differ per call; every number is handed out exactly once, requeued ranges first, lowest first
        let (mut rq, mut next) = (vec![(500, 520), (100, 110)], 1000u64);
        let mut got = vec![];
        for len in [4, 100, 7, 3, 1, 50, 9999].into_iter().cycle().take(40) {
            if let Some(c) = next_chunk(&mut rq, &mut next, 1000, len) {
                got.push(c);
            }
        }
        assert_eq!(got[0], (100, 104));
        assert_eq!(got.iter().map(|c| c.1 - c.0).sum::<u64>(), 10 + 20 + 0); // next == end here: only the queue is handed out
        assert!(got.windows(2).all(|w| w[0].0 != w[1].0));
    }

    #[test]
    fn duplicate_result_dropped_and_blocked_busy_filtered() {
        let c = ctx();
        let j = submit(&c, spec(20)).unwrap();
        {
            let mut st = c.sh.0.lock().unwrap();
            for (id, addr) in [(1, "a"), (2, "b")] {
                st.workers.insert(id, Worker { addr: addr.into(), state: "active".into(), presence: "idle".into(), ..Default::default() });
            }
            // both hold (0,10): worker 2 got a speculative copy
            for id in [1, 2] {
                let w = st.workers.get_mut(&id).unwrap();
                w.inflight.push((j.id, 0, 10));
                w.sent.insert((j.id, 0, 10), Instant::now());
            }
            assert!(held(&st, (j.id, 0, 10)));
        }
        finish_credit(&c, 2, &result(j.id, 0, 10)).unwrap(); // the copy wins
        {
            let st = c.sh.0.lock().unwrap();
            assert_eq!(st.jobs[&j.id].left, 10);
            assert!(st.workers[&1].inflight.is_empty() && st.workers[&2].inflight.is_empty(), "the loser's entry is dropped too");
            assert!(st.rates.contains_key(&("b".to_string(), KIND_SYNTH)));
        }
        finish_credit(&c, 1, &result(j.id, 0, 10)).unwrap(); // the original arrives late: dropped
        assert_eq!(c.sh.0.lock().unwrap().jobs[&j.id].left, 10);
        // a node that is blocked or busy takes nothing; an idle one does
        let mut st = c.sh.0.lock().unwrap();
        assert!(take(&mut st, 1).is_some());
        st.workers.get_mut(&1).unwrap().blocked = true;
        assert!(take(&mut st, 1).is_none(), "blocked");
        st.workers.get_mut(&1).unwrap().blocked = false;
        st.workers.get_mut(&1).unwrap().presence = "busy".into();
        assert!(take(&mut st, 1).is_none(), "busy");
        assert!(!can_work(&st.workers[&1]) && can_work(&st.workers[&2]));
        let sel = Selector::default();
        assert_eq!(fleet(&st, &sel, KIND_SYNTH).0, 1, "busy node is not counted");
    }

    #[test]
    fn pending_acks_dup_acks_and_cancel_range() {
        let c = ctx();
        let j = submit(&c, spec(20)).unwrap();
        // a worker whose socket we can read from
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let cli = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (mut srv, _) = l.accept().unwrap();
        let out = Arc::new(Mutex::new(Out { s: Sock(cli), mac: None }));
        {
            let mut st = c.sh.0.lock().unwrap();
            st.workers.insert(1, Worker { addr: "a".into(), state: "active".into(), out: Some(out), ..Default::default() });
            st.workers.insert(2, Worker { addr: "b".into(), state: "active".into(), ..Default::default() });
        }
        let mut p = pack(&[j.id, 0, 10]);
        // pending bookkeeping: seq <= durable leave, the rest stay
        let mut pend = vec![(1, 7, 0, 5), (3, 7, 5, 9), (2, 8, 0, 1)];
        assert_eq!(drain_durable(&mut pend, 2), vec![(7, 0, 5), (8, 0, 1)]);
        assert_eq!(pend, vec![(3, 7, 5, 9)]);
        c.sh.0.lock().unwrap().workers.get_mut(&1).unwrap().pending.push((5, j.id, 0, 10));
        durable_upto(&c, 4);
        assert_eq!(c.sh.0.lock().unwrap().workers[&1].pending.len(), 1, "seq 5 is not durable at 4");
        durable_upto(&c, 5);
        assert!(c.sh.0.lock().unwrap().workers[&1].pending.is_empty());
        assert_eq!(recv(&mut srv).unwrap(), (ACK, p.clone()));
        // stub repl: durable at once, so a fresh result is ACKed by finish; the second copy (duplicate) is ACKed too
        p = pack(&[j.id, 10, 20]);
        c.sh.0.lock().unwrap().workers.get_mut(&2).unwrap().inflight.push((j.id, 10, 20));
        finish_credit(&c, 1, &result(j.id, 10, 20)).unwrap();
        assert_eq!(recv(&mut srv).unwrap(), (ACK, p.clone()));
        finish_credit(&c, 1, &result(j.id, 10, 20)).unwrap();
        assert_eq!(recv(&mut srv).unwrap(), (ACK, p), "stale/dup result is ACKed at once");
    }

    #[test]
    fn epoch_beacons_and_quorum_gating() {
        let own = "10.0.0.1:7700";
        assert_eq!(higher_epoch("ck/1 lab 10.0.0.2:7700 e=3", "lab", own, 2), Some(3));
        assert_eq!(higher_epoch("ck/1 lab 10.0.0.2:7700 e=3 relay", "lab", own, 2), Some(3));
        assert_eq!(higher_epoch("ck/1 lab 10.0.0.2:7700 e=2", "lab", own, 2), None, "equal epoch");
        assert_eq!(higher_epoch("ck/1 lab 10.0.0.1:7700 e=9", "lab", own, 2), None, "our own beacon");
        assert_eq!(higher_epoch("ck/1 other 10.0.0.2:7700 e=9", "lab", own, 2), None, "another cluster");
        assert_eq!(higher_epoch("ck/1 my lab 10.0.0.2:7700 e=4", "my lab", own, 1), Some(4));
        assert_eq!(higher_epoch("ck/1 lab 10.0.0.2:7700", "lab", own, 1), None, "old head without epoch");
        let long = Duration::from_secs(11);
        assert!(!dispatch_ok("candidate", false, 0, Duration::ZERO) && dispatch_ok("candidate", true, 0, Duration::ZERO));
        assert!(dispatch_ok("primary", false, 1, Duration::from_secs(9)), "grace period");
        assert!(!dispatch_ok("primary", false, 1, long), "standbys + no quorum for 10 s");
        assert!(dispatch_ok("primary", false, 0, long), "no standby: keeps working");
        // gate_tick on a candidate: 1 of 3 known nodes connected = no quorum, nothing is handed out; 2 of 3 = dispatching
        let c = ctx();
        c.store.set_meta("candidate", "1").unwrap(); // before repl's first use loads it
        c.store.set_meta("membership", r#"["a","b","c"]"#).unwrap();
        let _ = submit(&c, spec(20)).unwrap();
        init_ha(&c);
        let hello = |id: &str| Some(Hello { node_id: id.into(), ..Default::default() });
        c.sh.0.lock().unwrap().workers.insert(1, Worker { addr: "a".into(), state: "active".into(), hello: hello("a"), ..Default::default() });
        gate_tick(&c);
        assert!(c.sh.0.lock().unwrap().gated && !c.repl.status.lock().unwrap().dispatching);
        assert!(take(&mut c.sh.0.lock().unwrap(), 1).is_none());
        c.sh.0.lock().unwrap().workers.insert(2, Worker { addr: "b".into(), state: "active".into(), hello: hello("b"), ..Default::default() });
        gate_tick(&c);
        assert!(take(&mut c.sh.0.lock().unwrap(), 1).is_some());
        {
            let s = c.repl.status.lock().unwrap();
            assert_eq!((s.role.as_str(), s.connected, s.dispatching), ("primary", 2, true));
        }
        halt(&c);
        assert!(take(&mut c.sh.0.lock().unwrap(), 1).is_none(), "fenced head hands out nothing");
    }

    #[test]
    fn overlapping_resent_results_never_double_count() {
        assert_eq!((covered(0, 10, &[(5, 7), (9, 20)]), covered(30, 40, &[(0, 30)])), (3, 0));
        assert_eq!(subtract((0, 10), (3, 5)), vec![(0, 3), (5, 10)]);
        assert_eq!(subtract((0, 10), (0, 10)), vec![]);
        assert_eq!(subtract((0, 10), (20, 30)), vec![(0, 10)]);
        let c = ctx();
        let j = submit(&c, JobSpec { chunk: 0, ..spec(100) }).unwrap();
        let left = |c: &Ctx| c.sh.0.lock().unwrap().jobs.get(&j.id).map(|j| j.left);
        let queue = |c: &Ctx| c.sh.0.lock().unwrap().jobs[&j.id].requeue.clone();
        c.sh.0.lock().unwrap().workers.insert(1, Worker { addr: "a".into(), state: "active".into(), fed: 1, ..Default::default() });
        c.sh.0.lock().unwrap().workers.insert(2, Worker { addr: "b".into(), state: "active".into(), inflight: vec![(j.id, 60, 80)], fed: 1, ..Default::default() });
        finish_credit(&c, 1, &result(j.id, 0, 30)).unwrap(); // a resent result of an earlier chunking
        assert_eq!((left(&c), queue(&c)), (Some(70), vec![(30, 100)]));
        finish_credit(&c, 1, &result(j.id, 20, 50)).unwrap(); // overlaps (0,30): dropped, its gap 30..50 is queued already
        assert_eq!((left(&c), queue(&c)), (Some(70), vec![(30, 100)]));
        finish_credit(&c, 1, &result(j.id, 0, 30)).unwrap(); // exact duplicate
        assert_eq!(left(&c), Some(70));
        finish_credit(&c, 1, &result(j.id, 50, 70)).unwrap(); // clean: stored; worker 2's 60..80 is cut, 70..80 stays to do
        assert_eq!(left(&c), Some(50));
        assert!(c.sh.0.lock().unwrap().workers[&2].inflight.is_empty());
        assert_eq!(queue(&c), vec![(70, 100), (30, 50)], "queue holds exactly the uncovered numbers: 30..50 and 70..100");
        assert_eq!(queue(&c).iter().map(|r| r.1 - r.0).sum::<u64>(), 50);
        // credit: a result nobody was sent a TASK for (fed used up) gets none
        assert!(!finish_credit(&c, 1, &result(j.id, 30, 50)).unwrap());
        assert_eq!(left(&c), Some(30));
    }
}

// ---- wave 12: alerts and per-minute samples
fn observe(ctx: &Ctx, cpu: Option<f64>) -> (crate::research::Obs, crate::store::Sample) {
    let (standbys, lag) = {
        let s = ctx.repl.status.lock().unwrap();
        (s.standbys.len(), s.standbys.iter().map(|x| x.2).max().unwrap_or(0))
    };
    let reserves = ctx.store.reserves();
    let st = ctx.sh.0.lock().unwrap();
    let mut obs = crate::research::Obs { head_cpu: cpu, standby_lag: lag, standbys, target_standbys: ha_cfg(ctx).target_standbys as usize, ..Default::default() };
    let (mut online, mut busy, mut rate, mut energy) = (0u32, 0u32, 0.0, 0.0);
    for w in st.workers.values().filter(|w| can_work(w)) {
        online += 1;
        energy += w.energy_j;
        if !w.inflight.is_empty() {
            busy += 1;
            rate += w.rate;
            if w.rate > 0.0 {
                let (id, host) = who_of(w);
                obs.rates.push((if id.is_empty() { w.addr.clone() } else { id }, host, w.rate));
            }
        }
    }
    if !reserves.is_empty() {
        let views = node_views(&st);
        for r in reserves {
            let Ok(sel) = parse_sel(&r.selector) else { continue };
            let have = views.iter().filter(|n| n.takes_tasks && sel.matches(&n.attrs)).count() as u32;
            if have < r.keep_n {
                obs.reserves_short.push(format!("reserve {} ({}) holds {have} of {}", r.id, r.selector, r.keep_n));
            }
        }
    }
    (obs, crate::store::Sample { ts_ms: now_ms(), online, busy, rate, energy_j: energy })
}

fn research_loop(ctx: Arc<Ctx>) {
    ctx.store.alerts_close_stale(now_ms());
    let mut eng = crate::research::Engine::default();
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::get_current_pid().ok();
    let (mut losses, mut tick) = (HashMap::new(), 0u64);
    loop {
        thread::sleep(Duration::from_secs(5));
        tick += 1;
        if !primary(&ctx) {
            continue;
        }
        let cpu = pid.and_then(|p| {
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
            sys.process(p).map(|x| x.cpu_usage() as f64)
        });
        if tick % 12 == 1 {
            losses = losses_24h(&ctx);
        }
        let (mut obs, sample) = observe(&ctx, cpu);
        obs.losses = losses.clone();
        for ev in eng.step(&ctx.store, now_ms(), &crate::research::conditions(&obs, &crate::research::Cfg::now())) {
            use crate::research::Ev;
            let (row, kind) = match ev {
                Ev::Open(r) => (r, Some("alert.open")),
                Ev::Update(r) => (r, None),
                Ev::Close(r) => (r, Some("alert.close")),
            };
            if let Some(k) = kind {
                event(&ctx, k, "head", &format!("{} {}{}: {}", k.trim_start_matches("alert."), row.kind, row.node_id.as_deref().map_or(String::new(), |n| format!(" (node {n})")), row.detail));
            }
            ctx.bus.publish("alert", &serde_json::to_string(&row).unwrap_or_default());
        }
        if tick % 12 == 0 || tick == 2 {
            let _ = ctx.store.add_sample(&sample);
        }
    }
}
