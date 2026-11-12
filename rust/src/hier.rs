//! Root side of the two-level hierarchy (docs/HIERARCHY.md): lease bookkeeping for lab heads, the controller that picks and
//! moves lab heads (manual pins or auto), the orphan timer for silent lab heads, and the data behind GET /api/v1/hierarchy.
//!
//! A lab head (lab.rs) connects like a worker with Hello.lab_head set. The root's normal dispatch gives it big chunks (leases,
//! head.rs `lease_len`) of jobs that `Job.lease_ok` allows. This file handles what is new around that: the lab head's
//! status, the list of leases it holds after a reconnect, `lease_done`, and the 1 s controller tick.
//! Lock order: head State, then H (this file's controller memory). The tick never holds both while sending.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::head::*;
use crate::jobs::{now_ms, Ctx};
use crate::proto::*;

#[derive(Default)]
struct H {
    tried: HashMap<String, Instant>,   // node id -> last start_lab_head sent (also the failed ones)
    started: HashMap<String, Instant>, // lab -> last start_lab_head sent
    moved: HashMap<String, Instant>,   // PC node id -> last move_to sent
    released: HashSet<String>,         // lab head node ids sent lab_release (mode off, paused, drained)
    exiting: HashMap<String, Instant>, // lab head node ids told to exit (moved to another PC)
    members: HashMap<String, HashSet<String>>, // lab -> PCs last reported under its lab head (kept while it is released)
    keys: HashMap<String, usize>,      // lab head node id -> node keys sent
    paused: Option<String>,
    mode_seen: u8,
    standby: HashMap<String, (String, Instant)>, // lab -> (host node id of its chosen standby lab head, when chosen)
    alt_sent: HashMap<String, (String, Instant)>, // lab -> last lab_alt sent to its lab head (key, when)
}

fn h() -> MutexGuard<'static, H> {
    static S: OnceLock<Mutex<H>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// 0 off, 1 manual, 2 auto (tunable hier_mode).
pub fn mode() -> u8 {
    crate::tune::n("hier_mode").min(2) as u8
}

pub fn mode_name(m: u8) -> &'static str {
    ["off", "manual", "auto"][m.min(2) as usize]
}

/// Lab heads get leases now: the mode is not off and no job that lab heads cannot serve is waiting for PCs.
pub fn on() -> bool {
    mode() > 0 && h().paused.is_none()
}

/// The PC behind a node id: a lab head's id is "<PC>/lab-<lab>", a virtual PC's is "<PC>/v<i>".
fn base(n: &str) -> &str {
    n.split('/').next().unwrap_or(n)
}

/// Is this lab head the standby of its lab? A standby gets no leases (head.rs `lab_ok`) and no PCs until the lab head is gone.
pub(crate) fn is_standby(w: &Worker) -> bool {
    lab_of(w).is_some_and(|l| h().standby.get(&l.lab).is_some_and(|s| s.0 == base(&nkey(w))))
}

/// A lab head of `lab` left the root. Forgetting the standby entry is the takeover: the standby lab head (if connected) is
/// now the lab's head and gets its leases at once; if the standby itself left, the controller picks another. A duplicate lab
/// head leaving (`primary_left` false, not the standby) keeps the standby as it is.
pub(crate) fn lab_head_gone(lab: &str, node: &str, primary_left: bool) {
    let mut g = h();
    if primary_left || g.standby.get(lab).is_some_and(|s| s.0 == base(node)) {
        g.standby.remove(lab);
    }
}

pub fn start(ctx: Arc<Ctx>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        tick(&ctx);
    });
}

// ------------------------------------------------------------------ settings in store meta "hier"

#[derive(Default, Clone)]
struct Cfg {
    pins: std::collections::BTreeMap<String, String>, // lab -> node id
    drain: std::collections::BTreeSet<String>,
}

fn cfg(ctx: &Ctx) -> Cfg {
    let v: Value = ctx.store.meta("hier").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Cfg {
        pins: v["pins"].as_object().map(|o| o.iter().filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string()))).collect()).unwrap_or_default(),
        drain: v["drain"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
    }
}

fn save(ctx: &Ctx, c: &Cfg) -> Result<(), String> {
    ctx.store.set_meta("hier", &json!({"pins": c.pins, "drain": c.drain}).to_string())
}

pub fn pin(ctx: &Ctx, lab: &str, node: &str, actor: &str) -> Result<Value, String> {
    if lab.is_empty() || lab.len() > 64 {
        return Err("lab name must be 1-64 characters".into());
    }
    if ctx.store.node(node).is_none() {
        return Err(format!("no such node {node}"));
    }
    let mut c = cfg(ctx);
    c.pins.insert(lab.into(), node.into());
    save(ctx, &c)?;
    event(ctx, "hier.pin", actor, &format!("lab {lab}: lab head pinned to node {node}"));
    Ok(json!({"lab": lab, "pinned": node}))
}

pub fn unpin(ctx: &Ctx, lab: &str, actor: &str) -> Result<Value, String> {
    let mut c = cfg(ctx);
    if c.pins.remove(lab).is_none() {
        return Err(format!("lab {lab} has no pinned lab head"));
    }
    save(ctx, &c)?;
    event(ctx, "hier.unpin", actor, &format!("lab {lab}: lab head unpinned"));
    Ok(json!({"lab": lab, "pinned": null}))
}

pub fn drain(ctx: &Ctx, lab: &str, on: bool, actor: &str) -> Result<Value, String> {
    let mut c = cfg(ctx);
    if on {
        c.drain.insert(lab.into());
    } else {
        c.drain.remove(lab);
    }
    save(ctx, &c)?;
    event(ctx, "hier.drain", actor, &format!("lab {lab}: drain {}", if on { "on: no new leases, PCs return to the root when the held ones finish" } else { "off" }));
    Ok(json!({"lab": lab, "drain": on}))
}

// ------------------------------------------------------------------ CTRL from a lab head

/// Called for every CTRL a worker sends, before the normal handling. Only lab heads use these keys.
pub fn lab_ctrl(ctx: &Ctx, wid: u64, v: &Value) {
    let is_lab = ctx.sh.0.lock().unwrap().workers.get(&wid).is_some_and(|w| lab_of(w).is_some());
    if !is_lab {
        return;
    }
    if let Some(s) = v.get("lab_status") {
        let mut st = ctx.sh.0.lock().unwrap();
        if let Some(w) = st.workers.get_mut(&wid) {
            let mut s = s.clone();
            s["received_ms"] = now_ms().into();
            if let Some(t) = s["threads"].as_u64() {
                w.threads = t.max(1) as u32;
            }
            if let Some(c) = s["calib_mhs"].as_f64().filter(|c| *c > 0.0) {
                w.calib = c;
            }
            w.lab_pcs = s["pcs"].as_array().map(|a| a.iter().filter_map(|p| p["id"].as_str().map(String::from)).collect()).unwrap_or_default();
            let lab = lab_of(w).map(|l| l.lab.clone()).unwrap_or_default();
            for p in s["pcs"].as_array().into_iter().flatten().take(5000) {
                // a PC that never joined the root directly still gets a node row, so lists and counts include it
                if let Some(id) = p["id"].as_str().filter(|id| ctx.store.node(id).is_none()) {
                    let _ = ctx.store.upsert_node(id, p["host"].as_str().unwrap_or(""), "via lab", "{}", now_ms());
                    let _ = ctx.store.set_node_state(id, "active", "seen through a lab head", "head", now_ms());
                }
            }
            if !w.lab_pcs.is_empty() {
                h().members.insert(lab, w.lab_pcs.iter().cloned().collect());
            }
            w.lab_stat = s;
        }
    }
    if let Some(l) = v["leases"].as_array() {
        let ls: Vec<(u64, u64, u64, bool)> = l.iter().filter_map(|x| Some((x[0].as_u64()?, x[1].as_u64()?, x[2].as_u64()?, x[3].as_u64()? == 1))).collect();
        lab_leases(ctx, wid, &ls);
    }
    if let Some(d) = v.get("lease_done") {
        if let (Some(j), Some(a), Some(b)) = (d["job"].as_u64(), d["a"].as_u64(), d["b"].as_u64()) {
            lease_done(ctx, wid, j, a, b, d["secs"].as_f64().unwrap_or(0.0), d["rate"].as_f64().unwrap_or(0.0));
        }
    }
    if let Some(d) = v.get("lease_decline") {
        // the lab has no PC that may run this job (selector): the range goes back to the queue and the job stops being offered to it
        if let (Some(j), Some(a), Some(b)) = (d["job"].as_u64(), d["a"].as_u64(), d["b"].as_u64()) {
            let mut st = ctx.sh.0.lock().unwrap();
            let out = st.workers.get_mut(&wid).map(|w| {
                drop_within(w, j, a, b);
                w.declined.insert(j);
                w.out.clone()
            });
            if st.jobs.contains_key(&j) {
                let g = gaps(a, b, &ctx.store.done_in(j, a, b));
                requeue_missing(&mut st, j, g);
            }
            ctx.sh.1.notify_all();
            drop(st);
            flush(out.flatten().map(|o| vec![(o, ACK, pack(&[j, a, b]))]).unwrap_or_default());
        }
    }
    if let Some(l) = v["lab_head_failed"].as_str() {
        event(ctx, "hier.recruit", "head", &format!("a PC could not start a lab head: {l}"));
    }
}

fn node_of(w: &Worker) -> String {
    nkey(w)
}

/// `rate` = numbers per second the whole lab finished lately (0 = unknown: fall back to this lease over `secs`).
fn lease_done(ctx: &Ctx, wid: u64, job: u64, a: u64, b: u64, secs: f64, rate: f64) {
    let mut st = ctx.sh.0.lock().unwrap();
    let kind = st.jobs.get(&job).map(|j| j.kind);
    let mut upd = None;
    let mut sends: Sends = vec![];
    if let Some(w) = st.workers.get_mut(&wid) {
        drop_within(w, job, a, b);
        w.chunks += 1;
        w.leases_done += 1;
        let r = if rate > 0.0 { rate } else if secs > 0.0 { (b - a) as f64 / secs } else { 0.0 };
        if r > 0.0 {
            w.secs += secs;
            w.rate = r;
            upd = kind.map(|k| ((nkey(w), k), r));
        }
        sends.extend(w.out.clone().map(|o| (o, ACK, pack(&[job, a, b]))));
    }
    if let Some((key, x)) = upd {
        let r = st.rates.entry(key).or_insert(0.0);
        *r = ewma(*r, x);
    }
    // the lab says it sent every piece; if the root lacks some (a piece dropped from its bounded buffer), run them again
    if st.jobs.contains_key(&job) {
        let missing = gaps(a, b, &ctx.store.done_in(job, a, b));
        if !missing.is_empty() {
            requeue_missing(&mut st, job, missing);
        }
    }
    ctx.sh.1.notify_all();
    drop(st);
    flush(sends);
    ctx.bus.publish("event", &json!({"id": 0, "ts_ms": now_ms(), "kind": "labhead.lease", "actor": "head", "detail": format!("job {job} lease {a}..{b} done")}).to_string());
}

/// A lab head (re)connected and lists the leases it holds: (job, a, b, finished). Re-attach the ones the root still has
/// as orphans, take back the ones it can claim, tell the lab to drop the rest.
pub(crate) fn lab_leases(ctx: &Ctx, wid: u64, leases: &[(u64, u64, u64, bool)]) {
    let mut st = ctx.sh.0.lock().unwrap();
    let Some(me) = st.workers.get(&wid).map(node_of) else { return };
    let lab = st.workers.get(&wid).and_then(|w| lab_of(w).map(|l| l.lab.clone())).unwrap_or_default();
    let mut have: Vec<(u64, u64, u64)> = vec![];
    let healed = if let Some(o) = st.orphans.remove(&me) {
        have.extend(o.leases);
        true
    } else {
        false
    };
    // an older connection of the same lab head that the root has not timed out yet
    let olds: Vec<u64> = st.workers.iter().filter(|(i, w)| **i != wid && lab_of(w).is_some() && node_of(w) == me).map(|(i, _)| *i).collect();
    for i in olds {
        if let Some(w) = st.workers.get_mut(&i) {
            have.extend(std::mem::take(&mut w.inflight));
            w.sent.clear();
            if let Some(o) = &w.out {
                let _ = o.lock().unwrap().s.0.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    let mut sends: Sends = vec![];
    let out = st.workers.get(&wid).and_then(|w| w.out.clone());
    let mut adopted = 0;
    for &(j, a, b, fin) in leases {
        if !st.jobs.contains_key(&j) {
            sends.extend(out.clone().map(|o| (o, CANCEL_RANGE, pack(&[j, a, b]))));
            continue;
        }
        let gaps_ = gaps(a, b, &ctx.store.done_in(j, a, b));
        if fin {
            if let Some(w) = st.workers.get_mut(&wid) {
                drop_within(w, j, a, b); // it says the lease is complete: nothing of it is in flight any more
            }
            if !gaps_.is_empty() {
                requeue_missing(&mut st, j, gaps_);
            }
            sends.extend(out.clone().map(|o| (o, ACK, pack(&[j, a, b]))));
            continue;
        }
        let mut mine = vec![];
        for (x, y) in gaps_ {
            let others = st.workers.iter().any(|(i, w)| *i != wid && w.inflight.iter().any(|t| t.0 == j && t.1 < y && x < t.2));
            if others {
                continue; // the root gave it to someone else while the lab was away: first result wins, the lab drops its copy
            }
            let q = &mut st.jobs.get_mut(&j).unwrap().requeue;
            *q = q.drain(..).flat_map(|r| subtract(r, (x, y))).collect();
            q.sort_by(|p, r| r.cmp(p));
            mine.push((j, x, y));
        }
        if mine.is_empty() {
            sends.extend(out.clone().map(|o| (o, CANCEL_RANGE, pack(&[j, a, b]))));
        }
        if let Some(w) = st.workers.get_mut(&wid) {
            for t in mine {
                w.inflight.push(t);
                w.sent.insert(t, Instant::now());
                adopted += 1;
            }
        }
    }
    // leases the root kept for this lab that it no longer lists: back to the queue
    for t in have {
        let listed = leases.iter().any(|l| l.0 == t.0 && l.1 <= t.1 && t.2 <= l.2);
        if !listed && st.jobs.contains_key(&t.0) {
            let g = gaps(t.1, t.2, &ctx.store.done_in(t.0, t.1, t.2));
            requeue_missing(&mut st, t.0, g);
        }
    }
    ctx.sh.1.notify_all();
    drop(st);
    flush(sends);
    if healed {
        event(ctx, "labhead.heal", "head", &format!("lab head of lab {lab} is back: {adopted} lease piece(s) re-attached, its un-ACKed results were resent first"));
    }
}

// ------------------------------------------------------------------ controller

fn root_addr() -> String {
    format!("{}:{}", local_ip().map_or("127.0.0.1".into(), |i| i.to_string()), port())
}

/// Orphaned leases whose lab head stayed silent past lab_silence_s go back to the queue.
pub(crate) fn expire_orphans(ctx: &Ctx) {
    let limit = crate::tune::secs("lab_silence_s");
    let mut st = ctx.sh.0.lock().unwrap();
    let gone: Vec<String> = st.orphans.iter().filter(|(_, o)| o.since.elapsed() > limit).map(|(k, _)| k.clone()).collect();
    for k in gone {
        let Some(o) = st.orphans.remove(&k) else { continue };
        let mut n = 0;
        for (j, a, b) in &o.leases {
            if st.jobs.contains_key(j) {
                let g = gaps(*a, *b, &ctx.store.done_in(*j, *a, *b));
                requeue_missing(&mut st, *j, g);
                n += 1;
            }
        }
        ctx.sh.1.notify_all();
        drop(st);
        event(ctx, "labhead.leave", "head", &format!("lab head of lab {} was silent for {} s: {n} lease(s) given back to the queue", o.lab, limit.as_secs()));
        st = ctx.sh.0.lock().unwrap();
    }
    if st.orphans.is_empty() {
        return;
    }
    // jobs that finished or were cancelled: nothing to wait for
    let jobs: HashSet<u64> = st.jobs.keys().copied().collect();
    for o in st.orphans.values_mut() {
        o.leases.retain(|l| jobs.contains(&l.0));
    }
    st.orphans.retain(|_, o| !o.leases.is_empty());
}

struct LabHead {
    wid: u64,
    node: String,
    lab: String,
    addr: String,
    port: u16,
    out: Option<Arc<Mutex<Out>>>,
    inflight: usize,
    standby: bool,
    pcs: usize,
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Port a lab head started by the root listens on: the lab_head_port base plus 10 per lab (hash of its name, 50 slots).
pub fn lab_port(lab: &str) -> u16 {
    (crate::tune::n("lab_head_port") + 10 * (fnv(lab) % 50)).min(65_000) as u16
}

fn tick(ctx: &Arc<Ctx>) {
    expire_orphans(ctx);
    if !primary(ctx) {
        return;
    }
    let mode = mode();
    // pause while a job runs that lab heads cannot serve (--only jobs name exact PCs): its PCs must reach it directly
    let blocked: Option<String> = {
        let st = ctx.sh.0.lock().unwrap();
        st.jobs.values().find(|j| j.has_work() && !j.lease_ok).map(|j| format!("job {} cannot use lab heads (--only names exact PCs)", j.id))
    };
    let (labs, direct): (Vec<LabHead>, Vec<(String, String, Option<Arc<Mutex<Out>>>, bool)>) = {
        let st = ctx.sh.0.lock().unwrap();
        let labs = st.workers.iter().filter_map(|(wid, w)| {
            let l = lab_of(w)?;
            Some(LabHead { wid: *wid, node: node_of(w), lab: l.lab.clone(), addr: w.addr.clone(), port: l.port, out: w.out.clone(), inflight: w.inflight.len(), standby: is_standby(w), pcs: w.lab_pcs.len() })
        });
        let direct = st.workers.values().filter(|w| lab_of(w).is_none() && w.hello.is_some()).map(|w| (node_of(w), w.addr.clone(), w.out.clone(), w.hello.as_ref().is_some_and(|h| h.virtual_node)));
        (labs.collect(), direct.collect())
    };
    {
        let mut g = h();
        if g.paused != blocked && mode > 0 {
            match &blocked {
                Some(m) => event(ctx, "hier.pause", "head", &format!("lab heads paused, PCs go back to the root: {m}")),
                None => event(ctx, "hier.resume", "head", "lab heads resumed"),
            }
        }
        g.paused = if mode > 0 { blocked } else { None };
        if g.mode_seen != mode {
            event(ctx, "hier.mode", "head", &format!("hierarchy mode {}", mode_name(mode)));
            g.mode_seen = mode;
        }
    }
    let root = root_addr();
    let c = cfg(ctx);
    let active = on();
    // lab_pcs of a lab head that vanished no longer count
    for l in &labs {
        let want_release = !active || c.drain.contains(&l.lab);
        let was = h().released.contains(&l.node);
        if let Some(o) = &l.out {
            if want_release && !was {
                // recall the leases (the lab drops them), send its PCs home
                recall(ctx, l.wid);
                ctrl_to(o, json!({"lab_release": root}));
                h().released.insert(l.node.clone());
                event(ctx, "hier.release", "head", &format!("lab {}: PCs sent back to the root ({})", l.lab, if !active { "hierarchy off or paused" } else { "drain" }));
            } else if want_release && was && !active {
                recall(ctx, l.wid);
            } else if !want_release && was {
                ctrl_to(o, json!({"lab_adopt": true}));
                h().released.remove(&l.node);
            }
        }
        let mut st = ctx.sh.0.lock().unwrap();
        if let Some(w) = st.workers.get_mut(&l.wid) {
            w.drain = c.drain.contains(&l.lab);
            if w.drain && w.inflight.is_empty() && !was {
                drop(st);
                if let Some(o) = &l.out {
                    ctrl_to(o, json!({"lab_release": root}));
                }
                h().released.insert(l.node.clone());
            }
        }
    }
    h().released.retain(|n| labs.iter().any(|l| &l.node == n));
    if !active {
        return;
    }
    // secured fleets: the lab head admits its PCs with the node keys the root holds
    if !fleet_open() {
        let keys = ctx.store.node_keys();
        for l in &labs {
            if h().keys.get(&l.node) != Some(&keys.len()) {
                if let Some(o) = &l.out {
                    let list: Vec<Value> = keys.iter().filter_map(|(n, k)| Some(json!({"node": n, "host": ctx.store.node(n)?.host, "key": k.iter().map(|b| format!("{b:02x}")).collect::<String>()}))).collect();
                    ctrl_to(o, json!({"lab_keys": list}));
                    h().keys.insert(l.node.clone(), keys.len());
                }
            }
        }
    }
    // PCs by lab: the store's lab seats, plus whoever a lab head reported under it
    let rows = ctx.store.nodes();
    let lab_names: HashMap<u64, String> = ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect();
    let mut lab_of_node: HashMap<String, String> = rows.iter().filter_map(|r| Some((r.node_id.clone(), lab_names.get(&r.lab_id?)?.clone()))).collect();
    for (lab, ids) in h().members.iter() {
        for id in ids {
            lab_of_node.entry(id.clone()).or_insert_with(|| lab.clone());
        }
    }
    let mut names: std::collections::BTreeSet<String> = lab_of_node.values().cloned().collect();
    names.extend(c.pins.keys().cloned());
    names.extend(labs.iter().map(|l| l.lab.clone()));
    let min_pcs = crate::tune::n("lab_head_min_pcs") as usize;
    for lab in names {
        // the lab head is the one serving the most PCs (after a takeover that is the new one), not the standby on a tie, then the oldest connection
        let head = labs.iter().filter(|l| l.lab == lab && !h().released.contains(&l.node)).max_by_key(|l| (l.pcs, !l.standby, std::cmp::Reverse(l.wid)));
        if head.is_some_and(|hd| hd.standby) {
            // the "standby" is serving the lab: it is the head now, a new standby is chosen next tick
            h().standby.remove(&lab);
            event(ctx, "hier.takeover", "head", &format!("lab {lab}: {} serves the lab's PCs and is its lab head", head.map_or("", |hd| hd.node.as_str())));
            continue;
        }
        // a standby holds no leases: any it got while it served the lab go back to the root
        for sb in labs.iter().filter(|l| l.lab == lab && l.standby && l.inflight > 0) {
            recall(ctx, sb.wid);
            event(ctx, "hier.recall", "head", &format!("lab {lab}: standby {} held {} lease(s), returned to the root", sb.node, sb.inflight));
        }
        // two lab heads for one lab (an old one came back after its standby took over): the other one leaves
        for extra in labs.iter().filter(|l| l.lab == lab && !l.standby && head.is_some_and(|hd| hd.wid != l.wid) && !h().released.contains(&l.node)) {
            let due = h().exiting.get(&extra.node).is_none_or(|t| t.elapsed() > Duration::from_secs(30)); // own statement: an `if let` would hold h() over the body
            if let (Some(o), true) = (&extra.out, due) {
                recall(ctx, extra.wid);
                ctrl_to(o, json!({"lab_release": root}));
                ctrl_to(o, json!({"lab_exit": true}));
                h().exiting.insert(extra.node.clone(), Instant::now());
                event(ctx, "hier.dup", "head", &format!("lab {lab}: second lab head {} leaves, {} stays", extra.node, head.map_or("", |hd| hd.node.as_str())));
            }
        }
        let members: Vec<&(String, String, Option<Arc<Mutex<Out>>>, bool)> = direct.iter().filter(|d| lab_of_node.get(&d.0) == Some(&lab)).collect();
        let pinned = c.pins.get(&lab);
        // a pinned lab head on the wrong PC: ask it to leave
        if let (Some(hd), Some(p)) = (head, pinned) {
            if hd.node.split('/').next() != p.split('/').next() && h().exiting.get(&hd.node).is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                if let Some(o) = &hd.out {
                    recall(ctx, hd.wid);
                    ctrl_to(o, json!({"lab_release": root}));
                    ctrl_to(o, json!({"lab_exit": true}));
                    h().exiting.insert(hd.node.clone(), Instant::now());
                    event(ctx, "hier.move", "head", &format!("lab {lab}: lab head leaves for the pinned PC {p}"));
                }
            }
        }
        match head {
            Some(hd) if !c.drain.contains(&lab) => {
                // send the lab's PCs that sit at the root to the lab head (and tell them where the standby is)
                let target = format!("{}:{}", hd.addr, hd.port);
                let sb = standby_tick(ctx, &lab, hd, &labs, &lab_of_node);
                for (id, _, out, _) in &members {
                    let due = h().moved.get(id).is_none_or(|t| t.elapsed() > Duration::from_secs(8));
                    if let (true, Some(o)) = (due, out) {
                        ctrl_to(o, json!({"move_to": target, "alt": sb}));
                        h().moved.insert(id.clone(), Instant::now());
                    }
                }
            }
            Some(_) => {}
            None => {
                // no lab head for this lab: pick one when the mode and the lab size call for it
                let want = pinned.cloned().or_else(|| (mode == 2 && members.len() >= min_pcs).then(|| best_pc(ctx, &members)).flatten());
                let Some(node) = want else { continue };
                if h().started.get(&lab).is_some_and(|t| t.elapsed() < Duration::from_secs(20)) || h().tried.get(&node).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                    continue;
                }
                let Some(out) = direct.iter().find(|d| d.0 == node).and_then(|d| d.2.clone()) else { continue };
                let code = (!fleet_open()).then(|| join_code(ctx));
                let parents = parent_list(ctx);
                ctrl_to(&out, json!({"start_lab_head": {"lab": lab, "parent": parents, "port": lab_port(&lab), "join_code": code}}));
                h().started.insert(lab.clone(), Instant::now());
                h().tried.insert(node.clone(), Instant::now());
                event(ctx, "hier.recruit", "head", &format!("asked node {node} to start the lab head of lab {lab} ({} PCs)", members.len()));
            }
        }
    }
}

/// Standby of a lab (setting `lab_head_standby`): keep one second lab head on another PC of the lab, idle until the first is gone.
/// Returns the standby's "ip:port" for the lab's PCs (they try it before the root when their lab head dies), None when there is none.
fn standby_tick(ctx: &Ctx, lab: &str, hd: &LabHead, labs: &[LabHead], lab_of_node: &HashMap<String, String>) -> Option<String> {
    let sb_w = labs.iter().find(|l| l.lab == lab && l.standby);
    if crate::tune::get("lab_head_standby") <= 0.0 {
        // switched off: the standby leaves; its entry is dropped when its connection is
        if let Some((s, o)) = sb_w.and_then(|s| Some((s, s.out.as_ref()?))) {
            if h().exiting.get(&s.node).is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                ctrl_to(o, json!({"lab_exit": true}));
                h().exiting.insert(s.node.clone(), Instant::now());
            }
        } else {
            h().standby.remove(lab);
        }
        return None;
    }
    let alt = sb_w.map(|s| format!("{}:{}", s.addr, s.port));
    // the PCs of the lab hear about the standby through their lab head (and again every 15 s: PCs that joined later)
    let key = format!("{}|{}", hd.node, alt.as_deref().unwrap_or(""));
    if h().alt_sent.get(lab).is_none_or(|(k, t)| *k != key || t.elapsed() > Duration::from_secs(15)) {
        if let Some(o) = &hd.out {
            ctrl_to(o, json!({"lab_alt": alt}));
            h().alt_sent.insert(lab.into(), (key, Instant::now()));
        }
    }
    if sb_w.is_some() {
        return alt;
    }
    // chosen but never came up: choose again
    if h().standby.get(lab).is_some_and(|s| s.1.elapsed() > Duration::from_secs(40)) {
        h().standby.remove(lab);
    }
    if h().standby.contains_key(lab) || h().started.get(&format!("{lab}#standby")).is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
        return None;
    }
    let ids: Vec<&str> = lab_of_node.iter().filter(|(_, l)| l.as_str() == lab).map(|(n, _)| n.as_str()).collect();
    let allow_virtual = std::env::var("CK_HIER_ALLOW_VIRTUAL").is_ok_and(|v| v == "1");
    let tried = |id: &str| h().tried.get(id).is_some_and(|t| t.elapsed() < Duration::from_secs(60));
    let node = pick(cand_list(ctx, &crate::select::Selector::default()), &ids, Some(base(&hd.node)), tried, allow_virtual)?;
    let o = hd.out.as_ref()?;
    // the PC sits behind the lab head, so the start order is relayed through it; the standby listens 5 ports above the lab head's
    let code = (!fleet_open()).then(|| join_code(ctx));
    let start = json!({"start_lab_head": {"lab": lab, "parent": parent_list(ctx), "port": lab_port(lab) + 5, "join_code": code}});
    ctrl_to(o, json!({"relay": {"node": node, "ctrl": start}}));
    h().standby.insert(lab.into(), (base(&node).into(), Instant::now()));
    h().started.insert(format!("{lab}#standby"), Instant::now());
    h().tried.insert(node.clone(), Instant::now());
    event(ctx, "hier.recruit", "head", &format!("asked node {node} to start the standby lab head of lab {lab}"));
    None
}

/// A one-use join code for a secured fleet (the lab head the root starts needs one to be admitted).
fn join_code(ctx: &Ctx) -> String {
    let code: String = crate::auth::nonce()[..8].iter().map(|b| format!("{b:02X}")).collect();
    let _ = ctx.store.add_join_code(&crate::users::sha256_hex(code.as_bytes()), 1, now_ms() + 120_000, true, "head", now_ms());
    code
}

/// Best candidate among `ids` (a lab's PCs): eligible, not on the PC `not_base`, not tried lately; highest head score first.
/// Lab head choice: `not_base` None. Standby choice: the primary's PC, so it is the second best.
fn pick(mut c: Vec<Cand>, ids: &[&str], not_base: Option<&str>, tried: impl Fn(&str) -> bool, allow_virtual: bool) -> Option<String> {
    c.retain(|c| ids.contains(&c.id.as_str()) && (c.why.is_empty() || (allow_virtual && c.why == "virtual PC")) && c.role == "candidate" && !tried(&c.id) && not_base.is_none_or(|b| base(&c.id) != b));
    c.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    c.first().map(|c| c.id.clone())
}

/// Root address first, then the standbys (they answer there after a takeover).
fn parent_list(ctx: &Ctx) -> String {
    let mut v = vec![root_addr()];
    let sb = crate::repl::standby_nodes(ctx);
    let st = ctx.sh.0.lock().unwrap();
    for w in st.workers.values() {
        if w.hello.as_ref().is_some_and(|h| sb.contains(&h.node_id)) {
            v.push(format!("{}:{}", w.addr, port()));
        }
    }
    v.join(",")
}

/// The best PC of a lab as a lab head: the existing head-suitability ranking over the lab's PCs.
fn best_pc(ctx: &Ctx, members: &[&(String, String, Option<Arc<Mutex<Out>>>, bool)]) -> Option<String> {
    let allow_virtual = std::env::var("CK_HIER_ALLOW_VIRTUAL").is_ok_and(|v| v == "1");
    let ids: Vec<&str> = members.iter().map(|m| m.0.as_str()).collect();
    pick(cand_list(ctx, &crate::select::Selector::default()), &ids, None, |id| h().tried.get(id).is_some_and(|t| t.elapsed() < Duration::from_secs(60)), allow_virtual)
}

/// Take a lab head's leases back: they go to the queue, the lab is told to drop them.
fn recall(ctx: &Ctx, wid: u64) {
    let mut st = ctx.sh.0.lock().unwrap();
    let Some(w) = st.workers.get_mut(&wid) else { return };
    let gone = std::mem::take(&mut w.inflight);
    (w.sent.clear(), w.staged = None);
    let out = w.out.clone();
    for t in &gone {
        if st.jobs.contains_key(&t.0) {
            let g = gaps(t.1, t.2, &ctx.store.done_in(t.0, t.1, t.2));
            requeue_missing(&mut st, t.0, g);
        }
    }
    ctx.sh.1.notify_all();
    drop(st);
    flush(out.map(|o| gone.iter().map(|t| (o.clone(), CANCEL_RANGE, pack(&[t.0, t.1, t.2]))).collect()).unwrap_or_default());
}

// ------------------------------------------------------------------ API

/// GET /api/v1/hierarchy
pub fn json(ctx: &Ctx) -> Value {
    let c = cfg(ctx);
    let (mode, active, paused) = (mode(), on(), h().paused.clone());
    let st = ctx.sh.0.lock().unwrap();
    let rows = ctx.store.nodes();
    let lab_names: HashMap<u64, String> = ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect();
    let mut lab_of_node: HashMap<String, String> = rows.iter().filter_map(|r| Some((r.node_id.clone(), lab_names.get(&r.lab_id?)?.clone()))).collect();
    let members = h().members.clone();
    for (lab, ids) in members.iter() {
        for id in ids {
            lab_of_node.entry(id.clone()).or_insert_with(|| lab.clone());
        }
    }
    let direct: Vec<&Worker> = st.workers.values().filter(|w| lab_of(w).is_none()).collect();
    let mut names: std::collections::BTreeSet<String> = lab_of_node.values().cloned().collect();
    names.extend(c.pins.keys().cloned());
    names.extend(st.workers.values().filter_map(|w| lab_of(w).map(|l| l.lab.clone())));
    let released = h().released.clone();
    let now = now_ms();
    let labs: Vec<Value> = names.iter().map(|lab| {
        let hw = st.workers.values().find(|w| lab_of(w).is_some_and(|l| &l.lab == lab) && !is_standby(w));
        let sb = h().standby.get(lab).cloned();
        let sbw = st.workers.values().find(|w| lab_of(w).is_some_and(|l| &l.lab == lab) && is_standby(w));
        let standby = sb.map(|(n, since)| json!({
            "node_id": sbw.map_or_else(|| n.clone(), nkey), "pc": n, "ready": sbw.is_some(), "host": sbw.and_then(|w| w.hello.as_ref()).map(|h| h.host.as_str()),
            "addr": sbw.map(|w| format!("{}:{}", w.addr, lab_of(w).map_or(0, |l| l.port))), "chosen_s": since.elapsed().as_secs_f64(),
        }));
        let orphan = st.orphans.iter().find(|(_, o)| &o.lab == lab);
        let head = hw.map(|w| {
            let s = &w.lab_stat;
            let granted: u64 = w.inflight.iter().map(|t| t.2 - t.1).sum();
            let age = s["received_ms"].as_i64().map_or(f64::NAN, |t| (now - t) as f64 / 1000.0);
            let l = lab_of(w).unwrap();
            json!({
                "node_id": node_of(w), "host": w.hello.as_ref().map(|h| h.host.as_str()), "addr": w.addr, "port": l.port, "api": l.api,
                "connected_s": w.joined.map(|t| t.elapsed().as_secs_f64()), "pcs": s["pcs"].as_array().map_or(0, |a| a.len()), "threads": w.threads,
                "leases": s["leases"], "root_inflight": w.inflight.iter().map(|t| json!({"job": t.0, "a": t.1, "b": t.2})).collect::<Vec<_>>(),
                "granted_in_flight": granted, "lab_granted": s["granted"], "lab_done": s["done"], "leases_done": w.leases_done,
                "lag_s": if age.is_nan() { Value::Null } else { age.into() }, "unacked": s["unacked"], "partitioned": s["partitioned"].as_bool().unwrap_or(false),
                "released": released.contains(&node_of(w)), "rate": w.rate, "chunks_inflight": s["chunks_inflight"],
            })
        });
        let pcs: Vec<Value> = match hw {
            Some(w) if w.lab_stat["pcs"].is_array() => w.lab_stat["pcs"].as_array().cloned().unwrap_or_default(),
            _ => vec![],
        };
        let at_root = direct.iter().filter(|w| w.hello.as_ref().is_some_and(|h| lab_of_node.get(&h.node_id) == Some(lab))).count();
        let state = match (&head, orphan) {
            (_, Some(_)) => "partitioned",
            (Some(h), _) if h["released"] == true => "released",
            (Some(_), _) => "active",
            (None, _) if c.pins.contains_key(lab) || mode == 2 => "absent",
            _ => "none",
        };
        json!({"lab": lab, "pinned": c.pins.get(lab), "drain": c.drain.contains(lab), "state": state, "head": head, "pcs": pcs, "pcs_at_root": at_root, "standby": standby,
               "silent_s": orphan.map(|(_, o)| o.since.elapsed().as_secs_f64()), "orphaned_leases": orphan.map(|(_, o)| o.leases.len())})
    }).collect();
    let rs = crate::repl::status(ctx);
    let unleasable: Vec<u64> = st.jobs.values().filter(|j| j.has_work() && !j.lease_ok).map(|j| j.id).collect();
    json!({
        "mode": mode_name(mode), "active": active, "paused": paused,
        "settings": {"hier_mode": mode, "lease_s": crate::tune::get("lease_s"), "lab_head_min_pcs": crate::tune::get("lab_head_min_pcs"), "lab_silence_s": crate::tune::get("lab_silence_s"), "lab_head_standby": crate::tune::get("lab_head_standby") > 0.0},
        "root": {"node_id": crate::identity::node_id(), "role": rs.role, "epoch": rs.epoch, "addr": root_addr(), "pcs_direct": direct.len()},
        "standbys": rs.standbys.iter().map(|s| s.0.clone()).collect::<Vec<_>>(),
        "labs": labs,
        "orphans": st.orphans.iter().map(|(n, o)| json!({"node_id": n, "lab": o.lab, "leases": o.leases.len(), "silent_s": o.since.elapsed().as_secs_f64(),
            "expires_in_s": (crate::tune::get("lab_silence_s") - o.since.elapsed().as_secs_f64()).max(0.0)})).collect::<Vec<_>>(),
        "unleasable_jobs": unleasable,
    })
}

/// What the advisor needs from `json(ctx)`: every lab with its PC count (at the root plus behind its lab head).
pub fn advice_labs(v: &Value) -> Vec<(String, usize)> {
    v["labs"].as_array().into_iter().flatten().map(|l| {
        (l["lab"].as_str().unwrap_or("").to_string(), l["pcs_at_root"].as_u64().unwrap_or(0) as usize + l["head"]["pcs"].as_u64().unwrap_or(0) as usize)
    }).collect()
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use crate::jobs::{JobSpec, JobState};
    use crate::store::Store;

    fn ctx() -> Ctx {
        Ctx { sh: Arc::default(), store: Arc::new(Store::memory()), bus: Default::default(), repl: Default::default() }
    }
    fn lab_worker(c: &Ctx, wid: u64, node: &str) {
        let h = Hello { node_id: node.into(), host: "lab-a".into(), virtual_node: true, lab_head: Some(LabHello { lab: "A".into(), port: 1, api: 3, pcs: 4 }), ..Default::default() };
        c.sh.0.lock().unwrap().workers.insert(wid, Worker { hello: Some(h), threads: 8, state: "active".into(), ..Default::default() });
    }
    fn result(job: u64, a: u64, b: u64) -> Vec<u8> {
        let mut p = pack(&[job, a, b, 0, 0, 0, 0]);
        p.extend_from_slice(&(b - a).to_le_bytes());
        p
    }
    /// A job over 0..100 whose whole range is leased to worker `wid` (what take() + feed() do, minus the socket).
    fn leased(c: &Ctx, wid: u64) -> u64 {
        let j = crate::head::submit(c, JobSpec { name: "t".into(), kind: KIND_SYNTH, from: 0, end: 100, chunk: 10, arg: vec![0; 8], submitter: "t".into(), ..Default::default() }).unwrap();
        let mut st = c.sh.0.lock().unwrap();
        st.jobs.get_mut(&j.id).unwrap().requeue.clear();
        let w = st.workers.get_mut(&wid).unwrap();
        w.inflight.push((j.id, 0, 100));
        w.sent.insert((j.id, 0, 100), Instant::now());
        j.id
    }
    fn inflight(c: &Ctx, wid: u64) -> Vec<(u64, u64, u64)> {
        let mut v = c.sh.0.lock().unwrap().workers[&wid].inflight.clone();
        v.sort();
        v
    }
    fn left(c: &Ctx, job: u64) -> u64 {
        c.sh.0.lock().unwrap().jobs.get(&job).map_or(0, |j| j.left)
    }

    #[test]
    fn grant_partial_return_dedup_and_completion() {
        let c = ctx();
        lab_worker(&c, 1, "lab1");
        let job = leased(&c, 1);
        finish_part(&c, 1, &result(job, 0, 40), true).unwrap();
        assert_eq!((inflight(&c, 1), left(&c, job)), (vec![(job, 40, 100)], 60), "lease shrinks to what is left");
        finish_part(&c, 1, &result(job, 0, 40), true).unwrap(); // resent after a reconnect
        assert_eq!(left(&c, job), 60, "a repeated piece counts once");
        finish_part(&c, 1, &result(job, 70, 100), true).unwrap(); // pieces arrive in any order
        assert_eq!(inflight(&c, 1), vec![(job, 40, 70)]);
        finish_part(&c, 1, &result(job, 40, 70), true).unwrap();
        assert!(inflight(&c, 1).is_empty());
        assert_eq!(c.store.job(job).unwrap().state, JobState::Done, "every piece stored: the job is done");
        assert_eq!(c.store.parts(job).iter().map(|p| p.1 - p.0).sum::<u64>(), 100);
    }

    #[test]
    fn silent_lab_keeps_leases_then_gives_them_back() {
        let c = ctx();
        lab_worker(&c, 1, "lab1");
        let job = leased(&c, 1);
        finish_part(&c, 1, &result(job, 0, 40), true).unwrap();
        // the connection times out: leases are kept as an orphan entry (what serve_worker does)
        {
            let mut st = c.sh.0.lock().unwrap();
            let gone = st.workers.remove(&1).unwrap().inflight;
            st.orphans.insert("lab1".into(), Orphan { since: Instant::now(), lab: "A".into(), leases: gone });
        }
        expire_orphans(&c);
        assert_eq!(c.sh.0.lock().unwrap().orphans.len(), 1, "still within lab_silence_s");
        assert!(c.sh.0.lock().unwrap().jobs[&job].requeue.is_empty());
        c.sh.0.lock().unwrap().orphans.get_mut("lab1").unwrap().since = Instant::now().checked_sub(Duration::from_secs(4000)).unwrap();
        expire_orphans(&c);
        let st = c.sh.0.lock().unwrap();
        assert!(st.orphans.is_empty());
        assert_eq!(st.jobs[&job].requeue, vec![(40, 100)], "the unreported part is queued again; the stored part is not");
    }

    #[test]
    fn reconnect_reattaches_orphans_and_drops_finished_or_unknown_leases() {
        let c = ctx();
        lab_worker(&c, 1, "lab1");
        let job = leased(&c, 1);
        finish_part(&c, 1, &result(job, 0, 40), true).unwrap();
        {
            let mut st = c.sh.0.lock().unwrap();
            let gone = st.workers.remove(&1).unwrap().inflight;
            st.orphans.insert("lab1".into(), Orphan { since: Instant::now(), lab: "A".into(), leases: gone });
        }
        lab_worker(&c, 2, "lab1"); // the same lab head, new connection
        lab_leases(&c, 2, &[(job, 0, 100, false), (999, 0, 5, false)]);
        assert!(c.sh.0.lock().unwrap().orphans.is_empty());
        assert_eq!(inflight(&c, 2), vec![(job, 40, 100)], "only the part not stored yet is re-attached");
        // a finished lease whose pieces the root lacks is queued again
        lab_leases(&c, 2, &[(job, 0, 100, true)]);
        assert_eq!(c.sh.0.lock().unwrap().jobs[&job].requeue, vec![(40, 100)]);
    }

    #[test]
    fn lease_done_updates_rate_and_clears_the_lease() {
        let c = ctx();
        lab_worker(&c, 1, "lab1");
        let job = leased(&c, 1);
        finish_part(&c, 1, &result(job, 0, 90), true).unwrap();
        lease_done(&c, 1, job, 0, 100, 0.0, 50.0);
        let st = c.sh.0.lock().unwrap();
        assert!(st.workers[&1].inflight.is_empty() && st.workers[&1].leases_done == 1);
        assert_eq!(st.rates.get(&("lab1".to_string(), KIND_SYNTH)).copied(), Some(50.0), "the lab's measured speed sizes its next lease");
    }

    #[test]
    fn lease_sizing_and_eligibility() {
        let c = ctx();
        lab_worker(&c, 1, "lab1");
        lab_worker(&c, 2, "lab2");
        let mut st = c.sh.0.lock().unwrap();
        st.rates.insert(("lab1".into(), KIND_SYNTH), 1000.0);
        let w = &st.workers[&1];
        assert!(!crate::head::lab_ok(w, &fake_job(true)), "hierarchy off: no leases");
        // 30 s x 1,000/s = 30,000, but at most half of what is left per lab (1,000,000 / 4) and never under one second of work
        assert_eq!(crate::head::lease_len(&st, w, KIND_SYNTH, 1_000_000, 10), 30_000);
        assert_eq!(crate::head::lease_len(&st, w, KIND_SYNTH, 40_000, 10), 10_000, "guided cap");
        assert_eq!(crate::head::lease_len(&st, w, KIND_SYNTH, 1_500, 10), 1_000, "the tail is not cut into slivers");
        assert_eq!(crate::head::lease_len(&st, w, KIND_SYNTH, 500, 10), 500);
    }

    fn fake_job(ok: bool) -> crate::head::Job {
        crate::head::Job { id: 1, kind: KIND_SYNTH, arg: vec![], next: 0, end: 10, chunk: 1, total: 10, left: 10, todo: 10, requeue: vec![], only: vec![], started: Instant::now(),
            selector: Default::default(), priority: 2, user: String::new(), policy: crate::sched::ChunkPolicy::Adaptive, max_pcs: 0, placement: String::new(), mem_mb: 0, blobs: vec![],
            blob_mode: String::new(), shared: vec![], shards: vec![], item_sha: vec![], item_base: 0, selector_src: String::new(), lease_ok: ok, chunk_set: 0 }
    }

    #[test]
    fn old_hello_has_no_lab_head() {
        let old = br#"{"node_id":"n","host":"h"}"#;
        assert!(serde_json::from_slice::<Hello>(old).unwrap().lab_head.is_none());
        let pc = Hello { node_id: "n".into(), ..Default::default() };
        assert!(!serde_json::to_string(&pc).unwrap().contains("lab_head"), "a PC sends nothing new");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_ports_are_stable_and_in_range() {
        assert_eq!(lab_port("A"), lab_port("A"));
        for l in ["A", "lab-b", "x y", ""] {
            let p = lab_port(l) as u32;
            assert!((7710..7710 + 500).contains(&p), "{l}: {p}");
        }
    }

    fn cand(id: &str, score: f64, why: &str) -> Cand {
        Cand { id: id.into(), host: id.into(), lab: "L".into(), score, parts: [0.0; 4], role: "candidate", why: why.into() }
    }

    #[test]
    fn standby_is_the_second_best_pc_of_the_lab() {
        let c = || vec![cand("pcA", 0.9, ""), cand("pcB", 0.8, ""), cand("pcC", 0.7, "asleep"), cand("pcD/v1", 0.6, "virtual PC"), cand("other", 0.99, "")];
        let ids = ["pcA", "pcB", "pcC", "pcD/v1"];
        assert_eq!(pick(c(), &ids, None, |_| false, false).as_deref(), Some("pcA"), "lab head: the best of the lab, not of the fleet");
        assert_eq!(pick(c(), &ids, Some("pcA"), |_| false, false).as_deref(), Some("pcB"), "standby: best PC that is not the lab head's");
        assert_eq!(pick(c(), &ids, Some(base("pcA/lab-X")), |_| false, false).as_deref(), Some("pcB"), "base() strips the lab head suffix");
        assert_eq!(pick(c(), &ids, Some("pcA"), |id| id == "pcB", false), None, "tried lately, asleep and virtual PCs are skipped");
        assert_eq!(pick(c(), &ids, Some("pcA"), |id| id == "pcB", true).as_deref(), Some("pcD/v1"), "virtual PCs only when allowed");
        assert_eq!(pick(c(), &ids, Some("pcD"), |id| id == "pcA", true).as_deref(), Some("pcB"), "the lab head's own PC is excluded for all its virtual agents");
    }

    fn lab_head(lab: &str, node: &str) -> Worker {
        let h = Hello { node_id: node.into(), lab_head: Some(LabHello { lab: lab.into(), port: 1, api: 3, pcs: 0 }), ..Default::default() };
        Worker { hello: Some(h), state: "active".into(), ..Default::default() }
    }

    #[test]
    fn standby_takes_over_when_the_lab_head_leaves() {
        let (lab, other) = ("sbLabT", "sbLabU");
        let (primary, standby) = (lab_head(lab, "pcA/lab-sbLabT"), lab_head(lab, "pcB/lab-sbLabT"));
        h().standby.insert(lab.into(), ("pcB".into(), Instant::now()));
        h().standby.insert(other.into(), ("pcB".into(), Instant::now()));
        assert!(is_standby(&standby), "the chosen PC's lab head is the standby: no leases (head.rs lab_ok)");
        assert!(!is_standby(&primary));
        assert!(!is_standby(&lab_head(other, "pcA/lab-sbLabU")) && is_standby(&lab_head(other, "pcB/lab-sbLabU")), "per lab");
        lab_head_gone(lab, "pcC/lab-sbLabT", false); // a duplicate lab head left; the primary is still there
        assert!(is_standby(&standby), "a duplicate leaving keeps the standby");
        lab_head_gone(lab, "pcA/lab-sbLabT", true); // the primary's connection closed
        assert!(!is_standby(&standby), "promoted at once: it is the lab's head and gets leases");
        assert!(is_standby(&lab_head(other, "pcB/lab-sbLabU")), "other labs keep their standby");
        h().standby.remove(other);
    }

    #[test]
    fn mode_reads_the_tunable() {
        assert_eq!(mode(), 0, "off by default: a root with no lab heads behaves as before");
        assert!(!on());
    }
}
