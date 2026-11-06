//! Agent (worker): finds the head, pulls chunks, runs them, reports. Owner: a3.
//! Contract: keep `run(&Opts)` (the `ck worker`/`ck agent` command) and `worker(head, name, threads, gpu)`
//! (the head's local worker).

use std::io;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::auth::{self, FrameMac};
use crate::head::{recv_any, Out};
use crate::kernels::{run_task_out, CANCELLED, CANCELLED_RANGE};
use crate::proto::*;
use crate::{die, Opts};

/// Join code for the head's own worker (set by head::run); other agents read CK_JOIN_CODE.
pub static LOCAL_CODE: OnceLock<String> = OnceLock::new();

fn join_code() -> Option<String> {
    LOCAL_CODE.get().cloned().or_else(|| std::env::var("CK_JOIN_CODE").ok()).map(|c| c.trim().to_string()).filter(|c| !c.is_empty())
}

pub fn run(o: &Opts) {
    if o.relay || o.head.is_none() {
        listen_once(o.relay);
    }
    if o.virtual_n <= 1 {
        return worker(o.head.clone(), &o.name, o.threads, o.gpu);
    }
    // SIM-01: N logical agents, one connection each. One discovery (one UDP bind) shared by all.
    // ponytail: the head address is resolved once; if the head moves, restart (or pass --head).
    let ip = o.head.clone().unwrap_or_else(|| discover(&o.name).0);
    let hs: Vec<_> = (0..o.virtual_n)
        .map(|i| {
            let (ip, name, threads, gpu) = (ip.clone(), o.name.clone(), o.threads, o.gpu);
            thread::spawn(move || worker_as(Some(ip), &name, threads, gpu, Some(i)))
        })
        .collect();
    for h in hs {
        let _ = h.join();
    }
}

/// One beacon: cluster name, head address "ip:port", and whether a relay already forwarded it.
#[derive(Debug, PartialEq)]
pub(crate) struct Beacon {
    pub name: String,
    pub addr: String,
    pub relayed: bool,
    /// head term (" e=<epoch>", repl.rs); 0 from heads that do not send one
    pub epoch: u64,
}

/// "ck/1 <name> <ip>:<port>[ e=<epoch>][ relay]"; the old "ck/1 <name>" means the sender is the head.
pub(crate) fn parse_beacon(text: &str, from: std::net::IpAddr) -> Option<Beacon> {
    let mut rest = text.strip_prefix("ck/1 ")?.trim();
    let (mut relayed, mut epoch) = (false, 0);
    loop {
        // trailing tokens in any order
        if let Some(r) = rest.strip_suffix(" relay") {
            (rest, relayed) = (r.trim_end(), true);
        } else if let Some((r, e)) = rest.rsplit_once(" e=").filter(|(_, e)| e.parse::<u64>().is_ok()) {
            (rest, epoch) = (r.trim_end(), e.parse().unwrap());
        } else {
            break;
        }
    }
    Some(match rest.rsplit_once(' ') {
        Some((name, last)) if last.parse::<SocketAddr>().is_ok() => Beacon { name: name.trim().into(), addr: last.into(), relayed, epoch },
        _ => Beacon { name: rest.into(), addr: format!("{from}:{}", port()), relayed, epoch },
    })
}

/// Beacons heard lately: (name, head address, when, epoch). One entry per (name, address): after a failover two heads may beacon.
static SEEN: Mutex<Vec<(String, String, Instant, u64)>> = Mutex::new(Vec::new());

/// The head to follow: highest epoch among the beacons of cluster `name` heard in the last 5 s (ties: the newest).
fn best_head(seen: &[(String, String, Instant, u64)], name: &str) -> Option<(String, u64)> {
    seen.iter().filter(|e| e.0 == name && e.2.elapsed() < Duration::from_secs(5)).max_by_key(|e| (e.3, e.2)).map(|e| (e.1.clone(), e.3))
}

/// Start (once per process) the thread that owns the discovery socket. With `relay` it re-broadcasts each head
/// beacon on this PC's subnet once a second, marked " relay" so relays do not pass it on again (FLT-05).
static NO_DISC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn listen_once(relay: bool) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let s = match UdpSocket::bind(("0.0.0.0", disc_port())) {
            Ok(s) => s,
            Err(e) => {
                // ponytail: the head on this PC holds the port (it listens for higher epochs); use that head.
                // Real fix if it matters: SO_REUSEADDR via a socket crate.
                println!("UDP {}: {e}; a head on this PC holds it, using 127.0.0.1:{}", disc_port(), crate::proto::port());
                NO_DISC.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        };
        let tx = relay.then(|| UdpSocket::bind("0.0.0.0:0").ok().filter(|t| t.set_broadcast(true).is_ok())).flatten();
        thread::spawn(move || {
            let mut last_relay: HashMap<String, Instant> = HashMap::new();
            let mut b = [0u8; 512];
            while let Ok((n, from)) = s.recv_from(&mut b) {
                let Some(bc) = std::str::from_utf8(&b[..n]).ok().and_then(|t| parse_beacon(t, from.ip())) else { continue };
                {
                    let mut seen = SEEN.lock().unwrap();
                    seen.retain(|e| !(e.0 == bc.name && e.1 == bc.addr) && e.2.elapsed() < Duration::from_secs(30));
                    seen.push((bc.name.clone(), bc.addr.clone(), Instant::now(), bc.epoch));
                }
                if let (Some(tx), false) = (&tx, bc.relayed) {
                    if last_relay.get(&bc.name).is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                        last_relay.insert(bc.name.clone(), Instant::now());
                        let msg = format!("ck/1 {} {} e={} relay", bc.name, bc.addr, bc.epoch);
                        let mut targets = vec!["255.255.255.255".to_string()];
                        if let Some(std::net::IpAddr::V4(ip)) = local_ip() {
                            let o = ip.octets();
                            targets.push(format!("{}.{}.{}.255", o[0], o[1], o[2])); // ponytail: assumes /24, like the head
                        }
                        for t in targets {
                            let _ = tx.send_to(msg.as_bytes(), (t.as_str(), disc_port()));
                        }
                    }
                }
            }
        });
    });
}

/// Wait for a beacon of cluster `name` (heard in the last 5 s); returns the highest-epoch head's "ip:port" and epoch.
fn discover(name: &str) -> (String, u64) {
    listen_once(false);
    if NO_DISC.load(std::sync::atomic::Ordering::Relaxed) {
        return (format!("127.0.0.1:{}", crate::proto::port()), 0);
    }
    println!("looking for head '{name}' on UDP {} ...", disc_port());
    loop {
        if let Some(h) = best_head(&SEEN.lock().unwrap(), name) {
            return h;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn hostport(ip: &str) -> String {
    if ip.contains(':') { ip.into() } else { format!("{ip}:{}", port()) }
}

/// The remembered head address lives next to the node keys.
pub(crate) fn last_head_path() -> std::path::PathBuf {
    let k = crate::identity::key_path("x");
    k.parent().map_or_else(|| "last_head".into(), |d| d.join("last_head"))
}

/// last_head file: "ip:port" or "ip:port e=<epoch>".
pub(crate) fn parse_last_head(t: &str) -> Option<(String, u64)> {
    let mut w = t.split_whitespace();
    let a = w.next()?.to_string();
    Some((a, w.next().and_then(|e| e.strip_prefix("e=")).and_then(|e| e.parse().ok()).unwrap_or(0)))
}

/// Reconnect delay. A session that ran a while (fails = 0) retries within 0..1 s, spread at random so a restarted head
/// is not hit by every PC in the same millisecond. Further failures back off fast: 0.25, 0.5, 1, 2, 4 s (x 0.75..1.25).
fn reconnect_delay(fails: u32) -> Duration {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()) ^ std::process::id().wrapping_mul(2654435761);
    if fails == 0 {
        return Duration::from_millis((n % 1000) as u64);
    }
    Duration::from_millis((250u64 << (fails - 1).min(4)) * (750 + (n % 500) as u64) / 1000)
}

fn connect(addr: &str) -> io::Result<TcpStream> {
    let a = addr.to_socket_addrs()?.next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    TcpStream::connect_timeout(&a, Duration::from_secs(3))
}

/// Chunks (job, start) being computed in this process, with how many agents run each (virtual agents share one process).
static RUNNING: std::sync::LazyLock<Mutex<HashMap<(u64, u64), u32>>> = std::sync::LazyLock::new(Default::default);

/// RESULT frames sent but not yet ACKed by a head (HA-04), oldest first. Bounded: past the limits the oldest are dropped.
struct Unacked {
    next: u64,
    by_seq: BTreeMap<u64, ((u64, u64, u64), Vec<u8>)>,
    idx: HashMap<(u64, u64, u64), u64>,
    files: HashMap<(u64, u64, u64), Vec<u8>>, // CTRL {"files"} that goes before a resent RESULT (JOB-08)
    bytes: usize,
    max: (usize, usize), // (0, 0) = read the tunables on every add
}

impl Unacked {
    fn new(max_n: usize, max_bytes: usize) -> Self {
        Unacked { next: 0, by_seq: BTreeMap::new(), idx: HashMap::new(), files: HashMap::new(), bytes: 0, max: (max_n, max_bytes) }
    }
    fn limits(&self) -> (usize, usize) {
        if self.max == (0, 0) { (crate::tune::n("unacked_max") as usize, (crate::tune::get("unacked_max_mb") * 1048576.0) as usize) } else { self.max }
    }
    fn add(&mut self, key: (u64, u64, u64), payload: Vec<u8>) {
        self.ack(key);
        self.bytes += payload.len();
        self.by_seq.insert(self.next, (key, payload));
        self.idx.insert(key, self.next);
        self.next += 1;
        let (max_n, max_bytes) = self.limits();
        while self.by_seq.len() > max_n || self.bytes > max_bytes {
            let Some((_, (k, p))) = self.by_seq.pop_first() else { break };
            self.idx.remove(&k);
            self.files.remove(&k);
            self.bytes -= p.len();
            crate::say!("agent.unacked_drop", "unacked results over the limit: dropped the oldest ({}, {}..{})", k.0, k.1, k.2);
        }
    }
    fn ack(&mut self, key: (u64, u64, u64)) -> bool {
        self.files.remove(&key);
        let Some(s) = self.idx.remove(&key) else { return false };
        if let Some((_, p)) = self.by_seq.remove(&s) {
            self.bytes -= p.len();
        }
        true
    }
    /// Payloads to resend after a reconnect, oldest first.
    fn payloads(&self) -> impl Iterator<Item = (Option<&Vec<u8>>, &Vec<u8>)> {
        self.by_seq.values().map(|(k, p)| (self.files.get(k), p))
    }
}

/// `{"cap_threads": N | null}` -> Some(Some(N)) / Some(None); anything else -> None.
fn parse_cap(p: &[u8]) -> Option<Option<u32>> {
    let v: serde_json::Value = serde_json::from_slice(p).ok()?;
    v.get("cap_threads").map(|c| c.as_u64().map(|n| n as u32))
}

/// Magic packet for Wake-on-LAN: 6 x 0xFF then the MAC 16 times. None if `mac` is not 6 hex pairs.
pub fn magic_packet(mac: &str) -> Option<[u8; 102]> {
    let parts: Vec<u8> = mac.split(|c| c == ':' || c == '-').map(|h| u8::from_str_radix(h, 16).ok()).collect::<Option<_>>()?;
    if parts.len() != 6 {
        return None;
    }
    let mut p = [0xFFu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&parts);
    }
    Some(p)
}

/// Broadcast a magic packet per MAC to 255.255.255.255:9 and the /24 broadcast of our address. ponytail: assumes a /24.
pub fn send_wol(macs: &[String]) {
    let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return };
    let _ = s.set_broadcast(true);
    let mut dst = vec!["255.255.255.255:9".to_string()];
    if let Some(std::net::IpAddr::V4(ip)) = local_ip() {
        let o = ip.octets();
        dst.push(format!("{}.{}.{}.255:9", o[0], o[1], o[2]));
    }
    for m in macs {
        match magic_packet(m) {
            Some(p) => {
                for d in &dst {
                    let _ = s.send_to(&p, d.as_str());
                }
                crate::say!("power.wol", "power: wol magic packet for {m} sent to {}", dst.join(", "));
            }
            None => crate::say!("power.wol", "power: wol: bad mac {m}"),
        }
    }
}

fn suspend() {
    if std::env::var_os("CK_POWER_DRY").is_some_and(|v| v == "1") {
        crate::say!("power.sleep", "power: would sleep now");
        return;
    }
    crate::say!("power.sleep", "power: sleeping now");
    let (prog, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pmset", &["sleepnow"])
    } else if cfg!(windows) {
        ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"])
    } else {
        ("systemctl", &["suspend"])
    };
    if let Err(e) = std::process::Command::new(prog).args(args).status() {
        crate::warnl!("power.sleep_failed", "power: suspend failed: {e}");
    }
}

/// HA-10: start `ck head --standby --head ADDR --no-local-worker` beside this agent, once per process.
fn start_standby(head: &str, w: &Mutex<Out>) {
    static STARTED: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();
    let mut g = STARTED.get_or_init(Default::default).lock().unwrap();
    if let Some(c) = g.as_mut() {
        if c.try_wait().ok().flatten().is_none() {
            return; // already running
        }
    }
    let data = std::path::Path::new(&std::env::var("CK_DATA").unwrap_or_else(|_| "ck-data".into())).join("standby");
    // T55: STARTED is per process, so after a kill -9 / restart of the agent the old standby (still running, same database
    // directory) was joined by a second one. A pid file in the data dir says one is already alive.
    let pidf = data.join("standby.pid");
    if cfg!(unix)
        && std::fs::read_to_string(&pidf).ok().is_some_and(|p| {
            std::process::Command::new("kill").args(["-0", p.trim()]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
        })
    {
        return;
    }
    let r = std::env::current_exe().and_then(|exe| {
        std::fs::create_dir_all(&data)?;
        std::process::Command::new(exe)
            .args(["head", "--standby", "--head", head, "--no-local-worker"])
            .env("CK_DATA", &data)
            .stdin(std::process::Stdio::null())
            .spawn()
    });
    let body = match r {
        Ok(c) => {
            crate::say!("agent.standby_start", "standby: started pid {} for head {head}", c.id());
            let _ = std::fs::write(&pidf, c.id().to_string());
            *g = Some(c);
            r#"{"standby_started":true}"#.to_string()
        }
        Err(e) => {
            crate::warnl!("agent.standby_start", "standby: failed to start: {e}");
            serde_json::json!({"standby_failed": e.to_string()}).to_string()
        }
    };
    let _ = w.lock().unwrap().put(CTRL, body.as_bytes());
}

/// CTRL keys wave 7: power / wol / standby_of (head -> agent).
fn power_ctrl(p: &[u8], w: &Mutex<Out>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(p) else { return };
    if v.get("power").and_then(|x| x.as_str()) == Some("sleep") {
        suspend();
    }
    if let Some(l) = v.get("wol").and_then(|x| x.as_array()) {
        let macs: Vec<String> = l.iter().filter_map(|m| m.as_str().map(String::from)).collect();
        thread::spawn(move || send_wol(&macs));
    }
    if let Some(h) = v.get("standby_of").and_then(|x| x.as_str()) {
        start_standby(h, w);
    }
}

/// Threads a chunk may use under cap `cap` (0 = none) on a PC with `threads`; None = the global pool.
fn pool_size(cap: u32, threads: u32) -> Option<usize> {
    (cap > 0).then(|| cap.min(threads).max(1) as usize)
}

// Sampled once a second in the background and sent with every RESULT.
static CPU_X10: AtomicU64 = AtomicU64::new(0);
static MEM_USED: AtomicU64 = AtomicU64::new(0);
static MEM_TOTAL: AtomicU64 = AtomicU64::new(0);

fn sampler() {
    let mut sys = sysinfo::System::new();
    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        CPU_X10.store((sys.global_cpu_usage() * 10.0) as u64, Relaxed);
        MEM_USED.store(sys.used_memory(), Relaxed);
        MEM_TOTAL.store(sys.total_memory(), Relaxed);
        thread::sleep(Duration::from_secs(1));
    }
}

pub fn worker(head: Option<String>, name: &str, threads: usize, use_gpu: bool) {
    worker_as(head, name, threads, use_gpu, None)
}

/// `virt` = Some(i): logical agent i of a --virtual process (node_id gets "/v<i>").
fn worker_as(head: Option<String>, name: &str, threads: usize, use_gpu: bool, virt: Option<usize>) {
    if use_gpu {
        #[cfg(feature = "gpu")]
        match crate::kernels::GPU.get_or_init(crate::gpu::Gpu::new) {
            Some(g) => println!("GPU: {} (SHA jobs run on it)", g.name),
            None => println!("GPU: none found, using the CPU"),
        }
        #[cfg(not(feature = "gpu"))]
        println!("GPU: this ck was built without the gpu feature, using the CPU");
    }
    crate::blobs::start_server(); // once per process; virtual agents share it
    let _ = crate::blobs::cached(); // hash-check the blob cache once now, not inside the first session where it would delay heartbeats
    static SAMPLER: Once = Once::new();
    SAMPLER.call_once(|| {
        thread::spawn(sampler);
    });
    let mut first = true;
    let unacked = Arc::new(Mutex::new(Unacked::new(0, 0))); // survives reconnects, even to another head
    // Round 4: CK_HEADS="ip:port,ip:port" lists more heads to try in turn when no beacons can be heard (one Mac, routed VLANs).
    let heads: Vec<String> = {
        let mut v: Vec<String> = head.iter().cloned().collect();
        for h in std::env::var("CK_HEADS").unwrap_or_default().split(',').map(str::trim).filter(|h| !h.is_empty()) {
            if !v.iter().any(|x| x == h) {
                v.push(h.to_string());
            }
        }
        if v.len() > 1 { v } else { vec![] }
    };
    let mut hi = 0usize;
    let mut fails = 0u32; // consecutive short or failed sessions: see reconnect_delay (0..1 s, then 0.25 s doubling to 4 s)
    loop {
        let began = Instant::now();
        // after a restart try the head we knew before waiting for a beacon
        let remembered = if first { std::fs::read_to_string(last_head_path()).ok().and_then(|t| parse_last_head(&t)) } else { None };
        first = false;
        let cur = if heads.is_empty() { head.clone() } else { Some(heads[hi % heads.len()].clone()) };
        let (ip, epoch) = match (&cur, remembered) {
            (Some(h), _) => (h.clone(), 0),
            (None, Some(r)) => r,
            (None, None) => discover(name),
        };
        match connect(&hostport(&ip)) {
            Ok(s) => {
                crate::say!("agent.connect", "connected to head {ip} ({threads} threads)");
                // the epoch to beat: what the beacon of this address says now, else what we knew
                let epoch = SEEN.lock().unwrap().iter().filter(|e| e.0 == name && e.1 == hostport(&ip) && e.2.elapsed() < Duration::from_secs(5)).map(|e| e.3).max().unwrap_or(epoch);
                if head.is_none() {
                    let p = last_head_path();
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let _ = std::fs::write(p, format!("{} e={epoch}", hostport(&ip)));
                }
                let follow = head.is_none().then(|| (name.to_string(), hostport(&ip), epoch));
                if let Err(e) = work(s, threads as u32, virt, unacked.clone(), follow) {
                    crate::warnl!("agent.head_lost", "lost head {ip}: {e}");
                    if e.kind() == io::ErrorKind::PermissionDenied {
                        thread::sleep(Duration::from_secs(28)); // refused: do not hammer the head, an operator has to act
                    }
                }
            }
            Err(e) => crate::warnl!("agent.unreachable", "cannot reach head {ip}: {e}"),
        }
        hi += 1; // next head in CK_HEADS (a good session ends only when the head is gone or fenced)
        fails = if began.elapsed() > Duration::from_secs(20) { 0 } else { (fails + 1).min(5) };
        thread::sleep(reconnect_delay(fails));
    }
}

/// Secured fleet, agent side: answer CHALLENGE with the saved key, or take the KEY a join code earns and save it.
fn admit(s: &mut TcpStream, node_id: &str, kp: &std::path::Path) -> io::Result<(FrameMac, FrameMac)> {
    let bad = |k, m: String| io::Error::new(k, m);
    match recv(s)? {
        (CHALLENGE, n) => {
            let key = auth::load_key(kp).map_err(|e| bad(io::ErrorKind::PermissionDenied, format!("head wants a proof but there is no usable key at {}: {e}", kp.display())))?;
            send(s, PROOF, &auth::respond(&key, &n, node_id))?;
            ack(s, &key)
        }
        (KEY, key) => {
            if let Some(d) = kp.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            auth::save_key(kp, &key)?;
            crate::say!("agent.admitted", "joined: node key saved to {}", kp.display());
            ack(s, &key)
        }
        (DENIED, m) => Err(bad(io::ErrorKind::PermissionDenied, format!("head refused this PC: {}", String::from_utf8_lossy(&m)))),
        _ => Err(bad(io::ErrorKind::InvalidData, "unexpected reply during admission".into())),
    }
}

/// The head answers a good admission with one sealed HEARTBEAT, a refusal with DENIED. Returns (send mac, receive mac).
fn ack(s: &mut TcpStream, key: &[u8]) -> io::Result<(FrameMac, FrameMac)> {
    let mut rx = FrameMac::new(key);
    let (k, mut p) = recv(s)?;
    if k == DENIED {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("head refused this PC: {}", String::from_utf8_lossy(&p))));
    }
    let tag = p.split_off(p.len().saturating_sub(40));
    if k != HEARTBEAT || !rx.open(k, &p, &tag) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "head did not confirm admission"));
    }
    Ok((FrameMac::new(key), rx))
}

/// Presence (SCH-10) and calibration (FLT-06) frames for one connection; ends when a send fails (connection gone).
fn extras(w: Arc<Mutex<Out>>) {
    let ctrl = |body: String| w.lock().unwrap().put(CTRL, body.as_bytes()).is_ok();
    let on = crate::presence::enabled();
    let mut h = crate::presence::Hysteresis::default();
    let t0 = Instant::now();
    // ponytail: calibration shares the CPUs with chunks already running, so a join under load reads low; once per process
    static CALIB: OnceLock<f64> = OnceLock::new();
    let mut calib_sent = false;
    let mut next_probe = Instant::now();
    static RAPL: OnceLock<Option<Mutex<crate::presence::Rapl>>> = OnceLock::new();
    let mut next_energy = Instant::now();
    loop {
        if on && Instant::now() >= next_probe {
            next_probe = Instant::now() + crate::tune::secs("presence_probe_s");
            if let Some(b) = h.step(crate::presence::busy(), t0.elapsed().as_secs(), crate::presence::get_or_env("presence_idle_s", "CK_PRESENCE_IDLE_SECS") as u64) {
                crate::say!("agent.presence", "presence: {}", if b { "busy" } else { "idle" });
                if !ctrl(format!(r#"{{"presence":"{}"}}"#, if b { "busy" } else { "idle" })) {
                    return;
                }
            }
        }
        if !calib_sent {
            calib_sent = true;
            let f = *CALIB.get_or_init(|| crate::kernels::calibrate_mhs(crate::tune::get("calib_s")));
            crate::say!("agent.calibration", "calibration: {f:.1} MH/s");
            if !ctrl(format!(r#"{{"calib_mhs":{f}}}"#)) {
                return;
            }
        }
        // OBS-03: package energy every 10 s, when this PC exposes RAPL (one reader per process: virtual agents share it)
        if let Some(r) = RAPL.get_or_init(|| crate::presence::Rapl::open().map(Mutex::new)) {
            if Instant::now() >= next_energy {
                next_energy = Instant::now() + crate::tune::secs("energy_every_s");
                let j = r.lock().unwrap().joules();
                if !ctrl(format!(r#"{{"energy_j":{j:.1}}}"#)) {
                    return;
                }
            }
        } else if !on && calib_sent {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

type Chunks = Arc<Mutex<HashSet<(u64, u64, u64)>>>;

/// Dropped when a chunk is finished or skipped: forget it in the live and cancelled sets.
struct Finished(Chunks, Chunks, (u64, u64, u64));
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.lock().unwrap().remove(&self.2);
        self.1.lock().unwrap().remove(&self.2);
    }
}

/// "unacked results: 0" at most every 5 s (it drains after every chunk).
fn log_drained() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if now >= LAST.load(Relaxed) + 5 {
        LAST.store(now, Relaxed);
        crate::say!("agent.unacked", "unacked results: 0");
    }
}

/// FLT-07: "this is the PC": log it, ring the terminal bell, and on Linux write a banner to /dev/tty1 when that is writable.
/// macOS: no `say`, nothing audible beyond the bell.
fn identify(secs: u64) {
    crate::say!("agent.identify", "identify requested for {secs} s: this is PC {}", crate::identity::host());
    eprint!("\x07");
    #[cfg(target_os = "linux")]
    if let Ok(mut t) = std::fs::OpenOptions::new().write(true).open("/dev/tty1") {
        use std::io::Write;
        let _ = write!(t, "\x07\r\n*** cluster-kit: this PC is {} (identify, {secs} s) ***\r\n", crate::identity::host());
    }
}

/// Upload the chunk's CK_OUT files; the CTRL frame that reports them to the head (None = no files).
fn upload_outputs(job: u64, a: u64, b: u64, d: &crate::kernels::OutDir) -> Result<Option<Vec<u8>>, (bool, String)> {
    let files = d.files(crate::tune::n("out_files_mb")).map_err(|e| (false, e))?;
    if files.is_empty() {
        return Ok(None);
    }
    let mut v = vec![];
    for (name, path, _) in files {
        let (sha, size) = crate::blobs::upload_file(job, &path).map_err(|e| (true, format!("{name}: {e}")))?;
        v.push(serde_json::json!({"name": name, "sha": sha, "size": size}));
    }
    crate::lgc!("chunk.files", {"job": job, "a": a, "b": b, "files": v.len()});
    Ok(Some(serde_json::json!({"files": {"job": job, "a": a, "b": b, "files": v}}).to_string().into_bytes()))
}

/// Round 4: newest signed head proof (epoch, text) any head sent us; sent back in HELLO so a stale head can fence itself.
static HEAD_PROOF: Mutex<(u64, String)> = Mutex::new((0, String::new()));

fn proof_path() -> std::path::PathBuf {
    last_head_path().with_file_name("last_proof")
}

fn seen_proof() -> Option<String> {
    let mut g = HEAD_PROOF.lock().unwrap();
    if g.0 == 0 {
        // after a restart: the file ("<epoch> <proof>")
        if let Some((e, p)) = std::fs::read_to_string(proof_path()).ok().and_then(|t| t.split_once(' ').map(|(e, p)| (e.parse().unwrap_or(0), p.trim().to_string()))) {
            *g = (e, p);
        }
    }
    (g.0 > 0).then(|| g.1.clone())
}

/// CTRL {"epoch": E, "proof": text} from a head: keep the highest.
fn note_proof(v: &serde_json::Value) {
    let (Some(e), Some(p)) = (v["epoch"].as_u64(), v["proof"].as_str()) else { return };
    let mut g = HEAD_PROOF.lock().unwrap();
    if e > g.0 && p.len() < 400 {
        *g = (e, p.to_string());
        let _ = std::fs::write(proof_path(), format!("{e} {p}"));
    }
}

fn work(mut s: TcpStream, threads: u32, virt: Option<usize>, unacked: Arc<Mutex<Unacked>>, follow: Option<(String, String, u64)>) -> io::Result<()> {
    // HA-01: a head silent for LOST_AFTER (frozen, cable pulled) errors the reader out.
    // ponytail: with --spin the socket is non-blocking and there is no timeout.
    if !SPIN.load(Relaxed) {
        s.set_read_timeout(Some(lost_after()))?;
    }
    // a head that stops reading (half-open link, full window) must not block a send forever
    s.set_write_timeout(Some(lost_after().max(Duration::from_secs(10))))?;
    let node_id = match virt {
        Some(i) => format!("{}/v{i}", crate::identity::node_id()),
        None => crate::identity::node_id(),
    };
    if let Ok(a) = s.peer_addr() {
        *crate::blobs::HEAD_IP.lock().unwrap() = a.ip().to_string();
    }
    let secure = !fleet_open();
    let kp = crate::identity::key_path(&node_id);
    let join_code = if secure && !kp.exists() { join_code() } else { None };
    let hello = Hello { node_id: node_id.clone(), host: crate::identity::host(), virtual_node: virt.is_some(), inv: crate::identity::inventory(), join_code, seen_proof: seen_proof() };
    send(&mut s, HELLO_W, &hello_payload(threads, &hello))?;
    let (mac_w, mut mac_r) = if secure { let (w, r) = admit(&mut s, &node_id, &kp)?; (Some(w), Some(r)) } else { (None, None) };
    let sock = s.try_clone()?;
    let (mut r, w) = conn(s)?;
    let w = Arc::new(Mutex::new(Out { s: w, mac: mac_w }));
    let send_w = |kind: u8, p: &[u8]| w.lock().unwrap().put(kind, p);
    CANCELLED.store(0, Relaxed); // job ids restart with each head
    crate::kernels::reset_piece_costs();
    CANCELLED_RANGE.0.store(u64::MAX, Relaxed);
    // HA-04: results the previous head never ACKed go first; the head dedups them and ACKs.
    // Each RESULT is also a request, so the pipeline fills from them. ponytail: still PIPELINE extra REQs; a few surplus chunks wait in the queue.
    {
        let u = unacked.lock().unwrap();
        if !u.by_seq.is_empty() {
            crate::say!("agent.resend", "resending {} un-ACKed results", u.by_seq.len());
        }
        for (f, p) in u.payloads() {
            if let Some(f) = f {
                send_w(CTRL, f)?;
            }
            send_w(RESULT, p)?;
        }
    }
    for _ in 0..pipeline() {
        send_w(REQ, &[])?;
    }
    crate::blobs::report_have(&w, serde_json::json!({})); // which blobs this PC caches, and its blob port (virtual agents: the shared cache)
    // HA-06: a head with a higher epoch beacons: drop this one and move (un-ACKed results go along).
    if let Some((name, addr, epoch)) = follow {
        let sock = sock.try_clone()?;
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(1));
            if sock.peer_addr().is_err() {
                return;
            }
            if let Some((a, e)) = best_head(&SEEN.lock().unwrap(), &name).filter(|(_, e)| *e > epoch) {
                crate::say!("agent.head_switch", "head {a} has epoch {e}, higher than {addr} ({epoch}): moving");
                let _ = sock.shutdown(std::net::Shutdown::Both);
                return;
            }
        });
    }
    if virt.is_none() {
        let w3 = w.clone();
        thread::spawn(move || extras(w3));
    }
    let w2 = w.clone();
    thread::spawn(move || {
        // ends when work() shuts the socket down and the send fails
        while {
            thread::sleep(heartbeat_every());
            w2.lock().unwrap().put(HEARTBEAT, &[]).is_ok()
        } {}
    });
    // Reader thread: queues TASKs and applies CANCEL at once, even mid-chunk. A read error (incl. timeout) is passed on.
    let (tx, tasks) = mpsc::channel();
    let cap = Arc::new(AtomicU32::new(0)); // 0 = no cap (ORG-05)
    let cap_r = cap.clone();
    let skip: Arc<Mutex<HashSet<(u64, u64, u64)>>> = Arc::default(); // CANCEL_RANGE for chunks queued or running here
    let live: Arc<Mutex<HashSet<(u64, u64, u64)>>> = Arc::default(); // chunks received and not finished: a CANCEL_RANGE for any other is stale
    let cur: Arc<Mutex<Option<(u64, u64, u64)>>> = Arc::default(); // the chunk this agent computes now
    let (skip_r, unacked_r, cur_r, live_r) = (skip.clone(), unacked.clone(), cur.clone(), live.clone());
    let w_r = w.clone();
    let sock_r = sock.try_clone()?;
    thread::spawn(move || loop {
        match recv_any(&mut r, &mut mac_r) {
            Ok((CANCEL, p)) => CANCELLED.store(u64_at(&p, 0).unwrap_or(0), Relaxed),
            Ok((CANCEL_RANGE, p)) => {
                if let (Ok(j), Ok(a), Ok(b)) = (u64_at(&p, 0), u64_at(&p, 8), u64_at(&p, 16)) {
                    if live_r.lock().unwrap().contains(&(j, a, b)) {
                        skip_r.lock().unwrap().insert((j, a, b));
                    }
                    // stop it mid-chunk only if it is ours and no other agent of this process runs the same chunk (the kernels' flag is per process)
                    if *cur_r.lock().unwrap() == Some((j, a, b)) && RUNNING.lock().unwrap().get(&(j, a)) == Some(&1) {
                        CANCELLED_RANGE.1.store(a, Relaxed);
                        CANCELLED_RANGE.0.store(j, Relaxed);
                    }
                }
            }
            Ok((ACK, p)) => {
                if let (Ok(j), Ok(a), Ok(b)) = (u64_at(&p, 0), u64_at(&p, 8), u64_at(&p, 16)) {
                    let mut u = unacked_r.lock().unwrap();
                    if u.ack((j, a, b)) && u.by_seq.is_empty() {
                        log_drained();
                    }
                }
            }
            Ok((CTRL, p)) => {
                if let Some(t) = serde_json::from_slice::<serde_json::Value>(&p).ok().and_then(|v| v.get("tune").cloned()) {
                    crate::tune::apply_remote(&t);
                }
                power_ctrl(&p, &w_r);
                crate::kernels::note_job_ctrl(&p);
                if let Some(secs) = serde_json::from_slice::<serde_json::Value>(&p).ok().and_then(|v| v.get("identify").and_then(|x| x.as_u64())) {
                    identify(secs);
                }
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&p) {
                    note_proof(&v);
                    crate::blobs::agent_ctrl(&v, &w_r); // virtual agents too (shared cache; fetch() serialises per sha)
                }
                if let Some(c) = parse_cap(&p) {
                    cap_r.store(c.unwrap_or(0), Relaxed);
                    match c {
                        Some(n) => crate::say!("agent.cap", "cap: chunks run on at most {n} thread(s)"),
                        None => crate::say!("agent.cap", "cap lifted: chunks use all {threads} threads"),
                    }
                }
            }
            Ok((TASK, p)) => {
                if let (Ok(j), Ok(a), Ok(b)) = (u64_at(&p, 0), u64_at(&p, 8), u64_at(&p, 16)) {
                    live_r.lock().unwrap().insert((j, a, b));
                }
                if tx.send(Ok(p)).is_err() {
                    break;
                }
            }
            Ok(_) => {} // HEARTBEAT and anything unknown
            Err(e) => {
                let _ = sock_r.shutdown(std::net::Shutdown::Both); // unblocks a send stuck on a dead link
                let _ = tx.send(Err(e));
                break;
            }
        }
    });
    let mut pools: HashMap<usize, rayon::ThreadPool> = HashMap::new(); // per agent, so virtual agents do not share a pool
    let res = (|| {
        for p in tasks {
            let p = p?;
            let (job, a, b) = (u64_at(&p, 0)?, u64_at(&p, 8)?, u64_at(&p, 16)?);
            let _done = Finished(live.clone(), skip.clone(), (job, a, b));
            if CANCELLED.load(Relaxed) == job || skip.lock().unwrap().contains(&(job, a, b)) {
                send_w(REQ, &[])?; // skip it, but keep the pipeline full
                continue;
            }
            CANCELLED_RANGE.0.store(u64::MAX, Relaxed); // a CANCEL_RANGE for an earlier run of this chunk must not stop this run
            let (kind, arg) = (p.get(24).copied().unwrap_or(KIND_SHA), p.get(25..).unwrap_or(&[]));
            let t = Instant::now();
            crate::lgc!("chunk.start", {"job": job, "a": a, "b": b});
            *cur.lock().unwrap() = Some((job, a, b));
            *RUNNING.lock().unwrap().entry((job, a)).or_default() += 1;
            let outdir = if matches!(kind, KIND_CMD | KIND_LIST) { crate::kernels::OutDir::new() } else { None };
            let od = outdir.as_ref().map(|d| d.0.as_path());
            let out = match pool_size(cap.load(Relaxed), threads) {
                None => run_task_out(kind, job, a, b, arg, od),
                Some(n) => {
                    if !pools.contains_key(&n) {
                        match rayon::ThreadPoolBuilder::new().num_threads(n).build() {
                            Ok(p) => {
                                pools.insert(n, p);
                            }
                            Err(e) => return Err(io::Error::other(format!("thread pool: {e}"))),
                        }
                    }
                    pools[&n].install(|| run_task_out(kind, job, a, b, arg, od))
                }
            };
            *cur.lock().unwrap() = None;
            {
                let mut run = RUNNING.lock().unwrap();
                if let Some(n) = run.get_mut(&(job, a)) {
                    *n -= 1;
                    if *n == 0 {
                        run.remove(&(job, a));
                    }
                }
            }
            if CANCELLED.load(Relaxed) == job || skip.lock().unwrap().contains(&(job, a, b)) {
                send_w(REQ, &[])?;
                continue;
            }
            // JOB-08: files the program left in CK_OUT go to the head's blob store before the result is reported
            let (out, files_ctrl) = match (out, &outdir) {
                (Ok(body), Some(d)) => match upload_outputs(job, a, b, d) {
                    Ok(c) => (Ok(body), c),
                    Err(e) => (Err(e), None),
                },
                (o, _) => (o, None),
            };
            drop(outdir);
            let body = match out {
                Ok(body) => body,
                Err((retry, msg)) => {
                    crate::lg!(Warn, "chunk.failed", {"job": job, "a": a, "b": b, "retry": retry, "ms": t.elapsed().as_secs_f64() * 1000.0,
                        "stderr_tail": msg.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect::<String>()});
                    // FAILED doubles as the next request, like RESULT.
                    let mut f = pack(&[job, a, b]);
                    f.push(retry as u8);
                    f.extend_from_slice(msg.as_bytes());
                    send_w(FAILED, &f)?;
                    continue;
                }
            };
            let us = t.elapsed().as_micros() as u64;
            let mut res = pack(&[job, a, b, CPU_X10.load(Relaxed), MEM_USED.load(Relaxed), MEM_TOTAL.load(Relaxed), us]);
            res.extend_from_slice(&body);
            if let Some(c) = &files_ctrl {
                send_w(CTRL, c)?; // before the RESULT on the same connection
            }
            let pending = {
                let mut u = unacked.lock().unwrap();
                u.add((job, a, b), res.clone());
                if let Some(c) = files_ctrl {
                    u.files.insert((job, a, b), c);
                }
                (u.by_seq.len(), u.bytes)
            };
            crate::lgc!("chunk.end", {"job": job, "a": a, "b": b, "ms": us as f64 / 1000.0, "unacked": pending.0, "unacked_bytes": pending.1});
            send_w(RESULT, &res)?;
        }
        Err(io::Error::new(io::ErrorKind::ConnectionAborted, "head closed the connection"))
    })();
    let _ = sock.shutdown(std::net::Shutdown::Both); // stops the heartbeat and reader threads
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tune_skips_head_keys_and_unacked_reads_it_live() {
        crate::tune::apply_remote(&serde_json::json!({"unacked_max": 100.0, "pipeline": 9.0, "calib_s": 0.1}));
        assert_eq!(crate::tune::get("pipeline"), 2.0, "head-scope key ignored");
        assert_eq!(crate::tune::get("calib_s"), 3.0, "out of range ignored");
        let mut u = Unacked::new(0, 0);
        for i in 0..150 {
            u.add((1, i, i + 1), vec![0; 4]);
        }
        assert_eq!(u.by_seq.len(), 100, "changed knob read at use");
        crate::tune::apply_remote(&serde_json::json!({}));
    }

    #[test]
    fn power_ctrl_dry_run_wol_and_standby_failure_report() {
        std::env::set_var("CK_POWER_DRY", "1");
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (mut srv, _) = l.accept().unwrap();
        let (_r, sock) = conn(c).unwrap();
        let w = Mutex::new(Out { s: sock, mac: None });
        power_ctrl(br#"{"power":"sleep","wol":["aa:bb:cc:dd:ee:ff"],"standby_of":"127.0.0.1:1"}"#, &w);
        // the test binary is not `ck`, so the child exits at once, but spawn succeeds and standby_started is reported
        let mut mac = None;
        let (k, body) = crate::head::recv_any(&mut std::io::BufReader::new(&mut srv), &mut mac).unwrap();
        assert_eq!(k, CTRL);
        assert!(String::from_utf8_lossy(&body).contains("standby_started"));
        std::thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn magic_packet_layout() {
        let p = magic_packet("aa:bb:cc:dd:ee:0f").unwrap();
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert!(p[6..].chunks(6).all(|c| c == [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x0f]));
        assert!(magic_packet("aa:bb:cc").is_none() && magic_packet("zz:bb:cc:dd:ee:ff").is_none());
        assert!(magic_packet("AA-BB-CC-DD-EE-FF").is_some());
    }

    #[test]
    fn beacon_and_cap_parsing() {
        let from: std::net::IpAddr = "10.0.0.9".parse().unwrap();
        let b = |n: &str, a: &str, r| Beacon { name: n.into(), addr: a.into(), relayed: r, epoch: 0 };
        assert_eq!(parse_beacon("ck/1 lab 10.1.2.3:7700", from), Some(b("lab", "10.1.2.3:7700", false)));
        assert_eq!(parse_beacon("ck/1 lab 10.1.2.3:7700 relay", from), Some(b("lab", "10.1.2.3:7700", true)));
        assert_eq!(parse_beacon("ck/1 my lab 10.1.2.3:7700", from), Some(b("my lab", "10.1.2.3:7700", false)));
        assert_eq!(parse_beacon("ck/1 lab", from), Some(b("lab", &format!("10.0.0.9:{}", port()), false)), "old form: the sender is the head");
        assert_eq!(parse_beacon("hello", from), None);
        let e = parse_beacon("ck/1 my lab 10.1.2.3:7700 e=7 relay", from).unwrap();
        assert_eq!((e.name.as_str(), e.addr.as_str(), e.relayed, e.epoch), ("my lab", "10.1.2.3:7700", true, 7));
        assert_eq!(parse_beacon("ck/1 lab 10.1.2.3:7700 e=12", from).unwrap().epoch, 12);
        assert_eq!(parse_last_head("10.0.0.1:7700 e=4\n"), Some(("10.0.0.1:7700".into(), 4)));
        assert_eq!(parse_last_head("10.0.0.1:7700"), Some(("10.0.0.1:7700".into(), 0)));
        assert_eq!(parse_cap(br#"{"cap_threads":3}"#), Some(Some(3)));
        assert_eq!(parse_cap(br#"{"cap_threads":null}"#), Some(None));
        assert_eq!(parse_cap(b"{}"), None);
        assert_eq!((pool_size(0, 8), pool_size(1, 8), pool_size(16, 8)), (None, Some(1), Some(8)));
    }

    #[test]
    fn highest_epoch_wins() {
        let now = Instant::now();
        let old = now.checked_sub(Duration::from_secs(9)).unwrap_or(now);
        let seen = vec![
            ("lab".to_string(), "10.0.0.1:7700".to_string(), now, 3),
            ("lab".into(), "10.0.0.2:7700".into(), now, 4),
            ("lab".into(), "10.0.0.3:7700".into(), old, 9), // stale beacon: ignored
            ("other".into(), "10.0.0.4:7700".into(), now, 8),
        ];
        assert_eq!(best_head(&seen, "lab"), Some(("10.0.0.2:7700".into(), 4)));
        assert_eq!(best_head(&seen, "none"), None);
    }

    #[test]
    fn unacked_bounds_and_order() {
        let mut u = Unacked::new(3, 1000);
        for i in 0..5u64 {
            u.add((1, i, i + 1), vec![i as u8; 10]);
        }
        assert_eq!(u.by_seq.len(), 3, "oldest two dropped");
        assert_eq!(u.payloads().map(|(_, p)| p[0]).collect::<Vec<_>>(), vec![2, 3, 4], "resend oldest first");
        assert!(u.ack((1, 3, 4)) && !u.ack((1, 3, 4)) && !u.ack((1, 0, 1)));
        assert_eq!((u.by_seq.len(), u.bytes), (2, 20));
        u.add((1, 2, 3), vec![9; 10]); // same range again replaces, moves to the back
        assert_eq!(u.payloads().map(|(_, p)| p[0]).collect::<Vec<_>>(), vec![4, 9]);
        let mut big = Unacked::new(100, 25);
        for i in 0..4u64 {
            big.add((1, i, i + 1), vec![0; 10]);
        }
        assert_eq!((big.by_seq.len(), big.bytes), (2, 20), "byte bound");
    }
}

#[cfg(test)]
mod backoff_tests {
    #[test]
    fn reconnect_delay_grows_and_is_bounded() {
        assert!(super::reconnect_delay(0).as_millis() < 1000, "first retry within a second");
        for f in 1..8 {
            let d = super::reconnect_delay(f).as_millis();
            let base = 250u128 << (f - 1).min(4);
            assert!(d >= base * 3 / 4 && d <= base * 5 / 4, "fails {f}: {d} ms");
        }
    }
}
