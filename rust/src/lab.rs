//! Lab head: the lower level of the two-level hierarchy (docs/HIERARCHY.md, design and numbers in docs/sim/HIERARCHY.md).
//!
//! `ck head --parent ROOT[:port][,STANDBY..] [--lab NAME]` is a full head for its own PCs (same scheduler, chunking, results,
//! retries) that also joins the root like one big worker. The root hands it large ranges (leases, `lease_s` seconds of work); each
//! lease becomes a local "shadow" job over the same numbers, the lab's PCs run it as normal chunks, and the finished pieces
//! are merged and sent up as PARTIAL frames (a few per second, not one per chunk). When the shadow job is complete the lab says
//! `lease_done`. The root de-duplicates by range exactly as it does for any result, so a lab head that dies loses at most
//! what was in flight on its PCs.
//!
//! Partition tolerance: leases, finished pieces and the shadow jobs live here, not on the link. If the root is unreachable the
//! lab keeps computing what it was granted, never takes more, and on reconnect resends every un-ACKed piece and lists the
//! leases it holds (CTRL `leases`) so the root can re-attach them. Epochs: the lab refuses a root whose epoch is lower than
//! the highest it has seen (a stale root), and never fences itself (it has no beacon and no standby of its own).
//!
//! Lock order: head State, then LAB. Code here never takes the State lock while holding LAB.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::head::{recv_any, Out};
use crate::jobs::{now_ms, Ctx, JobSpec};
use crate::proto::*;
use crate::Opts;

/// The next queued lease starts when the running one has this many tenths of its numbers done: one lease at a time keeps the held
/// work a sawtooth between one and two leases, and the nine-tenths overlap hides the straggler tail of each.
const START_AT: u64 = 9;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CTX: OnceLock<Arc<Ctx>> = OnceLock::new();

/// A blob fetched from the root landed in this lab head's blob store: register it so its PCs can fetch it from here.
pub fn blob_landed(sha: &str, size: u64) {
    if let Some(c) = CTX.get() {
        c.store.put_blob(sha, size, now_ms());
    }
}

/// Where the PCs of local job `local` upload output files and bulk results: the root's upload capability for the leased job.
pub fn put_target(ctx: &Ctx, local: u64) -> Option<serde_json::Value> {
    if !active() {
        return None;
    }
    let user = ctx.sh.0.lock().unwrap().jobs.get(&local).map(|j| j.user.clone())?;
    let root: u64 = user.strip_prefix("lab:")?.parse().ok()?;
    let (host, port, token) = crate::blobs::put_for(root)?;
    let host = if host.is_empty() { lab().parent.rsplit_once(':').map(|p| p.0.to_string()).unwrap_or_default() } else { host };
    Some(serde_json::json!({"host": host, "port": port, "token": token}))
}

/// This process is a lab head (started with --parent).
pub fn active() -> bool {
    ACTIVE.load(Relaxed)
}

/// One lease: root range [a, b) of root job `job`, run locally as job `local` over [a, b) (list jobs: over [0, b - a), `off` = a).
struct Lease {
    kind: u8,
    local: u64,                            // 0 until the shadow job is started (leases run one after another, see start_ready)
    spec: Option<JobSpec>,                 // what start_ready submits
    shared: Option<Vec<String>>,           // the root's job-wide blobs (None = the job has no blobs)
    len: u64,
    got: u64,                              // numbers whose parts were stored locally
    pending: Vec<(u64, u64, Vec<u8>)>,     // stored locally, not yet batched upward
    files: BTreeMap<(u64, u64), Vec<(String, String, u64)>>, // CK_OUT files of those chunks (already uploaded to the root by the PCs)
    since: Instant,
    finished: bool,                        // every part is in `unacked` / sent; waiting for the root's ACK of the lease
    done_sent: bool,
}

#[derive(Default)]
struct LabState {
    leases: BTreeMap<(u64, u64, u64), Lease>,
    by_local: HashMap<u64, (u64, u64, u64)>,
    early: HashMap<u64, Vec<(u64, u64, Vec<u8>)>>, // parts that arrived before their lease was registered
    expecting: u32,                                // submits in progress (parts of unknown jobs are kept only then)
    unacked: BTreeMap<(u64, u64, u64), Vec<u8>>,   // PARTIAL payloads the root has not ACKed, resent after a reconnect
    unacked_files: BTreeMap<(u64, u64, u64), Vec<u8>>, // the CTRL {files} that goes right before such a PARTIAL
    up: Option<Arc<Mutex<Out>>>,                   // the live uplink
    down_since: Option<Instant>,
    outstanding: u32, // REQs sent, no TASK back yet
    meta: HashMap<u64, serde_json::Value>,
    epoch: u64, // highest root epoch seen
    drain: bool,
    released: bool,
    done_total: u64,                 // numbers stored locally, all leases
    rate_win: Option<(Instant, u64)>, // throughput window: start, done_total at start (open while a lease is unfinished)
    parent: String,
    sent_partials: u64,
    leases_done: u64,
    leases_total: u64,
}

/// Wakes the streamer when a lease has all its parts (it then reports at once instead of at the next batch tick).
static WAKE: Condvar = Condvar::new();

fn lab() -> MutexGuard<'static, LabState> {
    static L: OnceLock<Mutex<LabState>> = OnceLock::new();
    L.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ pure helpers (unit tested below)

/// Combine the bodies of adjacent parts (ascending) into the body of their union: sha lists concatenate (the reducer sorts),
/// synth counts add, every other kind concatenates in range order. The root stores the result as one part.
pub fn merge_body(kind: u8, parts: &[(u64, u64, Vec<u8>)]) -> Vec<u8> {
    match kind {
        KIND_SYNTH => parts.iter().map(|p| u64::from_le_bytes(p.2.get(..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 8]))).sum::<u64>().to_le_bytes().to_vec(),
        _ => parts.iter().flat_map(|p| p.2.iter().copied()).collect(),
    }
}

/// Sort parts and group the ones that touch (b == next a) into runs of at most `max_bytes` of body. Returns (a, b, bodies).
pub fn group_runs(mut parts: Vec<(u64, u64, Vec<u8>)>, max_bytes: usize) -> Vec<(u64, u64, Vec<(u64, u64, Vec<u8>)>)> {
    parts.sort_by_key(|p| p.0);
    let mut out: Vec<(u64, u64, Vec<(u64, u64, Vec<u8>)>)> = vec![];
    let mut bytes = 0usize;
    for p in parts {
        match out.last_mut() {
            Some(r) if r.1 == p.0 && bytes + p.2.len() <= max_bytes => {
                r.1 = p.1;
                bytes += p.2.len();
                r.2.push(p);
            }
            _ => {
                bytes = p.2.len();
                out.push((p.0, p.1, vec![p]));
            }
        }
    }
    out
}

/// A shadow job's spec from a TASK payload (after the 24-byte range header): the same numbers, the same kind and argument, and
/// what the root told us about the job (`meta`: priority, mem_mb, chunk, selector, max_pcs, blobs). List jobs carry their items
/// after a 0x00 in the argument; the shadow job stores them from index `a`, so item numbers stay the root's.
pub fn shadow_spec(job: u64, a: u64, b: u64, kind: u8, arg: &[u8], meta: &serde_json::Value) -> JobSpec {
    let mut s = JobSpec {
        name: format!("lease of job {job} {a}..{b}"), kind, from: a, end: b, submitter: format!("lab:{job}"),
        priority: meta["priority"].as_u64().unwrap_or(2) as u8, mem_mb: meta["mem_mb"].as_u64().unwrap_or(0), chunk: meta["chunk"].as_u64().unwrap_or(0),
        selector: meta["selector"].as_str().unwrap_or("").to_string(), max_pcs: meta["max_pcs"].as_u64().unwrap_or(0) as u32,
        placement: meta["placement"].as_str().unwrap_or("").to_string(),
        blobs: meta["blobs"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect(), ..Default::default()
    };
    if kind == KIND_LIST {
        let (cmd, items) = match arg.iter().position(|c| *c == 0) {
            Some(i) => (&arg[..i], &arg[i + 1..]),
            None => (arg, &[][..]),
        };
        s.arg = cmd.to_vec();
        s.items = String::from_utf8_lossy(items).split('\n').map(String::from).collect();
        s.item_base = a;
        return s;
    }
    s.arg = arg.to_vec();
    s
}

/// Header of a PARTIAL payload (same layout as RESULT): job, a, b, cpu, mem used, mem total, compute us.
pub fn partial_payload(job: u64, a: u64, b: u64, body: &[u8]) -> Vec<u8> {
    let mut p = pack(&[job, a, b, 0, 0, 0, 0]);
    p.extend_from_slice(body);
    p
}

// ------------------------------------------------------------------ hook from head.rs

/// head::finish_inner stored a new part of a local job. If it belongs to a lease, queue it for the next batch.
pub fn on_part(local: u64, a: u64, b: u64, body: &[u8], files: Option<&[(String, String, u64)]>) {
    if !active() {
        return;
    }
    let mut g = lab();
    let g = &mut *g;
    if let Some(k) = g.by_local.get(&local).copied() {
        if let Some(l) = g.leases.get_mut(&k) {
            let before = l.got;
            l.got += b - a;
            l.pending.push((a, b, body.to_vec()));
            if let Some(f) = files.filter(|f| !f.is_empty()) {
                l.files.insert((a, b), f.to_vec());
            }
            g.done_total += b - a;
            // done, or past the point where the next queued lease may start: wake the streamer
            if l.got >= l.len || (before * 10 < l.len * START_AT && l.got * 10 >= l.len * START_AT) {
                WAKE.notify_one();
            }
        }
    } else if g.expecting > 0 {
        g.early.entry(local).or_default().push((a, b, body.to_vec()));
    }
}

// ------------------------------------------------------------------ start

/// Called by head::run in lab mode: drop zombie shadow jobs from a previous life, then start the uplink and the streamer.
pub fn start(ctx: Arc<Ctx>, o: &Opts) {
    ACTIVE.store(true, Relaxed);
    let _ = CTX.set(ctx.clone());
    let name = o.lab.clone().unwrap_or_else(|| o.name.clone());
    // a restarted lab head must not resume ranges the root has long since re-granted
    for r in ctx.store.unfinished() {
        let _ = crate::head::cancel_job(&ctx, r.id, "lab");
    }
    let parents: Vec<String> = o.parent.clone().unwrap_or_default().split(',').map(|p| p.trim()).filter(|p| !p.is_empty()).map(|p| if p.contains(':') { p.to_string() } else { format!("{p}:7700") }).collect();
    crate::say!("lab.start", "lab head for '{name}' under {}", parents.join(", "));
    let (c1, n1, p1) = (ctx.clone(), name.clone(), parents.clone());
    thread::spawn(move || uplink(c1, n1, p1));
    thread::spawn(move || streamer(ctx, name));
}

fn uplink(ctx: Arc<Ctx>, lab_name: String, parents: Vec<String>) {
    let (mut i, mut fails) = (0usize, 0u32);
    loop {
        let addr = parents[i % parents.len()].clone();
        let began = Instant::now();
        match session(&ctx, &addr, &lab_name) {
            Ok(()) => {}
            Err(e) => crate::warnl!("lab.uplink", "lost root {addr}: {e}"),
        }
        {
            let mut g = lab();
            g.up = None;
            g.outstanding = 0;
            g.down_since.get_or_insert_with(Instant::now);
        }
        i += 1; // the next address in --parent (a standby that took over answers there)
        fails = if began.elapsed() > Duration::from_secs(10) { 0 } else { (fails + 1).min(4) };
        thread::sleep(Duration::from_millis(if fails == 0 { 200 } else { 250 << (fails - 1) }));
    }
}

fn session(ctx: &Arc<Ctx>, addr: &str, lab_name: &str) -> io::Result<()> {
    let mut s = crate::agent::connect(addr)?;
    s.set_read_timeout(Some(lost_after()))?;
    s.set_write_timeout(Some(lost_after().max(Duration::from_secs(10))))?;
    let node_id = format!("{}/lab-{lab_name}", crate::identity::node_id());
    let secure = !fleet_open();
    let kp = crate::identity::key_path(&node_id);
    let join_code = if secure && !kp.exists() { std::env::var("CK_JOIN_CODE").ok().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) } else { None };
    let (pcs, threads) = pcs_threads(ctx);
    let hello = Hello {
        node_id: node_id.clone(), host: crate::identity::host(), virtual_node: true, inv: crate::identity::inventory(), join_code, seen_proof: None,
        lab_head: Some(LabHello { lab: lab_name.to_string(), port: port(), api: dash_port(), pcs }),
    };
    send(&mut s, HELLO_W, &hello_payload(threads.max(1), &hello))?;
    let (mac_w, mut mac_r) = if secure { let (w, r) = crate::agent::admit(&mut s, &node_id, &kp)?; (Some(w), Some(r)) } else { (None, None) };
    if let Ok(pa) = s.peer_addr() {
        *crate::blobs::HEAD_IP.lock().unwrap() = pa.ip().to_string(); // blob sources that say "the head" mean the root
    }
    let sock = s.try_clone()?;
    let (mut r, w) = conn(s)?;
    let w = Arc::new(Mutex::new(Out::new(w, mac_w)));
    // everything that did not get through before goes first, then the list of leases held, then requests for more
    {
        let mut g = lab();
        g.parent = addr.to_string();
        g.up = None; // the streamer must not interleave frames until the replay is out
        let replay: Vec<(Option<Vec<u8>>, Vec<u8>)> = g.unacked.iter().map(|(k, p)| (g.unacked_files.get(k).cloned(), p.clone())).collect();
        let held: Vec<serde_json::Value> = g.leases.iter().map(|(k, l)| serde_json::json!([k.0, k.1, k.2, l.finished as u8])).collect();
        drop(g);
        let mut o = w.lock().unwrap();
        for (f, p) in &replay {
            if let Some(f) = f {
                o.put(CTRL, f)?;
            }
            o.put(PARTIAL, p)?;
        }
        if !held.is_empty() {
            o.put(CTRL, serde_json::json!({"leases": held}).to_string().as_bytes())?;
        }
    }
    {
        let mut g = lab();
        if g.down_since.take().is_some_and(|t| t.elapsed() > Duration::from_secs(2)) {
            crate::say!("lab.reconnect", "root {addr} reachable again: {} un-ACKed pieces resent", g.unacked.len());
        }
        g.up = Some(w.clone());
    }
    crate::blobs::report_have(&w, serde_json::json!({})); // the root stages a lease's blobs here before it hands the lease out
    top_up(ctx);
    let w2 = w.clone();
    thread::spawn(move || {
        while {
            thread::sleep(heartbeat_every());
            w2.lock().unwrap().beat().is_ok()
        } {}
    });
    let res: io::Result<()> = (|| -> io::Result<()> { loop {
        let (k, p) = match recv_any(&mut r, &mut mac_r) {
            Ok(x) => x,
            Err(e) => return Err(e),
        };
        match k {
            TASK => on_task(ctx, &p),
            CANCEL => {
                let job = u64_at(&p, 0)?;
                drop_leases(ctx, |k| k.0 == job);
            }
            CANCEL_RANGE => {
                let (j, a, b) = (u64_at(&p, 0)?, u64_at(&p, 8)?, u64_at(&p, 16)?);
                drop_leases(ctx, |k| *k == (j, a, b));
            }
            ACK => {
                let key = (u64_at(&p, 0)?, u64_at(&p, 8)?, u64_at(&p, 16)?);
                let mut g = lab();
                g.unacked.remove(&key);
                g.unacked_files.remove(&key);
                if g.leases.get(&key).is_some_and(|l| l.done_sent) {
                    g.leases.remove(&key);
                    g.by_local.retain(|_, v| *v != key);
                }
            }
            CTRL => ctrl(ctx, &p, &w)?,
            _ => {}
        }
    } })();
    let _ = sock.shutdown(std::net::Shutdown::Both);
    res
}

/// (PCs, threads) connected to this lab head.
fn pcs_threads(ctx: &Ctx) -> (u32, u32) {
    let st = ctx.sh.0.lock().unwrap();
    (st.workers.len() as u32, st.workers.values().map(|w| w.threads).sum())
}

fn on_task(ctx: &Arc<Ctx>, p: &[u8]) {
    let (Ok(job), Ok(a), Ok(b)) = (u64_at(p, 0), u64_at(p, 8), u64_at(p, 16)) else { return };
    let (kind, arg) = (p.get(24).copied().unwrap_or(KIND_SHA), p.get(25..).unwrap_or(&[]));
    let key = (job, a, b);
    let meta = lab().meta.get(&job).cloned().unwrap_or_default();
    let sel = meta["selector"].as_str().unwrap_or("").to_string();
    let serve = sel.is_empty() || serves(ctx, &sel); // takes the State lock: not while holding LAB
    {
        let mut g = lab();
        g.outstanding = g.outstanding.saturating_sub(1);
        if g.leases.contains_key(&key) {
            return; // resent TASK of a lease we already hold
        }
        if !serve {
            // none of this lab's PCs may run the job: give the range back and tell the root to stop offering it
            let up = g.up.clone();
            drop(g);
            if let Some(o) = up {
                let _ = o.lock().unwrap().put(CTRL, serde_json::json!({"lease_decline": {"job": job, "a": a, "b": b}}).to_string().as_bytes());
            }
            crate::say!("lab.decline", "lease of job {job} {a}..{b} declined: no PC of this lab matches '{sel}'");
            return;
        }
        let spec = shadow_spec(job, a, b, kind, arg, &meta);
        let dt = g.done_total;
        g.rate_win.get_or_insert((Instant::now(), dt));
        let shared = meta.get("shared").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        g.leases.insert(key, Lease { kind, local: 0, spec: Some(spec), shared, len: b - a, got: 0, pending: vec![], files: BTreeMap::new(), since: Instant::now(), finished: false, done_sent: false });
        g.leases_total += 1;
        crate::say!("lab.lease", "lease of job {job} {a}..{b} ({} numbers) queued", b - a);
    }
    start_ready(ctx);
}

/// Can any connected PC of this lab run a job with this selector?
fn serves(ctx: &Ctx, sel: &str) -> bool {
    let Ok(sel) = crate::select::parse(sel) else { return true };
    ctx.sh.0.lock().unwrap().workers.values().any(|w| crate::head::can_work(w) && sel.matches(&w.attrs))
}

/// Start the oldest queued lease as a local job unless the running one is still under nine tenths done.
fn start_ready(ctx: &Arc<Ctx>) {
    loop {
        let (key, spec, shared) = {
            let mut g = lab();
            if g.leases.values().any(|l| l.local != 0 && !l.finished && l.got * 10 < l.len * START_AT) {
                return;
            }
            let Some(key) = g.leases.iter().find(|(_, l)| l.local == 0 && !l.finished && l.spec.is_some()).map(|(k, _)| *k) else { return };
            let spec = g.leases.get_mut(&key).and_then(|l| l.spec.take()).unwrap();
            let shared = g.leases.get(&key).and_then(|l| l.shared.clone());
            g.expecting += 1;
            (key, spec, shared)
        };
        let r = crate::head::submit(ctx, spec);
        if let (Ok(row), Some(sh)) = (&r, shared) {
            crate::head::set_job_shared(ctx, row.id, sh); // State lock: before taking LAB
        }
        let mut g = lab();
        g.expecting -= 1;
        match r {
            Ok(row) => {
                let early = g.early.remove(&row.id).unwrap_or_default();
                g.by_local.insert(row.id, key);
                let mut add = 0;
                if let Some(l) = g.leases.get_mut(&key) {
                    l.local = row.id;
                    l.since = Instant::now();
                    for (x, y, body) in early {
                        l.got += y - x;
                        add += y - x;
                        l.pending.push((x, y, body));
                    }
                }
                g.done_total += add;
                crate::say!("lab.lease", "lease of job {} {}..{} started as local job {}", key.0, key.1, key.2, row.id);
            }
            Err(e) => {
                g.early.clear();
                g.leases.remove(&key);
                let up = g.up.clone();
                drop(g);
                crate::warnl!("lab.lease", "cannot run lease {} {}..{}: {e}", key.0, key.1, key.2);
                if let Some(o) = up {
                    let mut f = pack(&[key.0, key.1, key.2]);
                    f.push(0); // no point trying again: the root fails the job with this message
                    f.extend_from_slice(e.as_bytes());
                    let _ = o.lock().unwrap().put(FAILED, &f);
                }
            }
        }
    }
}

/// Forget the leases `pick` selects and cancel their shadow jobs (the root cancelled the job or took the range back).
fn drop_leases(ctx: &Arc<Ctx>, pick: impl Fn(&(u64, u64, u64)) -> bool) {
    let gone: Vec<(u64, u64, u64, u64)> = {
        let mut g = lab();
        let ks: Vec<_> = g.leases.keys().filter(|k| pick(k)).copied().collect();
        let mut v = vec![];
        for k in ks {
            if let Some(l) = g.leases.remove(&k) {
                g.by_local.remove(&l.local);
                v.push((k.0, k.1, k.2, l.local));
            }
        }
        g.unacked.retain(|k, _| !pick(k));
        g.unacked_files.retain(|k, _| !pick(k));
        v
    };
    for (j, a, b, local) in gone {
        if local != 0 {
            let _ = crate::head::cancel_job(ctx, local, "lab");
        }
        crate::say!("lab.lease_drop", "lease of job {j} {a}..{b} dropped (cancelled by the root)");
    }
}

fn ctrl(ctx: &Arc<Ctx>, p: &[u8], up: &Arc<Mutex<Out>>) -> io::Result<()> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(p) else { return Ok(()) };
    crate::blobs::agent_ctrl(&v, up); // fetch / allow / job_blobs / probe: this lab head is the root's blob client and then its lab's first source
    if let Some(t) = v.get("tune") {
        crate::tune::apply_remote(t);
        crate::head::tune_changed(ctx);
    }
    if let Some(e) = v["epoch"].as_u64() {
        let mut g = lab();
        if e < g.epoch {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("stale root: epoch {e} is below {}", g.epoch)));
        }
        g.epoch = e;
    }
    if let Some(j) = v.get("hier_job") {
        if let Some(id) = j["id"].as_u64() {
            let mut g = lab();
            if g.meta.len() > 1000 {
                g.meta.clear();
            }
            g.meta.insert(id, j.clone());
        }
    }
    if let Some(keys) = v["lab_keys"].as_array() {
        for k in keys {
            let (Some(n), Some(h), Some(key)) = (k["node"].as_str(), k["host"].as_str(), k["key"].as_str()) else { continue };
            let key: Vec<u8> = (0..key.len() / 2).filter_map(|i| u8::from_str_radix(&key[2 * i..2 * i + 2], 16).ok()).collect();
            let _ = ctx.store.upsert_node(n, h, "", "{}", now_ms());
            let _ = ctx.store.set_node_key(n, &key);
            if ctx.store.node(n).is_some_and(|r| r.state != "active") {
                let _ = ctx.store.set_node_state(n, "active", "synced from the root", "head", now_ms());
                crate::head::node_state_changed(ctx, n);
            }
        }
    }
    if let Some(a) = v.get("lab_alt") {
        crate::head::alt_all(ctx, a.as_str()); // the lab's standby lab head: the PCs try it first when this one dies
    }
    if let (Some(n), Some(c)) = (v["relay"]["node"].as_str(), v["relay"].get("ctrl")) {
        crate::head::relay_ctrl(ctx, n, c);
    }
    if v["lab_adopt"].as_bool() == Some(true) {
        let mut g = lab();
        (g.released, g.drain) = (false, false);
    }
    if let Some(d) = v["lab_drain"].as_bool() {
        lab().drain = d;
    }
    if let Some(addr) = v["lab_release"].as_str() {
        lab().released = true;
        crate::head::move_all(ctx, Some(addr));
        crate::say!("lab.release", "PCs sent back to {addr}");
    }
    if v["lab_exit"].as_bool() == Some(true) {
        crate::say!("lab.exit", "the root asked this lab head to exit");
        thread::spawn(|| {
            thread::sleep(Duration::from_millis(800));
            crate::log::flush();
            std::process::exit(0)
        });
    }
    Ok(())
}

/// Ask the root for as many leases as this lab can hold (pipeline) minus what it holds or has asked for.
fn top_up(ctx: &Arc<Ctx>) {
    let has_pcs = pcs_threads(ctx).0 > 0;
    let (up, n) = {
        let mut g = lab();
        let active = g.leases.values().filter(|l| !l.finished).count() as u32; // queued ones count: they are granted
        let want = (pipeline() as u32).saturating_sub(active + g.outstanding);
        if g.drain || g.released || g.up.is_none() || want == 0 || !has_pcs {
            return;
        }
        g.outstanding += want;
        (g.up.clone().unwrap(), want)
    };
    let mut o = up.lock().unwrap();
    for _ in 0..n {
        if o.put(REQ, &[]).is_err() {
            return;
        }
    }
}

/// Every lab_part_s: batch finished pieces upward, report finished leases, ask for more, send the status once a second.
fn streamer(ctx: Arc<Ctx>, _lab_name: String) {
    let mut last_status = Instant::now() - Duration::from_secs(2);
    loop {
        {
            let g = lab();
            let _ = WAKE.wait_timeout(g, crate::tune::secs("lab_part_s"));
        }
        // 1. batch pending pieces
        let mut frames: Vec<((u64, u64, u64), Vec<u8>, Option<Vec<u8>>)> = vec![];
        let mut finished: Vec<(u64, u64, u64, u64)> = vec![];
        // fz1: leases that finished while the link to the root was down (a failover): `finished` but never announced. The reconnect list only
        // makes the root ACK them, and the ACK of a lease the root was never told about is ignored (done_sent), so they stayed forever.
        let mut redo: Vec<(u64, u64, u64, u64)> = vec![];
        let mut failed: Vec<(u64, u64, u64, u64)> = vec![];
        {
            let mut g = lab();
            let keys: Vec<_> = g.leases.keys().copied().collect();
            for k in keys {
                let l = g.leases.get_mut(&k).unwrap();
                if l.finished {
                    if !l.done_sent && l.local != 0 {
                        redo.push((k.0, k.1, k.2, l.local));
                    }
                    continue;
                }
                let pend = std::mem::take(&mut l.pending);
                let kind = l.kind;
                // a chunk with output files travels alone: the files are listed per chunk and the root keys them by its range
                let (with, plain): (Vec<_>, Vec<_>) = pend.into_iter().partition(|p| l.files.contains_key(&(p.0, p.1)));
                for (a, b, parts) in group_runs(plain, 8 << 20) {
                    frames.push(((k.0, a, b), partial_payload(k.0, a, b, &merge_body(kind, &parts)), None));
                }
                for p in with {
                    let f = l.files.remove(&(p.0, p.1)).unwrap_or_default();
                    let ctl = serde_json::json!({"files": {"job": k.0, "a": p.0, "b": p.1, "files": f.iter().map(|(n, s, z)| serde_json::json!({"name": n, "sha": s, "size": z})).collect::<Vec<_>>()}}).to_string().into_bytes();
                    frames.push(((k.0, p.0, p.1), partial_payload(k.0, p.0, p.1, &merge_body(kind, &[p])), Some(ctl)));
                }
                if l.local != 0 && l.got >= l.len && l.pending.is_empty() {
                    l.finished = true;
                    finished.push((k.0, k.1, k.2, l.local));
                }
            }
            for (key, p, f) in &frames {
                g.unacked.insert(*key, p.clone());
                if let Some(f) = f {
                    g.unacked_files.insert(*key, f.clone());
                }
            }
            g.sent_partials += frames.len() as u64;
        }
        // shadow jobs that failed (a chunk ran out of retries): tell the root, which counts it against the job
        {
            let ls: Vec<_> = lab().leases.iter().filter(|(_, l)| !l.finished && l.local != 0).map(|(k, l)| (*k, l.local)).collect();
            for (k, local) in ls {
                if ctx.store.job(local).is_some_and(|r| r.state == crate::jobs::JobState::Failed) {
                    failed.push((k.0, k.1, k.2, local));
                }
            }
        }
        let up = lab().up.clone();
        if let Some(o) = &up {
            let mut o = o.lock().unwrap();
            let mut ok = true;
            for (_, p, f) in &frames {
                if let Some(f) = f {
                    ok &= o.put(CTRL, f).is_ok();
                }
                ok &= o.put(PARTIAL, p).is_ok();
            }
            // throughput of the whole lab since the window opened (leases run side by side, so one lease's own time says little)
            let rate = if finished.is_empty() { 0.0 } else { window_rate() };
            for (j, a, b, _) in finished.iter().chain(&redo) {
                ok &= o.put(CTRL, serde_json::json!({"lease_done": {"job": j, "a": a, "b": b, "secs": 0.0, "rate": rate}}).to_string().as_bytes()).is_ok();
                if let Some(l) = lab().leases.get_mut(&(*j, *a, *b)) {
                    l.done_sent = ok;
                }
            }
            for (j, a, b, local) in &failed {
                // the shadow job already retried (exit 75, timeouts) as the root would: report the failing chunk itself, final
                let msg = ctx.store.job(*local).and_then(|r| r.error).unwrap_or_else(|| "lease failed".into());
                let (x, y, msg) = split_failure(&msg).filter(|(x, y, _)| a <= x && y <= b).unwrap_or((*a, *b, msg.clone()));
                let mut f = pack(&[*j, x, y]);
                f.push(0);
                f.extend_from_slice(msg.as_bytes());
                ok &= o.put(FAILED, &f).is_ok();
            }
            drop(o);
            if !ok {
                lab().up = None; // the reader sees the broken link and reconnects
            }
        }
        if !failed.is_empty() {
            let mut g = lab();
            for (j, a, b, local) in failed {
                g.leases.remove(&(j, a, b));
                g.by_local.remove(&local);
            }
        }
        if !finished.is_empty() {
            let mut g = lab();
            g.leases_done += finished.len() as u64;
            // a finished lease that could not be announced stays until the reconnect list (`leases`, finished = 1) tells the root
        }
        start_ready(&ctx);
        top_up(&ctx);
        // 2. status
        if last_status.elapsed() >= Duration::from_secs(1) {
            last_status = Instant::now();
            let up = lab().up.clone();
            if let Some(o) = up {
                let _ = o.lock().unwrap().put(CTRL, status(&ctx).to_string().as_bytes());
            }
        }
    }
}

/// "chunk 4..9 failed: why" -> (4, 9, "why"), the shape head::chunk_failed writes.
pub fn split_failure(m: &str) -> Option<(u64, u64, String)> {
    let r = m.strip_prefix("chunk ")?;
    let (range, why) = r.split_once(" failed: ")?;
    let (x, y) = range.split_once("..")?;
    Some((x.parse().ok()?, y.parse().ok()?, why.to_string()))
}

/// Numbers per second the lab finished since the throughput window opened (0 = too short to tell). The window restarts
/// now if leases are still unfinished, and closes when none are.
fn window_rate() -> f64 {
    let mut g = lab();
    let Some((t0, n0)) = g.rate_win else { return 0.0 };
    let dt = t0.elapsed().as_secs_f64();
    if dt < 0.3 {
        return 0.0;
    }
    let rate = (g.done_total - n0) as f64 / dt;
    g.rate_win = g.leases.values().any(|l| !l.finished).then(|| (Instant::now(), g.done_total));
    rate
}

/// The lab head's report to the root and to its own API.
pub fn status(ctx: &Ctx) -> serde_json::Value {
    let (pcs, threads, calib, inflight) = {
        let st = ctx.sh.0.lock().unwrap();
        let pcs: Vec<serde_json::Value> = st.workers.values().take(5000).map(|w| serde_json::json!({
            "id": w.hello.as_ref().map(|h| h.node_id.as_str()).unwrap_or(&w.addr), "host": w.hello.as_ref().map(|h| h.host.as_str()).unwrap_or(""),
            "threads": w.threads, "busy": !w.inflight.is_empty(), "virtual": w.hello.as_ref().is_some_and(|h| h.virtual_node),
            "cpu": (w.cpu * 10.0).round() / 10.0, "mem_used": w.mem_used, "mem_total": w.mem_total})).collect();
        (pcs, st.workers.values().map(|w| w.threads as u64).sum::<u64>(), st.workers.values().map(|w| w.calib).sum::<f64>(), st.workers.values().map(|w| w.inflight.len()).sum::<usize>())
    };
    let g = lab();
    let granted: u64 = g.leases.iter().map(|(k, _)| k.2 - k.1).sum();
    let done: u64 = g.leases.values().map(|l| l.got).sum();
    serde_json::json!({"lab_status": {
        "pcs": pcs, "threads": threads, "calib_mhs": calib, "chunks_inflight": inflight,
        "leases": g.leases.iter().map(|(k, l)| serde_json::json!({"job": k.0, "a": k.1, "b": k.2, "got": l.got, "finished": l.finished, "age_s": l.since.elapsed().as_secs_f64()})).collect::<Vec<_>>(),
        "granted": granted, "done": done, "unacked": g.unacked.len(), "partials_sent": g.sent_partials, "leases_done": g.leases_done, "leases_total": g.leases_total,
        "partitioned": g.up.is_none(), "down_s": g.down_since.map_or(0.0, |t| t.elapsed().as_secs_f64()), "drain": g.drain, "released": g.released,
        "epoch": g.epoch, "parent": g.parent,
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u64) -> Vec<u8> {
        [n.to_le_bytes().to_vec(), vec![7; 32]].concat()
    }

    #[test]
    fn merge_sha_concat_synth_sum() {
        let parts = vec![(0, 10, rec(3)), (10, 20, [rec(12), rec(15)].concat()), (20, 30, vec![])];
        assert_eq!(merge_body(KIND_SHA, &parts).len(), 3 * 40);
        let s = vec![(0, 5, 5u64.to_le_bytes().to_vec()), (5, 9, 4u64.to_le_bytes().to_vec()), (9, 10, vec![])];
        assert_eq!(merge_body(KIND_SYNTH, &s), 9u64.to_le_bytes().to_vec());
        assert_eq!(merge_body(KIND_CMD, &[(0, 1, b"a\n".to_vec()), (1, 2, b"b\n".to_vec())]), b"a\nb\n");
        // merged then reduced equals reduced directly
        let direct = crate::reduce::reduce("sha", "", &parts).unwrap();
        let merged = crate::reduce::reduce("sha", "", &[(0, 30, merge_body(KIND_SHA, &parts))]).unwrap();
        assert_eq!(direct, merged);
    }

    #[test]
    fn runs_group_only_touching_parts() {
        let p = |a, b| (a, b, vec![0u8; 4]);
        let r = group_runs(vec![p(20, 30), p(0, 10), p(10, 20), p(40, 50), p(50, 60)], 1 << 20);
        assert_eq!(r.iter().map(|x| (x.0, x.1, x.2.len())).collect::<Vec<_>>(), vec![(0, 30, 3), (40, 60, 2)]);
        let small = group_runs(vec![p(0, 1), p(1, 2), p(2, 3)], 8);
        assert_eq!(small.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), vec![(0, 2), (2, 3)], "byte cap splits a run");
        assert!(group_runs(vec![], 8).is_empty());
    }

    #[test]
    fn shadow_spec_keeps_numbers_and_item_base() {
        let meta = serde_json::json!({"priority": 1, "mem_mb": 0, "chunk": 5, "selector": "gpu=true", "max_pcs": 3, "blobs": ["aa", "bb"]});
        let s = shadow_spec(7, 100, 200, KIND_SHA, b"ab", &meta);
        assert_eq!((s.from, s.end, s.item_base, s.kind, s.arg.as_slice(), s.priority, s.chunk), (100, 200, 0, KIND_SHA, &b"ab"[..], 1, 5));
        assert_eq!((s.selector.as_str(), s.max_pcs, s.blobs.len(), s.submitter.as_str()), ("gpu=true", 3, 2, "lab:7"));
        let mut arg = b"prog -x".to_vec();
        arg.push(0);
        arg.extend_from_slice(b"i1\ni2\ni3");
        let l = shadow_spec(7, 40, 43, KIND_LIST, &arg, &serde_json::json!({"mem_mb": 512}));
        assert_eq!((l.item_base, l.items.len(), l.arg.as_slice(), l.mem_mb), (40, 3, &b"prog -x"[..], 512));
    }

    #[test]
    fn chunk_failures_keep_their_range() {
        assert_eq!(split_failure("chunk 4..9 failed: boom: x"), Some((4, 9, "boom: x".into())));
        assert_eq!(split_failure("lease failed"), None);
    }

    #[test]
    fn partial_payload_is_a_result_frame() {
        let p = partial_payload(3, 10, 20, b"xyz");
        assert_eq!((u64_at(&p, 0).unwrap(), u64_at(&p, 8).unwrap(), u64_at(&p, 16).unwrap()), (3, 10, 20));
        assert_eq!(&p[RESULT_HDR..], b"xyz");
    }

    #[test]
    fn parts_before_registration_are_kept_only_while_submitting() {
        ACTIVE.store(true, Relaxed);
        on_part(9_000_001, 0, 1, b"x", None);
        assert!(!lab().early.contains_key(&9_000_001), "nobody expects it: dropped");
        lab().expecting += 1;
        on_part(9_000_002, 0, 1, b"x", None);
        lab().expecting -= 1;
        assert_eq!(lab().early.remove(&9_000_002).map(|v| v.len()), Some(1));
        ACTIVE.store(false, Relaxed);
    }
}
