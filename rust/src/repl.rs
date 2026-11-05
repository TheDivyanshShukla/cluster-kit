//! High availability (M4, decision D6): standby heads, the replicated change log, durable results, election, fencing.
//! Owner: a2. Contract: keep every pub item; agents a3 (head.rs), a4 (agent.rs, client.rs), a5 (api.rs) build against it.
//!
//! Roles. One primary (dispatches work). Any number of standbys: `ck head --standby --head PRIMARY_IP` on a PC that is
//! an admitted node (it proves its node key like an agent; with CK_FLEET=open it just says hello). A standby holds a
//! full copy of the head database and takes over when the primary goes silent.
//!
//! Wire (proto.rs): standby -> primary HELLO_S (Hello JSON + last applied seq u64); primary -> standby SNAPSHOT (the
//! whole database file, made with VACUUM INTO, sent once when the standby is behind the in-memory log), then REPL frames
//! (one Entry as JSON each), HEARTBEAT every second; standby -> primary REPL_ACK (seq u64) after it applied an entry.
//!
//! Change log. The head calls `record(ctx, kind, data)` after every store write that a takeover needs:
//!   "job"    {"id", "spec": JobSpec incl. items}         after create_job
//!   "part"   {"job", "a", "b", "body": [bytes]}           after add_part returned true
//!   "state"  {"job", "state", "error", "ms"}               after set_state
//!   "config" {"export": <api config export JSON>, "users", "tokens", "node_keys", "nodes"}  after any org/user/key change
//! Entries get seq 1, 2, 3... per epoch; the primary keeps the last 100,000 in memory for standbys that fall behind.
//!
//! Durable results (HA-04). A part is durable when it is in the local store AND (no standby connected OR one standby
//! acked a seq >= the part's seq). Then the head sends the agent ACK (job, a, b); the agent drops its copy. Agents keep
//! un-ACKed results and resend them (RESULT) to whatever head they reach next; add_part dedups.
//!
//! Election (HA-05). A standby whose primary link has been silent for LOST_AFTER + 2 s x priority (priority = its
//! position in the primary's standby list, 0 first) promotes itself: epoch + 1 stored in the database, then it runs as a
//! head (head::run) and beacons "e=<epoch>". It starts dispatching only once it has quorum: connected agents x 2 >
//! membership (nodes seen connected in the 60 s before the primary went silent, replicated as part of "config"); with
//! no known membership it dispatches at once. A primary that loses quorum for 10 s while it has standbys stops
//! dispatching (a network split leaves at most one side working).
//!
//! Implementation notes (a2).
//!  - hello entry: before any other frame the primary sends a REPL frame with Entry{seq:0, kind:"hello", data:{"priority",
//!    "peers":[ip..], "mode":"replay"|"snapshot", "last_seq"}}; re-sent when the standby's position changes. `peers` are the
//!    other standbys' IPs: a standby that lost the primary also tries them (one of them may have promoted).
//!  - HELLO_S payload = Hello JSON (u32 len + bytes), last applied seq u64, then (extra) the standby's epoch u64. Replay only when
//!    the epoch matches and the seq is inside the ring; otherwise SNAPSHOT (then entries with seq > snapshot seq).
//!  - After a restart the primary starts at stored last_seq + 100 with an empty ring (so every standby re-syncs by snapshot).
//!  - Membership: node ids connected in the last 60 s (`refresh`), stored in meta "membership" and replicated as a
//!    "membership" entry {"nodes":[..]} whenever it changes. A promoted standby is "candidate" (meta "candidate"=1) and keeps the
//!    replicated membership until a quorum of agents has connected.
//!  - Secured fleet: after admission the primary sends a sealed HEARTBEAT with a salt; every later frame is sealed and
//!    encrypted under `link_key(node key, salt)` (see Half). CK_FLEET=open: plain frames.
//!  - Port clash on one machine: a standby listens on nothing, so primary and standby can both use CK_PORT=7930; after the
//!    primary dies the promoted head binds the same port. `CK_PRIMARY_PORT` overrides the port a standby connects to.
//!
//! Fencing (HA-06). Every head beacons its epoch; agents and standbys follow the highest epoch they hear; a head that
//! hears a higher epoch than its own stops dispatching, closes its agents and re-runs itself as `--standby --head <it>`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::jobs::{now_ms, Ctx, JobSpec, JobState, NodeRow};
use crate::proto::{self, recv, send, u64_at, Hello};
use crate::store::Store;

const RING_MAX: usize = 100_000;
const MEMBER_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub epoch: u64,
    pub kind: String,
    pub data: serde_json::Value,
}

/// What /api/v1/ha shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// "primary" | "standby" | "candidate" (promoted, waiting for quorum)
    pub role: String,
    pub epoch: u64,
    pub last_seq: u64,
    /// standbys of this primary: (node_id, acked seq, lag in entries)
    pub standbys: Vec<(String, u64, u64)>,
    pub membership: usize,
    pub connected: usize,
    pub dispatching: bool,
    /// The primary's "ip:port" as a standby sees it; empty on a primary.
    #[serde(default)]
    pub primary: String,
}

struct Link {
    id: u64,
    node: String,
    peer: String,
    tx: mpsc::Sender<Entry>,
    acked: u64,
}

#[derive(Default)]
struct Inner {
    init: bool,
    epoch: u64,
    last: u64,
    persisted: u64,
    ring: VecDeque<Entry>,
    links: Vec<Link>, // order = standby priority
    next_link: u64,
    members: HashMap<String, Instant>,
    saved_members: String,
    connected: usize,
    candidate: bool,
}

/// Replication state inside Ctx.
#[derive(Default)]
pub struct Repl {
    pub status: Mutex<Status>,
    inner: Mutex<Inner>,
}

fn num(o: Option<String>) -> u64 {
    o.and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn sync_status(ctx: &Ctx, g: &Inner) {
    let mut s = ctx.repl.status.lock().unwrap();
    s.role = if g.candidate { "candidate" } else { "primary" }.into();
    s.epoch = g.epoch;
    s.last_seq = g.last;
    s.standbys = g.links.iter().map(|l| (l.node.clone(), l.acked, g.last.saturating_sub(l.acked))).collect();
    s.membership = g.members.len();
    s.connected = g.connected;
    s.dispatching = !g.candidate && has_quorum(g.connected, g.members.len());
    s.primary.clear();
}

/// Loads epoch, last_seq (+100: see header), membership and the candidate flag from the store once.
fn init(ctx: &Ctx) -> MutexGuard<'_, Inner> {
    let mut g = ctx.repl.inner.lock().unwrap();
    if !g.init {
        g.init = true;
        let st = &ctx.store;
        g.epoch = num(st.meta("epoch")).max(1);
        let _ = st.set_meta("epoch", &g.epoch.to_string());
        g.last = num(st.meta("last_seq")) + 100;
        g.persisted = g.last;
        let _ = st.set_meta("last_seq", &g.last.to_string());
        g.candidate = st.meta("candidate").as_deref() == Some("1");
        let saved = st.meta("membership").unwrap_or_default();
        for id in serde_json::from_str::<Vec<String>>(&saved).unwrap_or_default() {
            g.members.insert(id, Instant::now());
        }
        g.saved_members = saved;
        sync_status(ctx, &g);
    }
    g
}

/// Call once at head start: loads epoch/role into the status.
pub fn start(ctx: &Ctx) {
    drop(init(ctx));
}

/// Current status (initialises it first).
pub fn status(ctx: &Ctx) -> Status {
    drop(init(ctx));
    ctx.repl.status.lock().unwrap().clone()
}

/// Append an entry to the change log and send it to the standbys. Returns its seq.
/// Safe to call while holding the head's State lock (never takes it).
pub fn record(ctx: &Ctx, kind: &str, data: serde_json::Value) -> u64 {
    let mut g = init(ctx);
    g.last += 1;
    let e = Entry { seq: g.last, epoch: g.epoch, kind: kind.into(), data };
    if g.ring.len() >= RING_MAX {
        g.ring.pop_front();
    }
    g.ring.push_back(e.clone());
    g.links.retain(|l| l.tx.send(e.clone()).is_ok());
    let seq = g.last;
    let due = seq - g.persisted >= 64;
    if due {
        g.persisted = seq;
    }
    sync_status(ctx, &g);
    drop(g);
    if due {
        let _ = ctx.store.set_meta("last_seq", &seq.to_string());
    }
    seq
}

/// Highest seq that is durable (see header). With no standby: the last recorded seq.
pub fn durable_seq(ctx: &Ctx) -> u64 {
    let g = init(ctx);
    if g.links.is_empty() { g.last } else { g.links.iter().map(|l| l.acked).max().unwrap_or(0) }
}

pub fn standby_count(ctx: &Ctx) -> usize {
    init(ctx).links.len()
}

/// Node ids of the standbys connected to this primary now (HA-10: never sleep or re-recruit them).
pub fn standby_nodes(ctx: &Ctx) -> Vec<String> {
    init(ctx).links.iter().map(|l| l.node.clone()).collect()
}

/// Size of the known membership (nodes connected in the last 60 s; after a takeover: the replicated list).
pub fn membership(ctx: &Ctx) -> usize {
    init(ctx).members.len()
}

/// Nodes connected now, as of the last `refresh`.
pub fn connected(ctx: &Ctx) -> usize {
    init(ctx).connected
}

/// Re-read the connected nodes from the head state, expire old members, clear "candidate" once a quorum is back and
/// replicate membership changes. Takes the State lock briefly: do not call it while holding that lock.
pub fn refresh(ctx: &Ctx) {
    let ids: HashSet<String> = ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| w.hello.as_ref().map(|h| h.node_id.clone())).collect();
    let mut g = init(ctx);
    let now = Instant::now();
    for id in &ids {
        g.members.insert(id.clone(), now);
    }
    g.connected = ids.len();
    // A candidate's replicated members carry the promotion time, so ones that never come back expire one window after
    // the takeover. Safe: a cut-off old primary with standbys stops dispatching after 10 s without quorum, well inside it.
    g.members.retain(|_, t| now.duration_since(*t) < MEMBER_WINDOW);
    let mut cleared = false;
    if g.candidate && has_quorum(g.connected, g.members.len()) {
        g.candidate = false;
        cleared = true;
    }
    let mut list: Vec<&String> = g.members.keys().collect();
    list.sort();
    let text = serde_json::to_string(&list).unwrap_or_default();
    let changed = text != g.saved_members;
    if changed {
        g.saved_members = text.clone();
    }
    sync_status(ctx, &g);
    let have_standbys = !g.links.is_empty();
    drop(g);
    if cleared {
        let _ = ctx.store.set_meta("candidate", "0");
    }
    if changed {
        let _ = ctx.store.set_meta("membership", &text);
        if have_standbys {
            record(ctx, "membership", json!({ "nodes": serde_json::from_str::<Value>(&text).unwrap_or_default() }));
        }
    }
}

/// May this head hand out work now: not a candidate without quorum, and a quorum of the membership is connected.
pub fn may_dispatch(ctx: &Ctx) -> bool {
    refresh(ctx);
    let g = init(ctx);
    !g.candidate && has_quorum(g.connected, g.members.len())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

/// The data of a "config" entry: `record(ctx, "config", repl::config_entry(ctx))` after any org/user/key change.
/// (UserRow serialises without its password hash, so users are written out by hand here.)
pub fn config_entry(ctx: &Ctx) -> Value {
    let st = &ctx.store;
    json!({
        "export": crate::api::export_config(ctx),
        "users": st.users().iter().map(|u| json!({"name": u.name, "role": u.role, "pw_hash": u.pw_hash, "created_ms": u.created_ms})).collect::<Vec<_>>(),
        "tokens": st.tokens().iter().map(|(h, u)| json!([h, u])).collect::<Vec<_>>(),
        "node_keys": st.node_keys().iter().map(|(n, k)| json!([n, hex(k)])).collect::<Vec<_>>(),
        "nodes": st.nodes(),
        // settings that live in store meta: they must survive a takeover too
        "meta": SETTINGS_META.iter().filter_map(|k| st.meta(k).map(|v| (k.to_string(), Value::String(v)))).collect::<serde_json::Map<_, _>>(),
    })
}

/// Store meta keys copied to standbys inside "config" entries (power, standby target, every tunable).
pub const SETTINGS_META: &[&str] = &["power", "ha_settings", "tune", "asleep"];

#[derive(Debug, PartialEq)]
pub enum Plan {
    Replay,
    Snapshot,
}

/// Pure: can the ring bring a standby at (`standby_seq`, `standby_epoch`) up to date?
pub fn plan(standby_seq: u64, standby_epoch: Option<u64>, epoch: u64, last: u64, ring_len: usize) -> Plan {
    let first = last + 1 - (ring_len as u64).min(last);
    if standby_epoch == Some(epoch) && standby_seq <= last && standby_seq + 1 >= first { Plan::Replay } else { Plan::Snapshot }
}

pub fn hello_s_payload(h: &Hello, seq: u64, epoch: u64) -> Vec<u8> {
    let j = serde_json::to_vec(h).unwrap_or_default();
    let mut p = (j.len() as u32).to_le_bytes().to_vec();
    p.extend_from_slice(&j);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&epoch.to_le_bytes());
    p
}

/// (hello, last applied seq, standby epoch if sent)
pub fn parse_hello_s(p: &[u8]) -> Option<(Hello, u64, Option<u64>)> {
    let n = u32::from_le_bytes(p.get(0..4)?.try_into().ok()?) as usize;
    let h = serde_json::from_slice(p.get(4..4 + n)?).ok()?;
    let seq = u64_at(p, 4 + n).ok()?;
    Some((h, seq, u64_at(p, 12 + n).ok()))
}

fn position(g: &Inner, lid: u64) -> (usize, Vec<String>) {
    let me = g.links.iter().position(|l| l.id == lid).unwrap_or(0);
    (me, g.links.iter().filter(|l| l.id != lid).map(|l| l.peer.clone()).collect())
}

fn hello_entry(g: &Inner, lid: u64, mode: &str) -> Entry {
    let (pri, peers) = position(g, lid);
    Entry { seq: 0, epoch: g.epoch, kind: "hello".into(), data: json!({"priority": pri, "peers": peers, "mode": mode, "last_seq": g.last}) }
}

fn put_entry(w: &mut impl Write, tx: &mut Half, e: &Entry) -> io::Result<()> {
    tx.send(w, proto::REPL, &serde_json::to_vec(e).unwrap_or_default())
}

/// One direction of a replication link. In a secured fleet (after admission) every frame is sealed with FrameMac and
/// its payload XORed with an HMAC-SHA256 counter keystream under the per-connection key, so a snapshot (it carries
/// node keys) never crosses the LAN in clear. CK_FLEET=open: plain frames.
pub struct Half {
    sec: Option<([u8; 32], crate::auth::FrameMac)>,
    dir: u8,
    n: u64,
}

impl Half {
    pub fn plain() -> Half {
        Half { sec: None, dir: 0, n: 0 }
    }
    /// dir 0 = primary to standby, 1 = standby to primary (separate keystreams).
    pub fn new(sk: [u8; 32], dir: u8) -> Half {
        Half { sec: Some((sk, crate::auth::FrameMac::new(&sk))), dir, n: 0 }
    }
    fn xor(sk: &[u8; 32], dir: u8, n: u64, p: &mut [u8]) {
        for (i, c) in p.chunks_mut(32).enumerate() {
            let mut m = [0u8; 17];
            m[0] = dir;
            m[1..9].copy_from_slice(&n.to_le_bytes());
            m[9..].copy_from_slice(&(i as u64).to_le_bytes());
            for (b, k) in c.iter_mut().zip(crate::auth::hmac_sha256(sk, &m)) {
                *b ^= k;
            }
        }
    }
    pub fn send(&mut self, w: &mut impl Write, kind: u8, p: &[u8]) -> io::Result<()> {
        let Some((sk, mac)) = &mut self.sec else { return send(w, kind, p) };
        let mut b = p.to_vec();
        Half::xor(sk, self.dir, self.n, &mut b);
        self.n += 1;
        proto::send_sealed(w, mac, kind, &b)
    }
    pub fn recv(&mut self, r: &mut impl io::Read) -> io::Result<(u8, Vec<u8>)> {
        let Some((sk, mac)) = &mut self.sec else { return recv(r) };
        let (k, mut b) = proto::recv_sealed(r, mac)?;
        Half::xor(sk, self.dir, self.n, &mut b);
        self.n += 1;
        Ok((k, b))
    }
}

/// Per-connection key from the node key and the primary's salt (sent in the sealed admission HEARTBEAT).
pub fn link_key(node_key: &[u8], salt: &[u8]) -> [u8; 32] {
    crate::auth::hmac_sha256(node_key, &[b"ck-repl/1 ".as_slice(), salt].concat())
}

/// Serve one standby connection (head.rs calls this for a HELLO_S, after admission). Blocks until it drops.
/// `sk`: the link key in a secured fleet (see Half), None with CK_FLEET=open.
pub fn serve_standby(ctx: &Ctx, s: TcpStream, hello: &[u8], sk: Option<[u8; 32]>) -> std::io::Result<()> {
    let (h, seq, ep) = parse_hello_s(hello).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HELLO_S"))?;
    let peer = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(proto::LOST_AFTER))?;
    let (tx, rx) = mpsc::channel();
    // register the link and pick the plan in one critical section: nothing recorded after this point is missed
    let (lid, snapshot, replay, snap_seq, epoch, first) = {
        let mut g = init(ctx);
        let snapshot = plan(seq, ep, g.epoch, g.last, g.ring.len()) == Plan::Snapshot;
        let replay: Vec<Entry> = if snapshot { vec![] } else { g.ring.iter().filter(|e| e.seq > seq).cloned().collect() };
        g.next_link += 1;
        let lid = g.next_link;
        g.links.push(Link { id: lid, node: h.node_id.clone(), peer, tx, acked: if snapshot { 0 } else { seq } });
        sync_status(ctx, &g);
        let first = hello_entry(&g, lid, if snapshot { "snapshot" } else { "replay" });
        (lid, snapshot, replay, g.last, g.epoch, first)
    };
    println!("standby {} joined ({})", h.node_id, if snapshot { "snapshot" } else { "replay" });
    let mut w = s.try_clone()?;
    let rs = s.try_clone()?;
    let (mut htx, mut hrx) = match sk { Some(k) => (Half::new(k, 0), Half::new(k, 1)), None => (Half::plain(), Half::plain()) };
    let res = std::thread::scope(|sc| {
        sc.spawn(move || {
            let mut r = io::BufReader::new(&rs);
            while let Ok((k, p)) = hrx.recv(&mut r) {
                if k == proto::REPL_ACK {
                    if let Ok(a) = u64_at(&p, 0) {
                        if let Some(l) = ctx.repl.inner.lock().unwrap().links.iter_mut().find(|l| l.id == lid) {
                            l.acked = l.acked.max(a);
                        }
                        let d = durable_seq(ctx);
                        sync_status(ctx, &ctx.repl.inner.lock().unwrap());
                        crate::head::durable_upto(ctx, d);
                    }
                }
            }
            let _ = rs.shutdown(std::net::Shutdown::Both);
        });
        let r = (|| -> io::Result<()> {
            put_entry(&mut w, &mut htx, &first)?;
            if snapshot {
                let tmp = std::env::temp_dir().join(format!("ck-snap-{}-{lid}.db", std::process::id()));
                let made = ctx.store.snapshot_to(&tmp).map_err(io::Error::other).and_then(|_| std::fs::read(&tmp));
                let _ = std::fs::remove_file(&tmp);
                let mut p = epoch.to_le_bytes().to_vec();
                p.extend_from_slice(&snap_seq.to_le_bytes());
                p.extend_from_slice(&made?);
                htx.send(&mut w, proto::SNAPSHOT, &p)?;
            }
            for e in &replay {
                put_entry(&mut w, &mut htx, e)?;
            }
            let mut shown = (position(&init(ctx), lid), 0u8).0;
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(e) => put_entry(&mut w, &mut htx, &e)?,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        htx.send(&mut w, proto::HEARTBEAT, &[])?;
                        refresh(ctx);
                        let (e, now) = {
                            let g = init(ctx);
                            (hello_entry(&g, lid, "replay"), position(&g, lid))
                        };
                        let last = ctx.repl.inner.lock().unwrap().last;
                        let _ = ctx.store.set_meta("last_seq", &last.to_string());
                        if now != shown {
                            shown = now;
                            put_entry(&mut w, &mut htx, &e)?;
                        }
                    }
                    Err(_) => return Ok(()),
                }
            }
        })();
        let _ = s.shutdown(std::net::Shutdown::Both);
        r
    });
    {
        let mut g = init(ctx);
        g.links.retain(|l| l.id != lid);
        sync_status(ctx, &g);
    }
    crate::head::durable_upto(ctx, durable_seq(ctx)); // fewer standbys can mean more is durable
    println!("standby {} left", h.node_id);
    res
}

// ---------------------------------------------------------------- applying entries (standby)

/// Apply one entry to a standby's store. Idempotent: replays after a snapshot may repeat entries.
pub fn apply(st: &Store, e: &Entry) -> Result<(), String> {
    let d = &e.data;
    let u = |k: &str| d.get(k).and_then(|x| x.as_u64()).ok_or_else(|| format!("{}: missing {k}", e.kind));
    match e.kind.as_str() {
        "job" => {
            let spec: JobSpec = serde_json::from_value(d.get("spec").cloned().unwrap_or_default()).map_err(|x| x.to_string())?;
            st.insert_job_with_id(u("id")?, &spec, d.get("ms").and_then(|x| x.as_i64()).unwrap_or_else(now_ms))?;
        }
        "part" => {
            let body: Vec<u8> = serde_json::from_value(d.get("body").cloned().unwrap_or_default()).map_err(|x| x.to_string())?;
            st.add_part(u("job")?, u("a")?, u("b")?, &body)?;
        }
        "state" => {
            let state = d.get("state").and_then(|x| x.as_str()).and_then(JobState::parse).ok_or("state: bad state")?;
            st.set_state(u("job")?, state, d.get("ms").and_then(|x| x.as_i64()).unwrap_or_else(now_ms), d.get("error").and_then(|x| x.as_str()))?;
        }
        "config" => apply_config(st, d)?,
        "membership" => st.set_meta("membership", &d.get("nodes").cloned().unwrap_or_default().to_string())?,
        _ => {}
    }
    Ok(())
}

fn apply_config(st: &Store, d: &Value) -> Result<(), String> {
    let arr = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if let Some(m) = d.get("meta").and_then(|m| m.as_object()) {
        for (k, v) in m.iter().filter(|(k, _)| SETTINGS_META.contains(&k.as_str())) {
            st.set_meta(k, v.as_str().unwrap_or_default())?;
        }
    }
    for n in arr(d, "nodes") {
        if let Ok(n) = serde_json::from_value::<NodeRow>(n) {
            let _ = st.upsert_node(&n.node_id, &n.host, &n.addr, &n.inventory, n.last_seen_ms);
            if !n.state.is_empty() {
                let _ = st.set_node_state(&n.node_id, &n.state, &n.state_reason, &n.state_by, now_ms());
            }
        }
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let users: Vec<crate::jobs::UserRow> = arr(d, "users")
        .iter()
        .map(|u| crate::jobs::UserRow { name: s(u, "name"), role: s(u, "role"), pw_hash: s(u, "pw_hash"), created_ms: u.get("created_ms").and_then(|x| x.as_i64()).unwrap_or(0) })
        .collect();
    let pair = |v: &Value| (v.get(0).and_then(|x| x.as_str()).unwrap_or("").to_string(), v.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string());
    let tokens: Vec<(String, String)> = arr(d, "tokens").iter().map(pair).collect();
    let keys: Vec<(String, Vec<u8>)> = arr(d, "node_keys").iter().map(|v| { let (n, k) = pair(v); (n, unhex(&k)) }).collect();
    st.replace_auth(&users, &tokens, &keys)?;
    if let Some(x) = d.get("export") {
        import_export(st, x);
    }
    Ok(())
}

/// Additive copy of the api config export (labs, placements, labels, pools, caps, reserves). Deletions are not replicated.
/// ponytail: a local copy of api.rs config_apply without validation; use that one once it is pub.
fn import_export(st: &Store, v: &Value) {
    let arr = |k: &str| v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let s = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let known: HashSet<String> = st.nodes().into_iter().map(|x| x.node_id).collect();
    for l in arr("labs") {
        let (name, w, h) = (s(&l, "name"), n(&l, "w"), n(&l, "h"));
        match st.labs().iter().find(|x| x.name == name) {
            None => { let _ = st.create_lab(&name, w, h, now_ms()); }
            Some(x) if x.w != w || x.h != h => { let _ = st.update_lab(x.id, None, Some(w), Some(h)); }
            _ => {}
        }
    }
    let labs = st.labs();
    for p in arr("placements") {
        let (id, lab) = (s(&p, "node_id"), s(&p, "lab"));
        if let (true, Some(l)) = (known.contains(&id), labs.iter().find(|x| x.name == lab)) {
            let _ = st.place_node(&id, Some((l.id, n(&p, "x"), n(&p, "y"))));
        }
    }
    for l in arr("labels") {
        if known.contains(&s(&l, "node_id")) {
            let _ = st.set_label(&s(&l, "node_id"), &s(&l, "key"), Some(&s(&l, "value")));
        }
    }
    for p in arr("pools") {
        let members: Vec<String> = p.get("members").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).filter(|m| known.contains(m)).collect()).unwrap_or_default();
        let _ = st.put_pool(&s(&p, "name"), &members, &s(&p, "note"));
    }
    for c in arr("caps") {
        let (sel, m) = (s(&c, "selector"), n(&c, "max_threads"));
        if !st.caps().iter().any(|x| x.selector == sel && x.max_threads == Some(m)) {
            let _ = st.add_cap(&sel, Some(m), &s(&c, "note"));
        }
    }
    for c in arr("reserves") {
        let (sel, kn, kp) = (s(&c, "selector"), n(&c, "keep_n"), n(&c, "keep_pct"));
        if !st.reserves().iter().any(|x| x.selector == sel && x.keep_n == kn && x.keep_pct == kp) {
            let _ = st.add_reserve(&sel, kn, kp, &s(&c, "note"));
        }
    }
}

/// Replace the database file with a snapshot (atomic rename) and reopen it.
fn install_snapshot(store: &mut Store, path: &Path, body: &[u8]) -> Result<(), String> {
    let tmp = path.with_file_name("head.db.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(body).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    drop(std::mem::replace(store, Store::memory())); // closes the old file
    for ext in ["-wal", "-shm"] {
        let mut o = path.as_os_str().to_owned();
        o.push(ext);
        let _ = std::fs::remove_file(o);
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    *store = Store::open(path)?;
    Ok(())
}

// ---------------------------------------------------------------- standby side

static STANDBY: OnceLock<Mutex<Status>> = OnceLock::new();

/// Status of this process while it runs as a standby (role "standby", primary "ip:port"); a5 can serve it.
pub fn standby_status() -> Status {
    STANDBY.get_or_init(Default::default).lock().unwrap().clone()
}

fn set_sb(f: impl FnOnce(&mut Status)) {
    f(&mut STANDBY.get_or_init(Default::default).lock().unwrap());
}

struct Sb {
    handover: bool, // HA-07: the primary named this standby in a "promote" entry; promote as soon as the link closes
    priority: u32,
    peers: Vec<String>,
    last_heard: Instant,
    epoch: Arc<AtomicU64>,
}

fn invalid(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

/// One connection to a primary: HELLO_S, then snapshot / entries until the link ends. Returns on any error.
fn session(store: &mut Store, path: &Path, mut s: TcpStream, sb: &mut Sb) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(proto::LOST_AFTER + Duration::from_secs(2 * sb.priority as u64)))?;
    let node = crate::identity::node_id();
    let hello = Hello { node_id: node.clone(), host: crate::identity::host(), virtual_node: false, inv: crate::identity::inventory(), join_code: std::env::var("CK_JOIN_CODE").ok() };
    let mut applied = num(store.meta("last_seq"));
    send(&mut s, proto::HELLO_S, &hello_s_payload(&hello, applied, num(store.meta("epoch"))))?;
    let mut r = io::BufReader::new(s.try_clone()?);
    let kp = crate::identity::key_path(&node);
    // admission happens in plain frames; after the primary's sealed HEARTBEAT (salt) everything goes through Half
    let (mut key, mut rx, mut tx): (Option<Vec<u8>>, Half, Half) = (None, Half::plain(), Half::plain());
    loop {
        let (k, mut p) = rx.recv(&mut r)?;
        sb.last_heard = Instant::now();
        if rx.sec.is_none() {
            if let Some(nk) = &key {
                // secured fleet: the only plain frame allowed after admission is the sealed salt HEARTBEAT
                let tag = p.split_off(p.len().saturating_sub(40));
                if k != proto::HEARTBEAT || p.len() != 32 || !crate::auth::FrameMac::new(nk).open(k, &p, &tag) {
                    return Err(invalid("unsealed frame from the primary"));
                }
                let sk = link_key(nk, &p);
                (rx, tx) = (Half::new(sk, 0), Half::new(sk, 1));
                continue;
            }
        }
        match k {
            proto::CHALLENGE => {
                let nk = crate::auth::load_key(&kp)?;
                send(&mut s, proto::PROOF, &crate::auth::respond(&nk, &p, &node))?;
                key = Some(nk);
            }
            proto::KEY => {
                if let Some(d) = kp.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                crate::auth::save_key(&kp, &p)?;
                key = Some(p);
            }
            proto::DENIED => return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("primary refused this PC: {}", String::from_utf8_lossy(&p)))),
            proto::SNAPSHOT => {
                let (epoch, seq) = (u64_at(&p, 0)?, u64_at(&p, 8)?);
                install_snapshot(store, path, &p[16..]).map_err(io::Error::other)?;
                for (k, v) in [("last_seq", seq), ("epoch", epoch)] {
                    store.set_meta(k, &v.to_string()).map_err(io::Error::other)?;
                }
                store.set_meta("synced", "1").map_err(io::Error::other)?;
                applied = seq;
                sb.epoch.store(epoch, SeqCst);
                set_sb(|t| { t.epoch = epoch; t.last_seq = seq });
                tx.send(&mut s, proto::REPL_ACK, &seq.to_le_bytes())?;
            }
            proto::REPL => {
                let e: Entry = serde_json::from_slice(&p).map_err(|_| invalid("bad REPL entry"))?;
                if e.epoch != sb.epoch.load(SeqCst) {
                    sb.epoch.store(e.epoch, SeqCst);
                    let _ = store.set_meta("epoch", &e.epoch.to_string());
                }
                if e.kind == "hello" {
                    sb.priority = e.data.get("priority").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    sb.peers = e.data.get("peers").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    if e.data.get("mode").and_then(|x| x.as_str()) == Some("replay") {
                        let _ = store.set_meta("synced", "1");
                    }
                    s.set_read_timeout(Some(proto::LOST_AFTER + Duration::from_secs(2 * sb.priority as u64)))?;
                } else if e.seq > applied {
                    if e.kind == "promote" && e.data["node"].as_str() == Some(node.as_str()) {
                        sb.handover = true;
                        println!("standby: the primary hands over to this PC");
                    }
                    if e.seq != applied + 1 {
                        return Err(invalid("gap in the change log"));
                    }
                    if let Err(x) = apply(store, &e) {
                        eprintln!("standby: entry {} ({}): {x}", e.seq, e.kind); // ponytail: logged, not fatal
                    }
                    applied = e.seq;
                    let _ = store.set_meta("last_seq", &applied.to_string());
                    set_sb(|t| t.last_seq = applied);
                    tx.send(&mut s, proto::REPL_ACK, &applied.to_le_bytes())?;
                }
            }
            _ => {} // HEARTBEAT and anything unknown
        }
    }
}

fn connect(target: &str) -> io::Result<TcpStream> {
    let t = if target.contains(':') { target.to_string() } else { format!("{target}:{}", std::env::var("CK_PRIMARY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or_else(proto::port)) };
    let a = t.to_socket_addrs()?.next().ok_or_else(|| invalid("no address"))?;
    TcpStream::connect_timeout(&a, Duration::from_secs(1))
}

/// Best effort: a beacon with a higher epoch than the primary we follow means a standby promoted; follow that head.
fn watch_beacons(epoch: Arc<AtomicU64>, head: Arc<Mutex<String>>, cur: Arc<Mutex<Option<TcpStream>>>, switch: Arc<Mutex<Option<String>>>) {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", proto::disc_port())) else { return }; // agents on this PC may hold the port
    let mut buf = [0u8; 512];
    while let Ok((n, _)) = sock.recv_from(&mut buf) {
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let mut t = text.split_whitespace();
        let (Some("ck/1"), addr, e) = (t.next(), text.split_whitespace().find(|x| x.contains(':')), text.split_whitespace().find_map(|x| x.strip_prefix("e=").and_then(|v| v.parse::<u64>().ok()))) else { continue };
        let (Some(addr), Some(e)) = (addr, e) else { continue };
        let same = head.lock().unwrap().split(':').next() == addr.split(':').next();
        if e > epoch.load(SeqCst) && !same {
            *switch.lock().unwrap() = Some(addr.to_string());
            if let Some(s) = cur.lock().unwrap().take() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// `ck head --standby`: sync from the primary, apply entries, watch it; returns when this PC must promote (the caller
/// then runs head::run). Never returns while the primary is healthy.
pub fn run_standby(o: &crate::Opts) {
    let first = o.head.clone().unwrap_or_else(|| crate::die("--standby needs --head PRIMARY_IP"));
    let path = crate::store::default_path();
    let mut store = Store::open(&path).unwrap_or_else(|e| crate::die(&format!("database: {e}")));
    let epoch = Arc::new(AtomicU64::new(num(store.meta("epoch"))));
    let head = Arc::new(Mutex::new(first));
    let (cur, switch): (Arc<Mutex<Option<TcpStream>>>, Arc<Mutex<Option<String>>>) = Default::default();
    {
        let (e, h, c, s) = (epoch.clone(), head.clone(), cur.clone(), switch.clone());
        std::thread::spawn(move || watch_beacons(e, h, c, s));
    }
    set_sb(|t| { t.role = "standby".into(); t.epoch = epoch.load(SeqCst); t.last_seq = num(store.meta("last_seq")); t.primary = head.lock().unwrap().clone() });
    let mut sb = Sb { handover: false, priority: 0, peers: vec![], last_heard: Instant::now(), epoch: epoch.clone() };
    let mut backoff = Duration::from_millis(200);
    println!("standby: following {}", head.lock().unwrap());
    loop {
        if let Some(a) = switch.lock().unwrap().take() {
            println!("standby: a head with a higher epoch beacons at {a}, following it");
            *head.lock().unwrap() = a;
            sb.last_heard = Instant::now();
        }
        let synced = store.meta("synced").as_deref() == Some("1");
        if synced && (sb.handover || should_promote(sb.priority, sb.last_heard.elapsed().as_secs_f64())) {
            let e = num(store.meta("epoch")).max(epoch.load(SeqCst)) + 1;
            let _ = store.set_meta("epoch", &e.to_string());
            let _ = store.set_meta("candidate", "1");
            set_sb(|t| { t.role = "candidate".into(); t.epoch = e; t.connected = 0 });
            if sb.handover {
                println!("standby: planned handover, promoting at epoch {e}");
            } else {
                println!("standby: primary silent for {:.1}s, promoting at epoch {e}", sb.last_heard.elapsed().as_secs_f64());
            }
            return;
        }
        let mut targets = vec![head.lock().unwrap().clone()];
        targets.extend(sb.peers.iter().cloned());
        let mut ran = false;
        for t in targets {
            let Ok(s) = connect(&t) else { continue };
            *head.lock().unwrap() = t.clone();
            *cur.lock().unwrap() = s.try_clone().ok();
            set_sb(|x| { x.primary = t.clone(); x.connected = 1 });
            let r = session(&mut store, &path, s, &mut sb);
            *cur.lock().unwrap() = None;
            set_sb(|x| x.connected = 0);
            eprintln!("standby: link to {t} ended: {}", r.err().map_or("closed".into(), |e| e.to_string()));
            ran = true;
            backoff = Duration::from_millis(200);
            break;
        }
        std::thread::sleep(backoff);
        if !ran {
            backoff = (backoff * 2).min(Duration::from_secs(1));
        }
    }
}

/// HA-07 primary side: tell standby `node` to take over and wait (up to 5 s) until it applied that entry, so it holds
/// everything this head recorded. Returns its address. The caller then stops dispatching and closes the link.
pub fn hand_over(ctx: &Ctx, node: &str) -> Result<String, String> {
    let peer = init(ctx).links.iter().find(|l| l.node == node).map(|l| l.peer.clone()).ok_or_else(|| format!("{node} is not a connected standby"))?;
    let seq = record(ctx, "promote", json!({ "node": node }));
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if init(ctx).links.iter().any(|l| l.node == node && l.acked >= seq) {
            return Ok(peer);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("{node} did not confirm the handover within 5 s; nothing changed"))
}

/// Pure: should a standby with this priority promote after the primary has been silent this long?
pub fn should_promote(priority: u32, silent_secs: f64) -> bool {
    silent_secs >= crate::proto::LOST_AFTER.as_secs_f64() + 2.0 * priority as f64
}

/// Pure: may a head dispatch with `connected` agents out of `membership` known nodes?
pub fn has_quorum(connected: usize, membership: usize) -> bool {
    membership == 0 || connected * 2 > membership
}

/// The epoch a head beacons and stamps on its work; stored in the database (store meta "epoch").
pub fn epoch(ctx: &Ctx) -> u64 {
    init(ctx).epoch
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn ctx() -> Ctx {
        Ctx { sh: Arc::default(), store: Arc::new(Store::memory()), bus: Default::default(), repl: Default::default() }
    }

    #[test]
    fn sealed_link_round_trip_and_tamper() {
        let sk = link_key(b"node key", b"salt");
        let (mut a, mut b) = (Half::new(sk, 0), Half::new(sk, 0));
        let mut wire = vec![];
        a.send(&mut wire, proto::SNAPSHOT, b"node_keys: secret").unwrap();
        a.send(&mut wire, proto::REPL, &[7u8; 100]).unwrap();
        assert!(!wire.windows(6).any(|w| w == b"secret"), "payload is encrypted on the wire");
        let mut r = &wire[..];
        assert_eq!(b.recv(&mut r).unwrap(), (proto::SNAPSHOT, b"node_keys: secret".to_vec()));
        assert_eq!(b.recv(&mut r).unwrap(), (proto::REPL, vec![7u8; 100]));
        wire[8] ^= 1; // flip one payload bit of the first frame
        let mut c = Half::new(sk, 0);
        assert!(c.recv(&mut &wire[..]).is_err(), "tampered frame refused");
        assert!(Half::new(link_key(b"other", b"salt"), 0).recv(&mut &wire[..]).is_err(), "wrong key refused");
    }

    #[test]
    fn pure_rules() {
        assert!(!should_promote(0, 4.9));
        assert!(should_promote(0, 5.0));
        assert!(!should_promote(2, 8.0));
        assert!(should_promote(2, 9.0));
        assert!(has_quorum(0, 0));
        assert!(!has_quorum(5, 10));
        assert!(has_quorum(6, 10));
    }

    #[test]
    fn replay_or_snapshot() {
        // ring holds seq 151..=200
        assert_eq!(plan(200, Some(1), 1, 200, 50), Plan::Replay, "up to date");
        assert_eq!(plan(150, Some(1), 1, 200, 50), Plan::Replay, "just inside the ring");
        assert_eq!(plan(149, Some(1), 1, 200, 50), Plan::Snapshot, "fell off the ring");
        assert_eq!(plan(201, Some(1), 1, 200, 50), Plan::Snapshot, "ahead of the primary");
        assert_eq!(plan(200, Some(0), 1, 200, 50), Plan::Snapshot, "other epoch");
        assert_eq!(plan(200, None, 1, 200, 50), Plan::Snapshot);
        assert_eq!(plan(0, Some(1), 1, 100, 0), Plan::Snapshot, "fresh standby after a head start");
        assert_eq!(plan(100, Some(1), 1, 100, 0), Plan::Replay);
    }

    #[test]
    fn hello_s_round_trip() {
        let h = Hello { node_id: "n1".into(), host: "h".into(), ..Default::default() };
        let p = hello_s_payload(&h, 7, 3);
        assert_eq!(parse_hello_s(&p), Some((h, 7, Some(3))));
        assert!(parse_hello_s(&p[..5]).is_none());
        assert_eq!(unhex(&hex(&[0, 255, 16])), vec![0, 255, 16]);
    }

    #[test]
    fn record_seq_and_durable_without_standby() {
        let c = ctx();
        start(&c);
        let st = status(&c);
        assert_eq!((st.role.as_str(), st.epoch, st.last_seq), ("primary", 1, 100));
        assert_eq!(record(&c, "x", json!({})), 101);
        assert_eq!(durable_seq(&c), 101);
        assert_eq!(c.store.meta("epoch").as_deref(), Some("1"));
    }

    fn spec(n: &str) -> JobSpec {
        JobSpec { name: n.into(), kind: proto::KIND_SYNTH, end: 20, chunk: 10, arg: vec![0; 8], submitter: "t".into(), ..Default::default() }
    }

    #[test]
    fn two_stores_converge_over_a_link() {
        let c = ctx();
        let j1 = c.store.create_job(&spec("before"), 1).unwrap(); // exists before the standby joins: comes by snapshot
        c.store.add_part(j1.id, 0, 10, b"aa").unwrap();
        record(&c, "job", json!({"id": j1.id, "spec": spec("before")}));
        let dir = std::env::temp_dir().join(format!("ck-repl-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.db");
        let mut sstore = Store::open(&path).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        let killer = client.try_clone().unwrap();
        let mut sb = Sb { handover: false, priority: 0, peers: vec![], last_heard: Instant::now(), epoch: Arc::new(AtomicU64::new(0)) };
        let ok = std::thread::scope(|sc| {
            sc.spawn(|| {
                let mut srv = srv;
                let (k, p) = recv(&mut srv).unwrap();
                assert_eq!(k, proto::HELLO_S);
                let _ = serve_standby(&c, srv, &p, None);
            });
            sc.spawn(|| { let _ = session(&mut sstore, &path, client, &mut sb); });
            let wait = |f: &dyn Fn() -> bool| (0..300).any(|_| f() || { std::thread::sleep(Duration::from_millis(20)); false });
            assert!(wait(&|| standby_count(&c) == 1 && durable_seq(&c) == c.repl.inner.lock().unwrap().last), "snapshot acked");
            // live changes
            let j2 = c.store.create_job(&spec("live"), 2).unwrap();
            record(&c, "job", json!({"id": j2.id, "spec": spec("live"), "ms": 2}));
            c.store.add_part(j2.id, 0, 10, b"bb").unwrap();
            let last = record(&c, "part", json!({"job": j2.id, "a": 0, "b": 10, "body": [98, 98]}));
            assert!(durable_seq(&c) < last, "not durable before the standby acks");
            c.store.set_state(j2.id, JobState::Running, 3, None).unwrap();
            record(&c, "state", json!({"job": j2.id, "state": "running", "error": null, "ms": 3}));
            c.store.create_user("u", "h", "admin", 1).unwrap();
            c.store.add_token("t", "u", 1).unwrap();
            c.store.create_lab("lab", 4, 4, 1).unwrap();
            let last = record(&c, "config", config_entry(&c));
            let ok = wait(&|| durable_seq(&c) == last);
            let _ = killer.shutdown(std::net::Shutdown::Both);
            ok
        });
        assert!(ok, "durable_seq follows the acks");
        let p = c.store.jobs(10, None);
        assert_eq!(sstore.jobs(10, None).len(), 2);
        for j in &p {
            assert_eq!(sstore.job(j.id).map(|x| (x.name, x.state, x.done)), Some((j.name.clone(), j.state, j.done)));
            assert_eq!(sstore.parts(j.id), c.store.parts(j.id));
        }
        assert_eq!((sstore.users(), sstore.tokens(), sstore.labs().len()), (c.store.users(), c.store.tokens(), 1));
        assert_eq!(sstore.meta("synced").as_deref(), Some("1"));
        assert_eq!(standby_count(&c), 0, "link removed after the drop");
        assert_eq!(durable_seq(&c), c.repl.inner.lock().unwrap().last);
        drop(sstore);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
