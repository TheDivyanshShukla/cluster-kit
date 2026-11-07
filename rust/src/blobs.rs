//! Bulk distribution (wave 13, docs/BULK.md, docs/API.md "Bulk distribution"): content-addressed blobs, the blob port
//! (server and client), the estimator, the strategy planner and the head's staging loop.
//!
//! Blob port (CK_BLOB_PORT, default CK_PORT+3), one TCP connection per transfer:
//!   client -> server  plain frame K_HELLO = ticket id (ascii). The head hands a ticket (id + 32-byte key) to the fetcher in
//!                     its CTRL "fetch" and to the serving PC in its CTRL "allow": that is the admission, so a PC that was
//!                     never told gets K_DENY, in an open fleet as well as in a secured one. Peers never hold each other's node keys.
//!   server -> client  sealed K_INFO (size u64, piece size u64); from here every frame both ways is sealed with the ticket key
//!   client -> server  K_REQ piece index u64 -> K_DATA piece bytes (or K_NOPE: a blob still downloading, piece not there yet; or K_ERR); also K_PING, K_PROBE_DOWN n, K_PROBE_UP n.
//! The server serves only the sha its ticket names, read from <CK_DATA>/cache/<sha> or <CK_DATA>/blobs/<sha>: it never
//! takes a path from the network. A PC serves at most `blob_peer_fanout` peers at once (K_BUSY beyond that).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::FrameMac;
use crate::head::{Out, State};
use crate::jobs::{now_ms, Ctx};
use crate::proto::{self, recv, recv_sealed, send, send_sealed};

const K_HELLO: u8 = 1;
const K_INFO: u8 = 2;
const K_REQ: u8 = 3;
const K_DATA: u8 = 4;
const K_PING: u8 = 5;
const K_PROBE_DOWN: u8 = 6;
const K_PROBE_UP: u8 = 7;
const K_ACK: u8 = 8;
const K_BUSY: u8 = 9;
const K_DENY: u8 = 10;
const K_ERR: u8 = 11;
const K_NOPE: u8 = 12; // this piece is not here yet (a download in progress serves the pieces it has)
const PROBE: &str = "*probe*";
const TICK_S: f64 = 0.3; // staging loop period: each distribution round pays about one

// ------------------------------------------------------------------ paths --
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("CK_DATA").unwrap_or_else(|_| "ck-data".into()))
}
/// Head store.
pub fn store_dir() -> PathBuf {
    data_dir().join("blobs")
}
/// Agent cache.
pub fn cache_dir() -> PathBuf {
    if crate::lab::active() {
        return store_dir(); // a lab head's fetches from the root land in its blob store: it is the first source for its lab's PCs
    }
    data_dir().join("cache")
}
pub fn blob_port() -> u16 {
    std::env::var("CK_BLOB_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or_else(|| proto::port().wrapping_add(3))
}
pub fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then(|| ())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
fn piece_bytes() -> u64 {
    (crate::tune::get("blob_piece_mb") * 1048576.0) as u64
}
fn info_log(kind: &str, v: Value) {
    crate::log::emit(crate::log::Level::Info, kind, v);
}
fn other(m: impl Into<String>) -> io::Error {
    io::Error::other(m.into())
}

/// Path of a cached or stored blob, if this process has it.
fn find(sha: &str) -> Option<PathBuf> {
    if !valid_sha(sha) {
        return None;
    }
    [cache_dir(), store_dir()].into_iter().map(|d| d.join(sha)).find(|p| p.is_file())
}

// ------------------------------------------------------- piece index files --
/// `<sha>.idx` = piece size u64 LE, then one sha256 per piece.
fn write_idx(path: &Path, piece: u64, hashes: &[[u8; 32]]) -> io::Result<()> {
    let mut b = piece.to_le_bytes().to_vec();
    hashes.iter().for_each(|h| b.extend_from_slice(h));
    fs::write(path, b)
}
fn read_idx(path: &Path) -> Option<(u64, Vec<[u8; 32]>)> {
    let b = fs::read(path).ok()?;
    let piece = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    let h = b[8..].chunks_exact(32).map(|c| c.try_into().unwrap()).collect();
    (piece > 0).then_some((piece, h))
}

/// Hash a reader into (whole sha, piece hashes, size), writing the bytes to `out` as they pass. Stops with an error past `max`.
fn hash_stream(r: &mut dyn Read, mut out: Option<&mut File>, piece: u64, max: u64) -> io::Result<(String, Vec<[u8; 32]>, u64)> {
    // one buffer per piece: the whole-blob hash, the piece hash and the disk write run on different cores
    let (mut whole, mut hashes, mut total) = (Sha256::new(), vec![], 0u64);
    let mut buf = vec![0u8; piece as usize];
    loop {
        let mut n = 0;
        while n < buf.len() {
            let k = r.read(&mut buf[n..])?;
            if k == 0 {
                break;
            }
            n += k;
        }
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max {
            return Err(other(format!("over the limit of {} MB", max >> 20)));
        }
        let b = &buf[..n];
        let (_, (ph, wr)) = rayon::join(
            || whole.update(b),
            || rayon::join(|| <[u8; 32]>::from(Sha256::digest(b)), || out.as_deref_mut().map_or(Ok(()), |f| f.write_all(b))),
        );
        wr?;
        hashes.push(ph);
    }
    Ok((hex(&whole.finalize()), hashes, total))
}

/// sha256 of a whole file, one pass.
fn hash_whole(r: &mut dyn Read) -> io::Result<String> {
    let (mut h, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&buf[..n]);
    }
}

/// Store a blob in `dir` from a stream (PUT /api/v1/blobs): (sha, size, existed). Never held in memory, never stored twice.
pub fn ingest(r: &mut dyn Read, dir: &Path, max: u64) -> io::Result<(String, u64, bool)> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}", hex(&crate::auth::nonce()[..8])));
    let piece = piece_bytes();
    let res = (|| {
        let mut f = File::create(&tmp)?;
        let (sha, hashes, size) = hash_stream(r, Some(&mut f), piece, max)?;
        f.flush()?;
        Ok((sha, hashes, size))
    })();
    let (sha, hashes, size) = match res {
        Ok(x) => x,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let fin = dir.join(&sha);
    if fin.is_file() {
        let _ = fs::remove_file(&tmp);
        return Ok((sha, size, true));
    }
    write_idx(&dir.join(format!("{sha}.idx")), piece, &hashes)?;
    fs::rename(&tmp, &fin)?;
    Ok((sha, size, false))
}

pub fn touch(p: &Path) {
    if let Ok(f) = OpenOptions::new().append(true).open(p) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Blobs in a directory: (sha, size, mtime).
fn scan(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok().filter(|n| valid_sha(n))?;
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some((n, m.len(), m.modified().ok()?))
        })
        .collect()
}

/// A killed fetch or upload leaves `<sha>.part` / `.tmp-*` behind and nothing else ever looks at them: delete those untouched for `max_age`.
pub fn sweep_stale(dir: &Path, max_age: Duration) -> usize {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n.starts_with(".tmp-") || n.ends_with(".part")) && e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|m| m.elapsed().ok()).is_some_and(|a| a > max_age)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

/// Delete the least recently used blobs until the directory holds at most `limit` bytes; never `keep`.
pub fn evict(dir: &Path, limit: u64, keep: &[String]) -> Vec<String> {
    sweep_stale(dir, Duration::from_secs(6 * 3600));
    let mut all = scan(dir);
    let mut total: u64 = all.iter().map(|x| x.1).sum();
    all.sort_by_key(|x| x.2);
    let mut gone = vec![];
    for (sha, size, _) in all {
        if total <= limit {
            break;
        }
        if keep.contains(&sha) {
            continue;
        }
        let _ = fs::remove_file(dir.join(&sha));
        let _ = fs::remove_file(dir.join(format!("{sha}.idx")));
        verified().lock().unwrap().remove(&sha); // or check_job would want a file no chunk asks the head for
        total -= size;
        gone.push(sha);
    }
    gone
}

// ---------------------------------------------------------------- tickets --
#[derive(Clone)]
struct Ticket {
    sha: String,
    key: Vec<u8>,
    exp: Instant,
    uses: u32,
    head: bool, // the head's own ticket: not counted against the fan-out cap
}
static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> = LazyLock::new(Default::default);
/// Bytes this process's blob port has sent (the head's "head bytes sent"), and transfers running now.
pub static SERVED: AtomicU64 = AtomicU64::new(0);
pub static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_PEER: AtomicUsize = AtomicUsize::new(0);
static MAX_ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn new_ticket() -> (String, Vec<u8>) {
    (hex(&crate::auth::nonce()[..8]), crate::auth::nonce().to_vec())
}
/// Admit a transfer of `sha` (or the probe) for the holder of `key`.
pub fn allow(id: &str, key: &[u8], sha: &str, head: bool) {
    let mut g = TICKETS.lock().unwrap();
    g.retain(|_, t| t.exp > Instant::now());
    g.insert(id.into(), Ticket { sha: sha.into(), key: key.to_vec(), exp: Instant::now() + Duration::from_secs(900), uses: 8, head });
}

// ------------------------------------------------- downloads in progress --
/// A blob this process is still downloading: its verified pieces are served to peers (swarm-style) before the whole blob is done.
struct Part {
    path: PathBuf,
    size: u64,
    piece: u64,
    have: Arc<Mutex<Vec<bool>>>,
}
static PARTS: LazyLock<Mutex<HashMap<String, Arc<Part>>>> = LazyLock::new(Default::default);
struct PartGuard(String);
impl Drop for PartGuard {
    fn drop(&mut self) {
        PARTS.lock().unwrap().remove(&self.0);
    }
}

// ------------------------------------------------------------ rate limit --
fn limit_mbps() -> f64 {
    std::env::var("CK_NET_LIMIT_MBPS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0)
}
/// Test aid (CK_NET_LIMIT_MBPS): cap the send rate of one connection so loopback behaves like a LAN.
struct Pace {
    t0: Instant,
    sent: u64,
    mbps: f64,
}
impl Pace {
    fn new() -> Pace {
        Pace { t0: Instant::now(), sent: 0, mbps: limit_mbps() }
    }
    fn after(&mut self, n: usize) {
        nic_wait(n);
        self.sent += n as u64;
        if self.mbps > 0.0 {
            let due = self.sent as f64 * 8.0 / (self.mbps * 1e6);
            let el = self.t0.elapsed().as_secs_f64();
            if due > el {
                thread::sleep(Duration::from_secs_f64(due - el));
            }
        }
    }
}

/// Test aid (CK_NET_NIC_MBPS): one shared send queue for the whole process, like a PC's network card.
static NIC: Mutex<Option<Instant>> = Mutex::new(None);
fn nic_wait(n: usize) {
    let mbps: f64 = std::env::var("CK_NET_NIC_MBPS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    if mbps <= 0.0 {
        return;
    }
    let end = {
        let mut g = NIC.lock().unwrap();
        let end = g.unwrap_or_else(Instant::now).max(Instant::now()) + Duration::from_secs_f64(n as f64 * 8.0 / (mbps * 1e6));
        *g = Some(end);
        end
    };
    thread::sleep(end.saturating_duration_since(Instant::now()));
}

// ----------------------------------------------------------------- server --
static SERVER: Once = Once::new();
static BOUND: AtomicU64 = AtomicU64::new(0);

/// Start the blob port once per process (the head and its local agent share it). 0 if the port is taken.
pub fn start_server() -> u16 {
    SERVER.call_once(|| {
        let port = blob_port();
        // fz1: a taken blob port used to leave BOUND at 0, and the head then told its PCs to fetch from "ip:0" (EADDRNOTAVAIL, retried
        // forever, the job never finished). Take any free port instead: the port is announced to the PCs, which need no fixed number.
        let bound = TcpListener::bind(("0.0.0.0", port)).or_else(|e| {
            crate::warnl!("blob.port", "blob port {port}: {e}; using a free port instead");
            TcpListener::bind(("0.0.0.0", 0))
        });
        match bound {
            Ok(l) => {
                let port = l.local_addr().map(|a| a.port()).unwrap_or(port);
                BOUND.store(port as u64, Relaxed);
                thread::spawn(move || {
                    for s in l.incoming().flatten() {
                        thread::spawn(move || {
                            let _ = handle(s);
                        });
                    }
                });
            }
            Err(e) => crate::warnl!("blob.port", "blob port {port}: {e}; this PC will not serve blobs to peers"),
        }
    });
    BOUND.load(Relaxed) as u16
}

struct Active(bool);
impl Active {
    fn new(peer: bool) -> Active {
        let n = ACTIVE.fetch_add(1, Relaxed) + 1;
        MAX_ACTIVE.fetch_max(n, Relaxed);
        if peer {
            ACTIVE_PEER.fetch_add(1, Relaxed);
        }
        Active(peer)
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Relaxed);
        if self.0 {
            ACTIVE_PEER.fetch_sub(1, Relaxed);
        }
    }
}

fn handle(s: TcpStream) -> io::Result<()> {
    s.set_nodelay(true)?;
    let mut s = s;
    let (k, id) = proto::recv_by(&mut s, 4096, Duration::from_secs(10))?; // unauthenticated: small and with a deadline
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut w = s.try_clone()?;
    let mut r = BufReader::with_capacity(1 << 16, s);
    let t = {
        let mut g = TICKETS.lock().unwrap();
        let id = String::from_utf8_lossy(&id).to_string();
        g.get_mut(&id).filter(|t| k == K_HELLO && t.exp > Instant::now() && t.uses > 0).map(|t| {
            t.uses -= 1;
            t.clone()
        })
    };
    let Some(t) = t else { return send(&mut w, K_DENY, b"no admission") };
    if !t.head && ACTIVE_PEER.load(Relaxed) >= crate::tune::n("blob_peer_fanout") as usize {
        return send(&mut w, K_BUSY, b"busy");
    }
    let _g = Active::new(!t.head);
    let (mut tx, mut rx) = (FrameMac::new(&t.key), FrameMac::new(&t.key));
    let probe = t.sha == PROBE;
    let part = if probe { None } else { PARTS.lock().unwrap().get(&t.sha).cloned() };
    let file = if probe { None } else { find(&t.sha) };
    if !probe && file.is_none() && part.is_none() {
        return Err(other("not cached"));
    }
    let (size, piece) = match (&file, &part) {
        (Some(p), _) => {
            let piece = read_idx(&PathBuf::from(format!("{}.idx", p.display()))).map(|x| x.0).unwrap_or_else(piece_bytes);
            (p.metadata()?.len(), piece)
        }
        (None, Some(pa)) => (pa.size, pa.piece),
        _ => (0, piece_bytes()),
    };
    send_sealed(&mut w, &mut tx, K_INFO, &proto::pack(&[size, piece]))?;
    let mut pace = Pace::new();
    loop {
        let (k, p) = recv_sealed(&mut r, &mut rx)?;
        match k {
            K_PING => send_sealed(&mut w, &mut tx, K_PING, &p)?,
            K_REQ => {
                let (false, Ok(i)) = (probe, proto::u64_at(&p, 0)) else { return send_sealed(&mut w, &mut tx, K_ERR, b"bad request") };
                let off = i.saturating_mul(piece);
                if size == 0 || off >= size {
                    return send_sealed(&mut w, &mut tx, K_ERR, b"no such piece");
                }
                // the whole blob, or (still downloading) a piece that is verified and written
                let path = match find(&t.sha) {
                    Some(p) => p,
                    None => match &part {
                        Some(pa) if pa.have.lock().unwrap().get(i as usize) == Some(&true) => pa.path.clone(),
                        _ => {
                            send_sealed(&mut w, &mut tx, K_NOPE, &[])?;
                            continue;
                        }
                    },
                };
                // the download may have just finished (renamed): then the whole blob is there
                let mut f = File::open(&path).or_else(|_| File::open(find(&t.sha).ok_or_else(|| other("gone"))?))?;
                let mut buf = vec![0u8; piece.min(size - off) as usize];
                f.seek(SeekFrom::Start(off))?;
                f.read_exact(&mut buf)?;
                send_sealed(&mut w, &mut tx, K_DATA, &buf)?;
                pace.after(buf.len()); // counted after the pacing wait so the uplink reading is smooth, not bursty
                SERVED.fetch_add(buf.len() as u64, Relaxed);
            }
            K_PROBE_DOWN if probe => {
                let mut left = proto::u64_at(&p, 0)?;
                let chunk = vec![0x5au8; 1 << 20];
                while left > 0 {
                    let n = left.min(chunk.len() as u64) as usize;
                    send_sealed(&mut w, &mut tx, K_DATA, &chunk[..n])?;
                    pace.after(n);
                    SERVED.fetch_add(n as u64, Relaxed);
                    left -= n as u64;
                }
            }
            K_PROBE_UP if probe => {
                let mut left = proto::u64_at(&p, 0)?;
                while left > 0 {
                    let (k, d) = recv_sealed(&mut r, &mut rx)?;
                    if k != K_DATA {
                        return Err(other("bad probe"));
                    }
                    left = left.saturating_sub(d.len() as u64);
                }
                send_sealed(&mut w, &mut tx, K_ACK, &[])?;
            }
            _ => return send_sealed(&mut w, &mut tx, K_ERR, b"not allowed"),
        }
    }
}

// ----------------------------------------------------------------- client --
#[derive(Clone, Debug)]
pub struct Source {
    pub addr: String,
    pub id: String,
    pub key: Vec<u8>,
    pub head: bool,
}
#[derive(Clone, Debug)]
pub struct Meta {
    pub sha: String,
    pub size: u64,
    pub piece: u64,
    pub hashes: Vec<[u8; 32]>,
}
#[derive(Debug)]
pub struct Got {
    pub bytes: u64,
    pub peer_bytes: u64, // of `bytes`, what came from peers rather than the head
    pub from: String,
    pub ms: f64,
}

type Conn = (BufReader<TcpStream>, TcpStream, FrameMac, FrameMac);

fn open_conn(src: &Source) -> io::Result<(Conn, u64, u64)> {
    let a = src.addr.to_socket_addrs()?.next().ok_or_else(|| other("no address"))?;
    let s = TcpStream::connect_timeout(&a, Duration::from_secs(3))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut w = s.try_clone()?;
    let mut r = BufReader::with_capacity(1 << 16, s);
    send(&mut w, K_HELLO, src.id.as_bytes())?;
    let (k, mut p) = recv(&mut r)?;
    let mut rx = FrameMac::new(&src.key);
    if k != K_INFO {
        return Err(other(format!("{} refused: {}", src.addr, String::from_utf8_lossy(&p))));
    }
    let tag = p.split_off(p.len().saturating_sub(40));
    if !rx.open(k, &p, &tag) {
        return Err(other("bad frame tag from the blob port"));
    }
    let (size, piece) = (proto::u64_at(&p, 0)?, proto::u64_at(&p, 8)?);
    Ok(((r, w, FrameMac::new(&src.key), rx), size, piece))
}

fn piece_range(m: &Meta, i: usize) -> (u64, usize) {
    let off = i as u64 * m.piece;
    (off, m.piece.min(m.size - off) as usize)
}

fn read_piece(f: &mut File, m: &Meta, i: usize) -> io::Result<Vec<u8>> {
    let (off, n) = piece_range(m, i);
    let mut b = vec![0u8; n];
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(&mut b)?;
    Ok(b)
}

/// Take the pieces `src` has: all of them from the head or a finished peer, the ones it holds so far from a peer that is still
/// downloading (K_NOPE for the rest). `start` rotates the order so fetchers of one blob hold different pieces and can trade.
fn pull(f: &mut File, m: &Meta, have: &mut [bool], shared: &Mutex<Vec<bool>>, start: usize, src: &Source) -> io::Result<u64> {
    let ((mut r, mut w, mut tx, mut rx), size, piece) = open_conn(src)?;
    if size != m.size || piece != m.piece {
        return Err(other("the source holds a different blob"));
    }
    let mut got = 0;
    // a window of requests in flight: the source reads and seals the next piece while this side verifies the last
    let want: Vec<usize> = (0..have.len()).map(|j| (start + j) % have.len()).filter(|i| !have[*i]).collect();
    let (mut sent, mut done) = (0, 0);
    while done < want.len() {
        while sent < want.len() && sent - done < 3 {
            send_sealed(&mut w, &mut tx, K_REQ, &proto::pack(&[want[sent] as u64]))?;
            sent += 1;
        }
        let i = want[done];
        done += 1;
        let (k, d) = recv_sealed(&mut r, &mut rx)?;
        if k == K_NOPE {
            continue;
        }
        if k != K_DATA || d.len() != piece_range(m, i).1 || <[u8; 32]>::from(Sha256::digest(&d)) != m.hashes[i] {
            return Err(other(format!("piece {i} from {} is wrong", src.addr)));
        }
        f.seek(SeekFrom::Start(piece_range(m, i).0))?;
        f.write_all(&d)?;
        have[i] = true;
        shared.lock().unwrap()[i] = true;
        got += d.len() as u64;
    }
    Ok(got)
}

/// Fetch a blob into `dir/<sha>`: resumes a broken transfer (pieces already in `<sha>.part` that match their hash are kept),
/// verifies every piece and then the whole sha256, tries the sources in order. Already cached = 0 bytes.
pub fn fetch(dir: &Path, m: &Meta, srcs: &[Source]) -> io::Result<Got> {
    // virtual agents in one process share the cache: one fetch per sha at a time, the others then hit the cache
    static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let lock = INFLIGHT.get_or_init(Default::default).lock().unwrap().entry(m.sha.clone()).or_default().clone();
    let _one = lock.lock().unwrap();
    let t0 = Instant::now();
    fs::create_dir_all(dir)?;
    let fin = dir.join(&m.sha);
    if fin.metadata().is_ok_and(|x| x.len() == m.size) {
        touch(&fin);
        return Ok(Got { bytes: 0, peer_bytes: 0, from: "cache".into(), ms: 0.0 });
    }
    let part = dir.join(format!("{}.part", m.sha));
    let existed = part.metadata().is_ok_and(|x| x.len() == m.size);
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part)?;
    f.set_len(m.size)?;
    let n = m.size.div_ceil(m.piece) as usize;
    if n != m.hashes.len() {
        return Err(other("piece list does not match the size"));
    }
    let mut have = vec![false; n];
    if existed {
        for (i, h) in have.iter_mut().enumerate() {
            *h = <[u8; 32]>::from(Sha256::digest(read_piece(&mut f, m, i)?)) == m.hashes[i];
        }
    }
    // from here peers may take the verified pieces while this download runs
    let shared = Arc::new(Mutex::new(have.clone()));
    // (one fetch per sha runs at a time in a process: INFLIGHT above)
    PARTS.lock().unwrap().entry(m.sha.clone()).or_insert_with(|| Arc::new(Part { path: part.clone(), size: m.size, piece: m.piece, have: shared.clone() }));
    let _unreg = PartGuard(m.sha.clone());
    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as usize) % n.max(1);
    let (mut bytes, mut peer_bytes, mut last) = (0, 0, other("no source"));
    let (mut dead, mut stall) = (vec![false; srcs.len()], Instant::now());
    // sources in order, round after round: a peer still downloading answers only for the pieces it has, so ask again until the
    // blob is whole; a source that errors is dropped; no new piece for 20 s fails the fetch (the head then serves it itself)
    while !have.iter().all(|h| *h) {
        let before = have.iter().filter(|h| **h).count();
        for (k, s) in srcs.iter().enumerate() {
            if dead[k] {
                continue;
            }
            match pull(&mut f, m, &mut have, &shared, start, s) {
                Ok(b) => {
                    bytes += b;
                    if !s.head {
                        peer_bytes += b;
                    }
                }
                Err(e) => (dead[k], last) = (true, e),
            }
            if have.iter().all(|h| *h) {
                break;
            }
        }
        if have.iter().all(|h| *h) {
            break;
        }
        if have.iter().filter(|h| **h).count() > before {
            stall = Instant::now();
        }
        if dead.iter().all(|d| *d) || stall.elapsed() > crate::tune::secs("blob_stall_s") {
            return Err(if dead.iter().all(|d| *d) { last } else { other("peers have not got the missing pieces yet; gave up") });
        }
        thread::sleep(Duration::from_millis(150));
    }
    let from = if peer_bytes * 2 > bytes { "peer" } else { "head" }.to_string();
    f.flush()?;
    f.seek(SeekFrom::Start(0))?;
    let sha = hash_whole(&mut f)?;
    if sha != m.sha {
        let _ = fs::remove_file(&part);
        return Err(other("the whole blob does not match its sha256; discarded"));
    }
    write_idx(&dir.join(format!("{}.idx", m.sha)), m.piece, &m.hashes)?;
    fs::rename(&part, &fin)?;
    Ok(Got { bytes, peer_bytes, from, ms: t0.elapsed().as_secs_f64() * 1000.0 })
}

/// Link probe against the head's blob port: (download Mbit/s, upload Mbit/s, round trip ms).
pub fn probe(src: &Source, mb: f64) -> io::Result<(f64, f64, f64)> {
    let ((mut r, mut w, mut tx, mut rx), _, _) = open_conn(src)?;
    let mut rtt = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        send_sealed(&mut w, &mut tx, K_PING, b"x")?;
        recv_sealed(&mut r, &mut rx)?;
        rtt = rtt.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    let total = (mb * 1048576.0) as u64;
    let t = Instant::now();
    send_sealed(&mut w, &mut tx, K_PROBE_DOWN, &proto::pack(&[total]))?;
    let mut left = total;
    while left > 0 {
        let (_, d) = recv_sealed(&mut r, &mut rx)?;
        left = left.saturating_sub(d.len() as u64);
    }
    let down = total as f64 * 8e-6 / t.elapsed().as_secs_f64().max(1e-6);
    let t = Instant::now();
    send_sealed(&mut w, &mut tx, K_PROBE_UP, &proto::pack(&[total]))?;
    let (chunk, mut pace) = (vec![0x33u8; 1 << 20], Pace::new());
    let mut left = total;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        send_sealed(&mut w, &mut tx, K_DATA, &chunk[..n])?;
        pace.after(n);
        left -= n as u64;
    }
    recv_sealed(&mut r, &mut rx)?;
    let up = total as f64 * 8e-6 / t.elapsed().as_secs_f64().max(1e-6);
    Ok((down, up, rtt))
}

// ------------------------------------------------------------------ agent --
/// Where the agent's head is (for CTRL sources that say "the head").
pub static HEAD_IP: Mutex<String> = Mutex::new(String::new());
/// Downloads in flight in this process -> the other connections that asked for the same blob meanwhile (virtual agents share one process and one
/// cache; each connection must still hear "have" for it, or the head keeps waiting to stage a chunk on a PC that already holds the blob).
static FETCHING: LazyLock<Mutex<HashMap<String, Vec<Arc<Mutex<Out>>>>>> = LazyLock::new(Default::default);
/// (job, blobs, upload capability); `host` empty = the head this agent is connected to (a lab head points its PCs at the root).
type Put = Option<(String, u16, String)>;
static JOB_BLOBS: Mutex<Vec<(u64, Vec<String>, Put)>> = Mutex::new(Vec::new());

/// The upload capability (host, port, token) the head that sent `job` gave this process (a lab head forwards it to its PCs).
pub fn put_for(job: u64) -> Put {
    JOB_BLOBS.lock().unwrap().iter().find(|x| x.0 == job).and_then(|x| x.2.clone())
}

type Stamp = (u64, std::time::SystemTime);
/// name -> (length, mtime) at the moment this process last hashed the file and found it good.
fn verified() -> &'static Mutex<HashMap<String, Stamp>> {
    static OK: OnceLock<Mutex<HashMap<String, Stamp>>> = OnceLock::new();
    OK.get_or_init(Default::default)
}
static RESYNC: AtomicBool = AtomicBool::new(false);
/// True once after a cached blob was found corrupt and deleted: the agent then tells the head what it really has.
pub fn take_resync() -> bool {
    RESYNC.swap(false, Relaxed)
}

/// Does the cached file `dir/n` still hash to its name? Hashed again only when its length or mtime differs from the last good hash
/// (a write, a truncation or a replaced file changes them; env_for re-records its own touch). A bad one is deleted so the head fetches it again.
/// ponytail: rot that changes bytes without changing size or mtime is not seen until restart.
fn verify(dir: &Path, n: &str, len: u64, mtime: std::time::SystemTime) -> bool {
    if verified().lock().unwrap().get(n) == Some(&(len, mtime)) { return true; }
    let good = File::open(dir.join(n)).and_then(|mut f| hash_stream(&mut f, None, 4 << 20, u64::MAX)).is_ok_and(|h| h.0 == n);
    if good { verified().lock().unwrap().insert(n.to_string(), (len, mtime)); } else {
        crate::warnl!("blob.corrupt", "cached blob {} does not match its hash; deleted, will be fetched again", &n[..12]);
        verified().lock().unwrap().remove(n);
        let _ = fs::remove_file(dir.join(n));
        let _ = fs::remove_file(dir.join(format!("{n}.idx")));
        RESYNC.store(true, Relaxed);
    }
    good
}

/// Before a job uses its blobs: re-check each one whose length or mtime changed since it was last hashed (a stat, no read, otherwise).
/// Err names the first bad blob (now deleted); the chunk is retried once the head has fetched it again.
pub fn check_job(job: u64) -> Result<(), String> {
    let Some(l) = JOB_BLOBS.lock().unwrap().iter().find(|x| x.0 == job).map(|x| x.1.clone()) else { return Ok(()) };
    let dir = cache_dir();
    for sha in l.iter().filter(|s| valid_sha(s)) {
        let m = fs::metadata(dir.join(sha)).ok().filter(|m| m.is_file());
        // fz1: the job's list names every blob of the job, but a PC holds only the ones its chunks use (ck.shard places them per chunk).
        // A file this process never verified is not "lost", it is not needed here; failing on it failed every SDK shard job.
        if m.is_none() && !verified().lock().unwrap().contains_key(sha.as_str()) {
            continue;
        }
        if !m.is_some_and(|m| m.modified().is_ok_and(|t| verify(&dir, sha, m.len(), t))) {
            // gone (the sweep above may have deleted it a moment ago) or bad: the head must learn this PC no longer has it
            RESYNC.store(true, Relaxed);
            thread::sleep(crate::tune::secs("heartbeat_s") * 3 / 2); // the heartbeat thread reports it; the retry must reach the head after that
            return Err(format!("blob {} is missing or was corrupt on this PC; the head will send it again", &sha[..12]));
        }
    }
    Ok(())
}

/// Shas in the cache (complete files only). The head trusts this list, so a file whose bytes no longer hash to its name
/// (bit rot, torn write, wrong length) must not be in it.
pub fn cached() -> Vec<String> {
    let dir = cache_dir();
    scan(&dir).into_iter().filter(|(n, len, t)| verify(&dir, n, *len, *t)).map(|x| x.0).collect()
}

pub fn report_have(w: &Mutex<Out>, extra: Value) {
    let mut v = json!({"have": cached(), "blob_port": BOUND.load(Relaxed)});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        e.iter().for_each(|(k, x)| {
            o.insert(k.clone(), x.clone());
        });
    }
    let _ = w.lock().unwrap().put(proto::CTRL, v.to_string().as_bytes());
}

fn sources(v: &Value) -> Vec<Source> {
    let head_ip = HEAD_IP.lock().unwrap().clone();
    v["from"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let key = unhex(s["key"].as_str()?)?;
            let id = s["id"].as_str()?.to_string();
            Some(match (s["addr"].as_str(), s["head"].as_u64()) {
                (Some(a), _) => Source { addr: a.into(), id, key, head: false },
                (None, Some(p)) => Source { addr: format!("{head_ip}:{p}"), id, key, head: true },
                _ => return None,
            })
        })
        .collect()
}

/// CTRL from the head that concerns blobs: fetch, allow, probe, job_blobs.
pub fn agent_ctrl(v: &Value, w: &Arc<Mutex<Out>>) {
    if let Some(a) = v["allow"].as_array() {
        for t in a {
            if let (Some(id), Some(key), Some(sha)) = (t["id"].as_str(), t["key"].as_str().and_then(unhex), t["sha"].as_str()) {
                allow(id, &key, sha, false);
            }
        }
    }
    if let Some(j) = v.get("job_blobs") {
        if let (Some(id), Some(l)) = (j["id"].as_u64(), j["blobs"].as_array()) {
            let mut g = JOB_BLOBS.lock().unwrap();
            g.retain(|x| x.0 != id);
            let put = j["put"]["port"].as_u64().zip(j["put"]["token"].as_str()).map(|(p, t)| (j["put"]["host"].as_str().unwrap_or("").to_string(), p as u16, t.to_string()));
            g.push((id, l.iter().filter_map(|s| s.as_str().map(String::from)).collect(), put));
            if g.len() > 64 {
                g.remove(0);
            }
        }
    }
    if let Some(f) = v.get("fetch") {
        let Some(sha) = f["sha"].as_str().filter(|s| valid_sha(s)).map(String::from) else { return };
        let hashes: Option<Vec<[u8; 32]>> = f["pieces"].as_array().and_then(|a| a.iter().map(|h| unhex(h.as_str()?)?.try_into().ok()).collect());
        let (Some(hashes), Some(size), Some(piece)) = (hashes, f["size"].as_u64(), f["piece"].as_u64().filter(|p| *p > 0)) else { return };
        let srcs = sources(f);
        {
            let mut f = FETCHING.lock().unwrap();
            if let Some(waiting) = f.get_mut(&sha) {
                waiting.push(w.clone()); // already on its way: answer this connection when it lands
                return;
            }
            f.insert(sha.clone(), vec![]);
        }
        let w = w.clone();
        thread::spawn(move || {
            let m = Meta { sha: sha.clone(), size, piece, hashes };
            match fetch(&cache_dir(), &m, &srcs) {
                Ok(g) => {
                    let gone = if crate::lab::active() { crate::lab::blob_landed(&sha, size); vec![] } else { evict(&cache_dir(), (crate::tune::get("blob_cache_gb") * 1073741824.0) as u64, &[sha.clone()]) };
                    info_log("blob.fetch", json!({"sha": sha, "bytes": g.bytes, "peer_bytes": g.peer_bytes, "source": g.from, "ms": g.ms, "evicted": gone.len()}));
                    let waiting = FETCHING.lock().unwrap().remove(&sha).unwrap_or_default();
                    for o in &waiting {
                        report_have(o, json!({}));
                    }
                    report_have(&w, json!({"fetched": {"sha": sha, "bytes": g.bytes, "peer_bytes": g.peer_bytes, "from": g.from, "ms": g.ms}}));
                }
                Err(e) => {
                    crate::warnl!("blob.fetch_failed", "blob {}: {e}", &sha[..12]);
                    let waiting = FETCHING.lock().unwrap().remove(&sha).unwrap_or_default();
                    for o in std::iter::once(&w).chain(waiting.iter()) {
                        let _ = o.lock().unwrap().put(proto::CTRL, json!({"fetch_failed": {"sha": sha, "error": e.to_string()}}).to_string().as_bytes());
                    }
                }
            }
        });
    }
    if let Some(p) = v.get("probe") {
        let (mb, srcs) = (p["mb"].as_f64().unwrap_or(16.0), sources(p));
        let w = w.clone();
        thread::spawn(move || {
            if let Some(Ok((down, up, rtt))) = srcs.first().map(|s| probe(s, mb)) {
                info_log("net.probe", json!({"down_mbps": down, "up_mbps": up, "rtt_ms": rtt, "mb": mb}));
                let _ = w.lock().unwrap().put(proto::CTRL, json!({"net": {"mbps": down, "up_mbps": up, "rtt_ms": rtt}}).to_string().as_bytes());
            } else {
                let _ = w.lock().unwrap().put(proto::CTRL, json!({"net": {"error": "probe failed"}}).to_string().as_bytes());
            }
        });
    }
}

/// Env for a program of `job` on this PC: CK_BLOBS and CK_BLOB_<i> (touches the files so LRU keeps them).
pub fn env_for(job: u64) -> Vec<(String, String)> {
    let Some((l, put)) = JOB_BLOBS.lock().unwrap().iter().find(|x| x.0 == job).map(|x| (x.1.clone(), x.2.clone())) else { return vec![] };
    let dir = cache_dir();
    let mut e = vec![("CK_BLOBS".to_string(), dir.display().to_string())];
    if let Some((host, port, tok)) = put {
        // job-scoped upload capability for bulk results: valid for PUT /api/v1/blobs of this job only, until the job ends
        let host = if host.is_empty() { HEAD_IP.lock().unwrap().clone() } else { host };
        e.push(("CK_BLOB_PUT_URL".into(), format!("http://{host}:{port}/api/v1/blobs")));
        e.push(("CK_BLOB_PUT_TOKEN".into(), tok));
    }
    for (i, sha) in l.iter().enumerate() {
        let p = dir.join(sha);
        touch(&p);
        if let Some(t) = fs::metadata(&p).ok().and_then(|m| m.modified().ok().map(|t| (m.len(), t))) {
            // touch() changed the mtime: keep a verified blob verified under its new stamp
            let mut ok = verified().lock().unwrap();
            if ok.contains_key(sha) { ok.insert(sha.clone(), t); }
        }
        e.push((format!("CK_BLOB_{i}"), p.display().to_string()));
    }
    e
}

/// Agent: upload one output file to the head's blob store with the job's upload capability (JOB-08). Returns (sha, size).
/// ponytail: one connection per file, streamed from disk; no resume (a failed upload fails the chunk, which is retried).
pub fn upload_file(job: u64, path: &Path) -> Result<(String, u64), String> {
    let (host, port, tok) = JOB_BLOBS.lock().unwrap().iter().find(|x| x.0 == job).and_then(|x| x.2.clone()).ok_or("the head sent no upload capability for this job")?;
    let host = if host.is_empty() { HEAD_IP.lock().unwrap().clone() } else { host };
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let addr = (host.as_str(), port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad head address")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("upload: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(120))).ok();
    s.set_write_timeout(Some(Duration::from_secs(120))).ok();
    let head = format!("PUT /api/v1/blobs HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
    s.write_all(head.as_bytes()).and_then(|_| io::copy(&mut f, &mut s)).map_err(|e| format!("upload: {e}"))?;
    let mut raw = vec![];
    s.read_to_end(&mut raw).map_err(|e| format!("upload: {e}"))?;
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("upload: bad reply")?;
    let status: u16 = String::from_utf8_lossy(&raw[..at]).split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let v: Value = serde_json::from_slice(&raw[at + 4..]).unwrap_or(Value::Null);
    if status != 200 {
        return Err(format!("upload refused (HTTP {status}): {}", v["error"]["message"].as_str().unwrap_or("")));
    }
    let sha = v["sha256"].as_str().filter(|s| valid_sha(s)).ok_or("upload: bad reply")?.to_string();
    Ok((sha, v["size"].as_u64().unwrap_or(len)))
}

/// `@blob:<sha>[:offset:len]` -> `<path>` or `<path>\t<offset>\t<len>`; any other item unchanged.
pub fn resolve_item(item: &[u8]) -> Vec<u8> {
    let Some(rest) = item.strip_prefix(b"@blob:") else { return item.to_vec() };
    let rest = String::from_utf8_lossy(rest);
    let mut it = rest.split(':');
    let sha = it.next().unwrap_or("");
    if !valid_sha(sha) {
        return item.to_vec();
    }
    let p = cache_dir().join(sha);
    match (it.next(), it.next()) {
        (Some(o), Some(l)) => format!("{}\t{o}\t{l}", p.display()).into_bytes(),
        _ => p.display().to_string().into_bytes(),
    }
}

/// shas named by `@blob:` items.
pub fn item_blob(item: &str) -> Option<&str> {
    item.strip_prefix("@blob:").and_then(|r| r.split(':').next()).filter(|s| valid_sha(s))
}

/// Every blob an item needs: `@blob:<sha>[:off:len]` (the head turns it into a path) or `@refs:<sha>,<sha>|<payload>` (the SDK:
/// the payload is an opaque pickle that holds handles; only the PC that gets the item needs those blobs).
pub fn item_blobs(item: &str) -> Vec<&str> {
    if let Some(r) = item.strip_prefix("@refs:") {
        return r.split('|').next().unwrap_or("").split(',').filter(|s| valid_sha(s)).collect();
    }
    item_blob(item).into_iter().collect()
}

// ------------------------------------------------- job-scoped upload token --
static SECRET: LazyLock<[u8; 32]> = LazyLock::new(crate::auth::nonce);
/// Capability for one job: valid only for PUT /api/v1/blobs while the job is unfinished (checked by the API). Never the user's token.
pub fn put_token(job: u64) -> String {
    format!("cbp.{job}.{}", hex(&crate::auth::hmac_sha256(&*SECRET, format!("blobput:{job}").as_bytes())))
}
/// The job a valid upload token belongs to.
pub fn check_put_token(tok: &str) -> Option<u64> {
    let job: u64 = tok.strip_prefix("cbp.")?.split('.').next()?.parse().ok()?;
    crate::auth::ct_eq(put_token(job).as_bytes(), tok.as_bytes()).then_some(job)
}

/// Distinct blobs chunk [a, b) needs: the job-wide ones plus the shards its items name.
pub fn chunk_blobs(shared: &[String], shards: &[String], item_sha: &[Vec<u32>], base: u64, a: u64, b: u64) -> Vec<String> {
    let mut v: Vec<String> = shared.to_vec();
    let (lo, hi) = (a.saturating_sub(base), b.saturating_sub(base).min(item_sha.len() as u64));
    for i in lo..hi {
        for k in item_sha[i as usize].iter().filter_map(|k| k.checked_sub(1)) {
            let s = &shards[k as usize];
            if !v.contains(s) {
                v.push(s.clone());
            }
        }
    }
    v
}

// -------------------------------------------------------------- estimator --
/// One PC as the estimator sees it.
#[derive(Clone, Debug)]
pub struct Pc {
    pub speed: f64, // relative to an average PC (1.0)
    pub link: f64,  // Mbit/s from the head (probe), or the assumed default
    pub lab: String,
    pub cached: bool,
}

/// Seconds for the head to send `mb` megabits to each PC in `links` at once.
pub fn transfer_direct(megabits: f64, links: &[f64], uplink: f64) -> f64 {
    if links.is_empty() || megabits <= 0.0 {
        return 0.0;
    }
    let (sum, min) = (links.iter().sum::<f64>(), links.iter().cloned().fold(f64::MAX, f64::min));
    (megabits * links.len() as f64 / uplink.min(sum)).max(megabits / min)
}

/// Rounds a lab needs: `h` holders each serve `fanout` others per round until `n` PCs are served.
pub fn peer_rounds(n: usize, h: usize, fanout: usize) -> usize {
    let (mut left, mut h, mut r) = (n, h.max(1), 0);
    while left > 0 {
        let served = left.min(h * fanout);
        left -= served;
        h += served;
        r += 1;
    }
    r
}

/// (seconds, bytes the head sends) for peer distribution: per lab the head seeds `seeds` PCs (none if the lab already has a
/// holder), then every holder serves `fanout` peers per round.
pub fn transfer_peer(megabits: f64, pcs: &[Pc], uplink: f64, fanout: usize, seeds: usize) -> (f64, usize) {
    let mut labs: HashMap<&str, (Vec<f64>, usize)> = HashMap::new(); // lab -> (links of PCs that need it, holders)
    for p in pcs {
        let e = labs.entry(p.lab.as_str()).or_default();
        if p.cached {
            e.1 += 1;
        } else {
            e.0.push(p.link);
        }
    }
    let mut seed_links = vec![];
    let mut plan = vec![];
    for (links, holders) in labs.values().filter(|l| !l.0.is_empty()) {
        let s = if *holders > 0 { 0 } else { seeds.min(links.len()) };
        seed_links.extend(links.iter().take(s).cloned());
        let mut l = links.clone();
        l.sort_by(|a, b| a.total_cmp(b));
        plan.push((links.len() - s, holders + s, l[l.len() / 2]));
    }
    let t_seed = transfer_direct(megabits, &seed_links, uplink);
    let rounds = plan.iter().map(|(n, h, link)| peer_rounds(*n, *h, fanout) as f64 * megabits / link).fold(0.0, f64::max);
    let nrounds = plan.iter().map(|(n, h, _)| peer_rounds(*n, *h, fanout)).max().unwrap_or(0);
    (t_seed + rounds + TICK_S * (nrounds + 1) as f64, seed_links.len())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alt {
    pub strategy: &'static str,
    pub pcs: usize,
    pub transfer_s: f64,
    pub finish_s: f64,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub best: Alt,
    pub alternatives: Vec<Alt>,
    pub compute_s: f64,
    pub overhead_s: f64,
    pub transfer_direct_s: f64,
    pub transfer_peer_s: f64,
    pub head_bytes_direct: f64,
    pub head_bytes_peer: f64,
}

/// The one estimate model (docs/BULK.md section 4): finish = transfer + compute + overhead, over strategies and PC counts.
/// PCs are taken cached first, then fastest first. `max_pcs` 0 = no limit.
pub fn plan(pcs: &[Pc], bytes: f64, work_s: f64, items: u64, uplink: f64, fanout: usize, seeds: usize, max_pcs: usize) -> Option<Plan> {
    if pcs.is_empty() {
        return None;
    }
    let mut v = pcs.to_vec();
    v.sort_by(|a, b| b.cached.cmp(&a.cached).then(b.speed.total_cmp(&a.speed)));
    let top = if max_pcs > 0 { max_pcs.min(v.len()) } else { v.len() };
    let mut ks: Vec<usize> = std::iter::successors(Some(1usize), |k| Some(k * 2)).take_while(|k| *k < top).collect();
    ks.push(top);
    let megabits = bytes * 8e-6;
    let overhead = 0.2 + 0.0005 * items as f64;
    let eval = |k: usize| {
        let c = &v[..k];
        let compute = work_s / c.iter().map(|p| p.speed.max(0.01)).sum::<f64>();
        let need: Vec<&Pc> = c.iter().filter(|p| !p.cached).collect();
        let td = transfer_direct(megabits, &need.iter().map(|p| p.link).collect::<Vec<_>>(), uplink);
        let (tp, hp) = transfer_peer(megabits, c, uplink, fanout, seeds);
        let tp = if need.is_empty() { 0.0 } else { tp };
        (compute, td, tp, hp, need.len())
    };
    let mut alts = vec![];
    for &k in &ks {
        let (compute, td, tp, _, _) = eval(k);
        alts.push(Alt { strategy: "direct", pcs: k, transfer_s: td, finish_s: td + compute + overhead });
        alts.push(Alt { strategy: "peer", pcs: k, transfer_s: tp, finish_s: tp + compute + overhead });
    }
    // ties go to direct: fewer moving parts
    let best = alts.iter().min_by(|a, b| a.finish_s.total_cmp(&b.finish_s)).cloned()?;
    let (compute, td, tp, hp, nneed) = eval(best.pcs);
    let mut alternatives: Vec<Alt> = ["direct", "peer"].iter().filter_map(|s| alts.iter().filter(|a| a.strategy == *s).min_by(|a, b| a.finish_s.total_cmp(&b.finish_s)).cloned()).collect();
    alternatives.extend(alts.iter().filter(|a| a.pcs == top).cloned());
    alternatives.dedup();
    Some(Plan {
        best, alternatives, compute_s: compute, overhead_s: overhead, transfer_direct_s: td, transfer_peer_s: tp,
        head_bytes_direct: bytes * nneed as f64,
        head_bytes_peer: bytes * hp as f64,
    })
}

pub fn human_secs(s: f64) -> String {
    if s < 90.0 { format!("{s:.0} s") } else if s < 5400.0 { format!("{:.1} min", s / 60.0) } else { format!("{:.1} h", s / 3600.0) }
}
pub fn human_bytes(b: f64) -> String {
    if b >= 1e9 { format!("{:.1} GB", b / 1e9) } else if b >= 1e6 { format!("{:.1} MB", b / 1e6) } else { format!("{:.0} KB", b / 1e3) }
}

// -------------------------------------------------------- strategy planner --
#[derive(Clone, Debug)]
pub struct Tgt {
    pub id: u64,
    pub lab: String,
    pub force_head: bool, // a peer fetch failed before: go to the head
}
#[derive(Clone, Debug)]
pub struct Hold {
    pub id: u64,
    pub lab: String,
    pub active: usize, // uploads running now
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum From {
    Head,
    Peer(u64),
}

/// Who each waiting PC fetches from. direct: the head. peer: a holder in the same lab with a free slot, else the head if the
/// lab has fewer than `seeds` holders-or-seeds, else wait. `seeding` = head fetches of this blob running now, per lab.
pub fn assign(peer: bool, targets: &[Tgt], holders: &[Hold], seeding: &HashMap<String, usize>, fanout: usize, seeds: usize) -> Vec<(u64, From)> {
    let mut hold: Vec<Hold> = holders.to_vec();
    let mut seeding = seeding.clone();
    let mut out = vec![];
    for t in targets {
        if !peer || t.force_head {
            out.push((t.id, From::Head));
            continue;
        }
        if let Some(h) = hold.iter_mut().filter(|h| h.lab == t.lab && h.active < fanout).min_by_key(|h| h.active) {
            h.active += 1;
            out.push((t.id, From::Peer(h.id)));
            continue;
        }
        let n = hold.iter().filter(|h| h.lab == t.lab).count() + seeding.get(&t.lab).copied().unwrap_or(0);
        if n < seeds {
            *seeding.entry(t.lab.clone()).or_default() += 1;
            out.push((t.id, From::Head));
        }
    }
    out
}

/// "10.0.3" for 10.0.3.7 (the /24 a PC is in).
pub fn lab_of(addr: &str) -> String {
    match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            let o = ip.octets();
            format!("{}.{}.{}", o[0], o[1], o[2])
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.segments()[..4].iter().map(|s| format!("{s:x}")).collect::<Vec<_>>().join(":"),
        Err(_) => addr.to_string(),
    }
}

// ------------------------------------------------------------------- head --
/// Link speed assumed for a PC nobody has probed (Mbit/s).
pub const ASSUMED_LINK: f64 = 1000.0;

#[derive(Default)]
pub struct Fetch {
    pub peer: Option<u64>, // wid of the serving PC, None = the head
    pub since: Option<Instant>,
}

static METAS: LazyLock<Mutex<HashMap<String, Arc<Meta>>>> = LazyLock::new(Default::default);
static UPLINK: Mutex<f64> = Mutex::new(0.0);
static WINDOW: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

#[cfg(test)]
pub fn test_meta(sha: &str) {
    METAS.lock().unwrap().insert(sha.into(), Arc::new(Meta { sha: sha.into(), size: 10, piece: 10, hashes: vec![[0; 32]] }));
}

pub fn meta_of(ctx: &Ctx, sha: &str) -> Option<Arc<Meta>> {
    if let Some(m) = METAS.lock().unwrap().get(sha) {
        return Some(m.clone());
    }
    let size = ctx.store.blob_size(sha)?;
    let (piece, hashes) = read_idx(&store_dir().join(format!("{sha}.idx")))?;
    let m = Arc::new(Meta { sha: sha.into(), size, piece, hashes });
    METAS.lock().unwrap().insert(sha.into(), m.clone());
    Some(m)
}

/// (Mbit/s the head can send in total, whether that is a guess).
pub fn head_uplink() -> (f64, bool) {
    let cap = crate::tune::get("head_uplink_mbps_cap");
    let l = *UPLINK.lock().unwrap();
    if cap > 0.0 { (cap, false) } else if l > 0.0 { (l, false) } else { (ASSUMED_LINK, true) }
}

pub fn load_uplink(ctx: &Ctx) {
    if let Some(v) = ctx.store.meta("head_uplink_mbps").and_then(|v| v.parse().ok()) {
        *UPLINK.lock().unwrap() = v;
    }
}

/// Learn the head uplink: the best aggregate rate seen in a window of at least a second with two or more transfers running.
fn observe_uplink(ctx: &Ctx) {
    let now = Instant::now();
    let total = SERVED.load(Relaxed);
    let mut w = WINDOW.lock().unwrap();
    let Some((t0, b0)) = *w else {
        *w = Some((now, total));
        return;
    };
    let dt = now.duration_since(t0).as_secs_f64();
    if dt < 1.0 {
        return;
    }
    let rate = (total - b0) as f64 * 8e-6 / dt;
    if MAX_ACTIVE.swap(0, Relaxed) >= 2 && rate > *UPLINK.lock().unwrap() {
        *UPLINK.lock().unwrap() = rate;
        ctx.store.set_meta("head_uplink_mbps", &format!("{rate:.1}")).ok();
    }
    *w = Some((now, total));
}

/// A PC that has been downloading `sha` for a moment serves the pieces it has (see Part). Only downloads that started at least
/// half a second ago count, and a fetcher picks holders already there: a chain, not a loop (a rare loop times out and goes to the head).
fn partial_holder(w: &crate::head::Worker, sha: &str) -> bool {
    w.fetching.get(sha).and_then(|f| f.since).is_some_and(|t| t.elapsed() >= Duration::from_millis(crate::tune::get("blob_partial_ms") as u64))
}

/// Is this connected PC one the blob logic applies to (a real agent that speaks blobs)?
fn real(w: &crate::head::Worker) -> bool {
    w.hello.is_some() && w.blob_rep // virtual agents too: they share their process's cache and blob port
}

/// PCs for the estimator: matching, able to work, real or virtual. Also the names of inputs that were guesses.
pub fn est_pcs(ctx: &Ctx, sel: &crate::select::Selector, sha: Option<&str>) -> (Vec<Pc>, Vec<&'static str>) {
    let st = ctx.sh.0.lock().unwrap();
    let ws: Vec<_> = st.workers.values().filter(|w| crate::head::can_work(w) && sel.matches(&w.attrs)).collect();
    let mut cal: Vec<f64> = ws.iter().map(|w| w.calib).filter(|c| *c > 0.0).collect();
    cal.sort_by(|a, b| a.total_cmp(b));
    let med = cal.get(cal.len() / 2).copied();
    let (mut no_link, mut no_speed) = (false, false);
    let pcs = ws
        .iter()
        .map(|w| {
            no_link |= w.net_mbps.is_none();
            let speed = match (med, w.calib > 0.0) {
                (Some(m), true) => w.calib / m,
                _ => {
                    no_speed = true;
                    1.0
                }
            };
            Pc { speed, link: w.net_mbps.unwrap_or(ASSUMED_LINK), lab: lab_of(&w.addr), cached: sha.is_some_and(|h| w.have.contains(h)) }
        })
        .collect();
    let mut assumed = vec![];
    if no_link {
        assumed.push("link_speed");
    }
    if no_speed {
        assumed.push("pc_speed");
    }
    (pcs, assumed)
}

/// CTRL from an agent about blobs. Called by head::ctrl_in with the state lock held.
pub fn on_ctrl(ctx: &Ctx, st: &mut State, wid: u64, v: &Value) {
    let Some(w) = st.workers.get_mut(&wid) else { return };
    if let Some(l) = v["have"].as_array() {
        w.have = l.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        let have = w.have.clone();
        w.fetching.retain(|s, _| !have.contains(s));
        w.blob_rep = true;
        if let Some(p) = v["blob_port"].as_u64() {
            w.blob_port = p as u16;
        }
    }
    if let Some(f) = v.get("fetched") {
        info_log("blob.fetch", json!({"node": w.hello.as_ref().map(|h| h.node_id.clone()), "sha": f["sha"], "bytes": f["bytes"], "source": f["from"], "peer_bytes": f["peer_bytes"], "ms": f["ms"]}));
    }
    if let Some(f) = v.get("fetch_failed") {
        if let Some(sha) = f["sha"].as_str() {
            if let Some(Fetch { peer: Some(p), .. }) = w.fetching.remove(sha) {
                w.avoid.insert(p);
            }
        }
    }
    if let Some(n) = v.get("net") {
        w.probing = None;
        if std::mem::take(&mut w.probe_burst) {
            return;
        }
        if let Some(mbps) = n["mbps"].as_f64() {
            let rtt = n["rtt_ms"].as_f64().unwrap_or(0.0);
            (w.net_mbps, w.rtt_ms, w.probed) = (Some(mbps), Some(rtt), Some(Instant::now()));
            if let Some(h) = &w.hello {
                ctx.store.set_node_net(&h.node_id, mbps, rtt, now_ms());
                info_log("net.probe", json!({"node": h.node_id, "down_mbps": mbps, "up_mbps": n["up_mbps"], "rtt_ms": rtt}));
            }
        } else {
            w.probed = Some(Instant::now());
        }
    }
}

pub fn is_ctrl(v: &Value) -> bool {
    ["have", "fetched", "fetch_failed", "net"].iter().any(|k| v.get(*k).is_some())
}

/// The jobs' blob needs, then who fetches what from whom; probes. Sends happen after the lock is released.
pub fn stage_tick(ctx: &Ctx) {
    observe_uplink(ctx);
    let mut sends: Vec<(Arc<Mutex<Out>>, Value)> = vec![];
    let mut pending_fail: Vec<(u64, String)> = vec![];
    {
        let mut st = ctx.sh.0.lock().unwrap();
        let st = &mut *st;
        // A reserved chunk whose blobs do not land (a peer died mid-transfer, the fetch answer was lost, the blob is gone from the head):
        // first re-stage it from the head alone, then give the chunk to another PC and keep this one off the job for a while.
        let limit = crate::tune::secs("stage_timeout_s");
        let mut hopeless: Vec<(u64, String)> = vec![];
        let mut back: Vec<(u64, u64, u64)> = vec![];
        for (wid, w) in st.workers.iter_mut() {
            let (Some((jid, a, b)), Some(t)) = (w.staged, w.staged_at) else { continue };
            let Some(j) = st.jobs.get(&jid) else { continue };
            let missing: Vec<String> = chunk_blobs(&j.shared, &j.shards, &j.item_sha, j.item_base, a, b).into_iter().filter(|s| !w.have.contains(s)).collect();
            if missing.is_empty() {
                continue;
            }
            let bytes: u64 = missing.iter().filter_map(|s| ctx.store.blob_size(s)).sum();
            if t.elapsed() < limit + Duration::from_secs_f64(bytes as f64 / 2e6) {
                continue;
            }
            let node = w.hello.as_ref().map_or(w.addr.clone(), |h| h.node_id.clone());
            let gone = missing.iter().find(|s| meta_of(ctx, s).is_none());
            if w.staged_strikes == 0 && gone.is_none() {
                w.fetching.retain(|s, _| !missing.contains(s));
                w.avoid.insert(u64::MAX); // the next fetch comes from the head, not from a peer
                (w.staged_strikes, w.staged_at) = (1, Some(Instant::now()));
                crate::head::event(ctx, "blob.restage", "head", &format!("job {jid} chunk {a}..{b} on {node}: {} blob(s) did not arrive in {:.0} s, fetching again from the head", missing.len(), t.elapsed().as_secs_f64()));
                continue;
            }
            if let Some(g) = gone {
                hopeless.push((jid, format!("blob {} is not on the head any more (it never reached {node}); upload it again and resubmit", &g[..12.min(g.len())])));
            }
            w.staged = None;
            w.inflight.retain(|x| *x != (jid, a, b));
            w.shunned.insert(jid, Instant::now());
            back.push((jid, a, b));
            crate::head::event(ctx, "blob.stage_timeout", "head", &format!("job {jid} chunk {a}..{b} given back: node {node} still lacked {} blob(s) after {:.0} s", missing.len(), t.elapsed().as_secs_f64()));
            let _ = wid;
        }
        for (jid, a, b) in back {
            if let Some(j) = st.jobs.get_mut(&jid) {
                j.requeue.push((a, b));
                j.requeue.sort_by(|x, y| y.cmp(x)); // lowest last
            }
            ctx.sh.1.notify_all();
        }
        for (jid, msg) in hopeless {
            pending_fail.push((jid, msg));
        }
        // sha -> (mode, wids that need it): blobs of the chunk each PC has reserved (head::take stages it)
        let mut need: HashMap<String, (String, Vec<u64>)> = HashMap::new();
        for (wid, w) in st.workers.iter() {
            let Some((jid, a, b)) = w.staged else { continue };
            let Some(j) = st.jobs.get(&jid) else { continue };
            if !real(w) {
                continue;
            }
            for sha in chunk_blobs(&j.shared, &j.shards, &j.item_sha, j.item_base, a, b).into_iter().filter(|s| !w.have.contains(s) && !w.fetching.contains_key(s)) {
                let e = need.entry(sha).or_insert_with(|| (j.blob_mode.clone(), vec![]));
                if !e.1.contains(wid) {
                    e.1.push(*wid);
                }
            }
        }
        for w in st.workers.values_mut() {
            // a fetch that never answered (agent hung) is tried again
            w.fetching.retain(|_, f| f.since.is_none_or(|t| t.elapsed() < Duration::from_secs(600)));
        }
        let (fanout, seeds) = (crate::tune::n("blob_peer_fanout") as usize, crate::tune::n("blob_seeds") as usize);
        let head_port = BOUND.load(Relaxed) as u64;
        for (sha, (mode, mut wids)) in need {
            let Some(m) = meta_of(ctx, &sha) else { continue };
            wids.sort();
            let lab = |w: &crate::head::Worker| lab_of(&w.addr);
            let tgts: Vec<Tgt> = wids.iter().map(|id| Tgt { id: *id, lab: lab(&st.workers[id]), force_head: !st.workers[id].avoid.is_empty() }).collect();
            let holders: Vec<Hold> = st
                .workers
                .iter()
                .filter(|(_, w)| real(w) && w.blob_port > 0 && (w.have.contains(&sha) || partial_holder(w, &sha)))
                .map(|(id, w)| Hold { id: *id, lab: lab(w), active: st.workers.values().filter(|o| o.fetching.values().any(|f| f.peer == Some(*id))).count() })
                .collect();
            let mut seeding: HashMap<String, usize> = HashMap::new();
            // head seeds that already serve their pieces are holders now: not counted twice
            for w in st.workers.values().filter(|w| w.fetching.get(&sha).is_some_and(|f| f.peer.is_none()) && !partial_holder(w, &sha)) {
                *seeding.entry(lab(w)).or_default() += 1;
            }
            let peer = match mode.as_str() {
                "direct" => false,
                "peer" => true,
                _ => {
                    // auto: the cheaper of the two by the estimator's own formulas
                    let pcs: Vec<Pc> = tgts.iter().map(|t| Pc { speed: 1.0, link: st.workers[&t.id].net_mbps.unwrap_or(ASSUMED_LINK), lab: t.lab.clone(), cached: false })
                        .chain(holders.iter().map(|h| Pc { speed: 1.0, link: ASSUMED_LINK, lab: h.lab.clone(), cached: true })).collect();
                    let mb = m.size as f64 * 8e-6;
                    let up = head_uplink().0;
                    let td = transfer_direct(mb, &pcs.iter().filter(|p| !p.cached).map(|p| p.link).collect::<Vec<_>>(), up);
                    m.size >= 1 << 20 && transfer_peer(mb, &pcs, up, fanout, seeds).0 < td
                }
            };
            let who = assign(peer, &tgts, &holders, &seeding, fanout, seeds);
            for (id, from) in who {
                let (tid, tkey) = new_ticket();
                let src = match from {
                    From::Head => {
                        allow(&tid, &tkey, &sha, true);
                        json!({"head": head_port, "id": tid, "key": hex(&tkey)})
                    }
                    From::Peer(h) => {
                        let hw = &st.workers[&h];
                        if let Some(o) = hw.out.clone() {
                            sends.push((o, json!({"allow": [{"id": tid, "key": hex(&tkey), "sha": sha}]})));
                        }
                        json!({"addr": format!("{}:{}", hw.addr, hw.blob_port), "id": tid, "key": hex(&tkey)})
                    }
                };
                let w = st.workers.get_mut(&id).unwrap();
                w.fetching.insert(sha.clone(), Fetch { peer: if let From::Peer(h) = from { Some(h) } else { None }, since: Some(Instant::now()) });
                if let Some(o) = w.out.clone() {
                    let pieces: Vec<String> = m.hashes.iter().map(|h| hex(h)).collect();
                    sends.push((o, json!({"fetch": {"sha": sha, "size": m.size, "piece": m.piece, "pieces": pieces, "from": [src]}})));
                }
            }
        }
        // link probes: idle real PCs that were never measured or are due; two at a time unless an operator asked
        let every = Duration::from_secs_f64(crate::tune::get("net_probe_every_s"));
        let mut running = st.workers.values().filter(|w| w.probing.is_some()).count();
        let mb = crate::tune::get("net_probe_mb");
        for w in st.workers.values_mut() {
            let due = w.probed.is_none_or(|t| t.elapsed() > every);
            let idle = w.inflight.is_empty() && w.fetching.is_empty() && w.presence != "busy";
            let forced = std::mem::take(&mut w.probe_now);
            let virt = w.hello.as_ref().is_some_and(|h| h.virtual_node); // a link probe of a virtual agent measures nothing real
            if !real(w) || virt || w.probing.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) || !(forced || (due && idle && w.calib > 0.0 && running < 2)) {
                continue;
            }
            let (tid, tkey) = new_ticket();
            allow(&tid, &tkey, PROBE, true);
            w.probing = Some(Instant::now());
            running += 1;
            if let Some(o) = w.out.clone() {
                sends.push((o, json!({"probe": {"mb": mb, "from": [{"head": BOUND.load(Relaxed), "id": tid, "key": hex(&tkey)}]}})));
            }
        }
    }
    for (o, v) in sends {
        let _ = o.lock().unwrap().put(proto::CTRL, v.to_string().as_bytes());
    }
    let mut seen = HashSet::new();
    for (jid, msg) in pending_fail {
        if seen.insert(jid) {
            crate::head::fail_job(ctx, jid, &msg);
        }
    }
}

/// Head loop: stage blobs and probe links about three times a second; trim the store once a minute.
pub fn head_loop(ctx: Arc<Ctx>) {
    start_server();
    load_uplink(&ctx);
    let mut last_gc = Instant::now();
    loop {
        thread::sleep(Duration::from_secs_f64(TICK_S));
        stage_tick(&ctx);
        if last_gc.elapsed() > Duration::from_secs(60) {
            last_gc = Instant::now();
            gc_store(&ctx);
        }
    }
}

/// Above blob_store_gb delete the least recently used blobs that no unfinished job names.
pub fn gc_store(ctx: &Ctx) -> usize {
    sweep_stale(&store_dir(), Duration::from_secs(6 * 3600));
    let limit = (crate::tune::get("blob_store_gb") * 1073741824.0) as u64;
    let mut rows = ctx.store.blob_rows();
    let mut total: u64 = rows.iter().map(|r| r.1).sum();
    rows.sort_by_key(|r| r.4);
    let mut n = 0;
    for (sha, size, refs, _, _) in rows {
        if total <= limit {
            break;
        }
        if refs > 0 {
            continue;
        }
        let d = store_dir();
        let _ = fs::remove_file(d.join(&sha));
        let _ = fs::remove_file(d.join(format!("{sha}.idx")));
        ctx.store.delete_blob(&sha);
        METAS.lock().unwrap().remove(&sha);
        total -= size;
        n += 1;
    }
    n
}

// ------------------------------------------------------- standby replication --
/// Primary side: a REPL "blob" offer (size, piece hashes, a head ticket) for each blob in `rows` not in `offered` yet, at most
/// `max` per call. The standby pulls it over the blob port, verified piece by piece and then by sha256.
pub fn offers(ctx: &Ctx, offered: &mut HashSet<String>, max: usize) -> Vec<Value> {
    let port = BOUND.load(Relaxed);
    if port == 0 {
        return vec![];
    }
    let mut out = vec![];
    for (sha, ..) in ctx.store.blob_rows() {
        if out.len() >= max {
            break;
        }
        let Some(m) = (!offered.contains(&sha)).then(|| meta_of(ctx, &sha)).flatten() else { continue };
        let (id, key) = new_ticket();
        allow(&id, &key, &sha, true);
        offered.insert(sha.clone());
        out.push(json!({"sha": sha, "size": m.size, "piece": m.piece, "pieces": m.hashes.iter().map(|h| hex(h)).collect::<Vec<_>>(), "port": port, "id": id, "key": hex(&key)}));
    }
    out
}

static TAKING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Standby side: fetch an offered blob from the primary into this PC's blob store (CK_DATA/blobs), in the background.
/// The database row is added by `adopt` when this PC takes over (the files are what matters until then).
pub fn standby_take(o: &Value, primary_ip: &str) {
    let hashes: Option<Vec<[u8; 32]>> = o["pieces"].as_array().and_then(|a| a.iter().map(|h| unhex(h.as_str()?)?.try_into().ok()).collect());
    let (Some(sha), Some(hashes), Some(size), Some(piece), Some(port), Some(id), Some(key)) =
        (o["sha"].as_str().filter(|s| valid_sha(s)), hashes, o["size"].as_u64(), o["piece"].as_u64().filter(|p| *p > 0), o["port"].as_u64(), o["id"].as_str(), o["key"].as_str().and_then(unhex))
    else {
        return;
    };
    let dir = store_dir();
    if dir.join(sha).metadata().is_ok_and(|m| m.len() == size) || !TAKING.lock().unwrap().insert(sha.into()) {
        return;
    }
    let (sha, m, src) = (sha.to_string(), Meta { sha: sha.into(), size, piece, hashes }, Source { addr: format!("{primary_ip}:{port}"), id: id.into(), key, head: true });
    thread::spawn(move || {
        match fetch(&dir, &m, &[src]) {
            Ok(g) => info_log("blob.standby_copy", json!({"sha": sha, "bytes": g.bytes, "ms": g.ms})),
            Err(e) => crate::warnl!("blob.standby_copy_failed", "standby: blob {}: {e}", &sha[..12]),
        }
        TAKING.lock().unwrap().remove(&sha);
    });
}

/// At takeover: blobs the standby copied get their database rows (they were never in the change log); logs the blobs the
/// database names that never arrived. Returns (rows added, blobs missing).
pub fn adopt(st: &crate::store::Store) -> (usize, usize) {
    let d = store_dir();
    let mut added = 0;
    for (sha, size, _) in scan(&d) {
        if st.blob_size(&sha).is_none() && d.join(format!("{sha}.idx")).is_file() {
            st.put_blob(&sha, size, now_ms());
            added += 1;
        }
    }
    let missing = st.blob_rows().iter().filter(|r| !d.join(&r.0).is_file()).count();
    if missing > 0 {
        crate::warnl!("blob.standby_missing", "takeover: {missing} blob(s) the database lists were not copied to this PC yet; jobs that need them wait for a re-upload");
    }
    (added, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-blobs-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }
    fn pc(lab: &str, link: f64, cached: bool) -> Pc {
        Pc { speed: 1.0, link, lab: lab.into(), cached }
    }

    #[test]
    fn rounds_and_formulas() {
        assert_eq!(peer_rounds(18, 2, 4), 2); // 2 holders serve 8, 10 holders serve the other 10
        assert_eq!(peer_rounds(0, 2, 4), 0);
        assert_eq!(peer_rounds(1, 1, 4), 1);
        // 200 MB, 20 PCs, 100 Mbit/s links, uplink unlimited: direct = one blob time, peer = seeds + 2 rounds
        let mb = 200e6 * 8e-6;
        let td = transfer_direct(mb, &[100.0; 20], 1e9);
        assert!((td - 16.0).abs() < 1e-9, "{td}");
        let pcs: Vec<Pc> = (0..20).map(|_| pc("a", 100.0, false)).collect();
        let (tp, head_copies) = transfer_peer(mb, &pcs, 1e9, 4, 2);
        assert_eq!(head_copies, 2);
        assert!((tp - (16.0 * 3.0 + 3.0 * TICK_S)).abs() < 1e-9, "{tp}");
        // the head uplink binds direct: 20 x 1600 Mbit over 1000 Mbit/s
        assert!((transfer_direct(mb, &[1000.0; 20], 1000.0) - 32.0).abs() < 1e-9);
        // a slow PC alone bounds it
        assert!((transfer_direct(mb, &[10.0, 1000.0], 1e9) - 160.0).abs() < 1e-9);
        // nothing to send
        assert_eq!(transfer_direct(mb, &[], 1000.0), 0.0);
    }

    #[test]
    fn plan_picks_strategy_and_counts_cached() {
        let pcs: Vec<Pc> = (0..20).map(|_| pc("a", 1000.0, false)).collect();
        let p = plan(&pcs, 2e9, 100.0, 0, 1000.0, 4, 2, 0).unwrap();
        assert_eq!(p.best.strategy, "peer", "2 GB to 20 PCs on 1 Gbit uplink: peer wins");
        assert!(p.transfer_peer_s < p.transfer_direct_s);
        assert!(p.head_bytes_peer * 4.0 <= p.head_bytes_direct, "peer cuts head bytes at least 4x");
        // a tiny blob: direct (peer pays rounds of latency)
        let p = plan(&pcs, 1e3, 100.0, 0, 1000.0, 4, 2, 0).unwrap();
        assert_eq!(p.best.strategy, "direct");
        // everything cached: no transfer at all
        let c: Vec<Pc> = (0..5).map(|_| pc("a", 100.0, true)).collect();
        let p = plan(&c, 2e9, 10.0, 0, 1000.0, 4, 2, 0).unwrap();
        assert_eq!((p.transfer_direct_s, p.transfer_peer_s), (0.0, 0.0));
        // more PCs help compute until transfer outweighs: big data, little work -> few PCs
        let p = plan(&pcs, 5e9, 5.0, 0, 100.0, 4, 2, 0).unwrap();
        assert!(p.best.pcs < 20, "{:?}", p.best);
        assert!(plan(&[], 1.0, 1.0, 0, 1.0, 4, 2, 0).is_none());
        let p = plan(&pcs, 1e3, 100.0, 0, 1000.0, 4, 2, 3).unwrap();
        assert!(p.best.pcs <= 3, "max_pcs respected");
    }

    #[test]
    fn assignment_tree() {
        let t = |id, lab: &str| Tgt { id, lab: lab.into(), force_head: false };
        let none = HashMap::new();
        // direct: everyone from the head
        assert!(assign(false, &[t(1, "a"), t(2, "a")], &[], &none, 4, 2).iter().all(|x| x.1 == From::Head));
        // peer, nobody holds it: the head seeds `seeds` PCs per lab, the rest wait
        let w = assign(true, &[t(1, "a"), t(2, "a"), t(3, "a"), t(4, "b")], &[], &none, 4, 2);
        assert_eq!(w, vec![(1, From::Head), (2, From::Head), (4, From::Head)]);
        // holders serve up to fanout each, same lab only
        let h = |id, lab: &str, active| Hold { id, lab: lab.into(), active };
        let tg: Vec<Tgt> = (10..20).map(|i| t(i, "a")).collect();
        let w = assign(true, &tg, &[h(1, "a", 0), h(2, "a", 1), h(3, "b", 0)], &none, 2, 1);
        assert_eq!(w.len(), 3, "holder 1 takes 2, holder 2 takes 1 more, lab b holder is not used: {w:?}");
        assert!(w.iter().all(|x| matches!(x.1, From::Peer(1) | From::Peer(2))));
        // a PC whose peer fetch failed goes to the head
        let w = assign(true, &[Tgt { force_head: true, ..t(5, "a") }], &[h(1, "a", 0)], &none, 4, 2);
        assert_eq!(w, vec![(5, From::Head)]);
    }

    #[test]
    fn ingest_dedup_limit_and_pieces() {
        let d = tmp("ingest");
        let b = data(3_000_000);
        let (sha, size, existed) = ingest(&mut &b[..], &d, 1 << 30).unwrap();
        assert_eq!((size, existed, sha.len()), (3_000_000, false, 64));
        assert_eq!(sha, hex(&Sha256::digest(&b)));
        let (sha2, _, existed) = ingest(&mut &b[..], &d, 1 << 30).unwrap();
        assert!(existed && sha2 == sha, "stored once");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "blob + idx, no temp file left");
        let (piece, hashes) = read_idx(&d.join(format!("{sha}.idx"))).unwrap();
        assert_eq!(hashes.len() as u64, 3_000_000u64.div_ceil(piece));
        assert!(ingest(&mut &b[..], &tmp("ingest2"), 1000).is_err(), "over the limit");
    }

    fn serve_dir(d: &Path, port: u16) {
        std::env::set_var("CK_DATA", d);
        std::env::set_var("CK_BLOB_PORT", port.to_string());
        let _ = d; // the server finds blobs in CK_DATA/blobs
    }

    #[test]
    fn transfer_resume_verify_and_admission() {
        // one process-wide server for this test binary: CK_DATA/blobs of this test
        let root = tmp("xfer");
        let store = root.join("blobs");
        let b = data(2_500_000);
        std::env::set_var("CK_TUNE_BLOB_PIECE_MB", "0.5");
        let (sha, size, _) = ingest(&mut &b[..], &store, 1 << 30).unwrap();
        let (piece, hashes) = read_idx(&store.join(format!("{sha}.idx"))).unwrap();
        assert_eq!(piece, 524288);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        serve_dir(&root, port);
        assert_eq!(start_server(), port);
        let m = Meta { sha: sha.clone(), size, piece, hashes: hashes.clone() };
        let (id, key) = new_ticket();
        let src = Source { addr: format!("127.0.0.1:{port}"), id: id.clone(), key: key.clone(), head: true };
        // no ticket: refused
        let e = fetch(&root.join("c0"), &m, &[src.clone()]).unwrap_err().to_string();
        assert!(e.contains("refused"), "{e}");
        allow(&id, &key, &sha, true);
        // wrong key: the sealed reply does not open
        let bad = Source { key: vec![9; 32], ..src.clone() };
        assert!(fetch(&root.join("c0"), &m, &[bad]).is_err());
        // a ticket names one sha: a request for another blob is refused
        let other_sha = "0".repeat(64);
        let (id2, key2) = new_ticket();
        allow(&id2, &key2, &other_sha, true);
        assert!(fetch(&root.join("c0"), &m, &[Source { id: id2, key: key2, ..src.clone() }]).is_err(), "ticket for another sha");
        let (id, key) = new_ticket();
        allow(&id, &key, &sha, true);
        let src = Source { id, key, ..src };
        // resume: pre-seed the .part with the first two good pieces and one corrupt one
        let c1 = root.join("c1");
        fs::create_dir_all(&c1).unwrap();
        let mut part = b.clone();
        part[piece as usize * 2 + 5] ^= 0xff; // piece 2 corrupt
        part.truncate(size as usize);
        part[piece as usize * 3..].fill(0); // pieces 3.. missing
        fs::write(c1.join(format!("{sha}.part")), &part).unwrap();
        let before = SERVED.load(Relaxed);
        let g = fetch(&c1, &m, &[src.clone()]).unwrap();
        assert_eq!(g.from, "head");
        assert_eq!(g.bytes, size - piece * 2, "pieces 0 and 1 were kept, the rest fetched");
        assert!(SERVED.load(Relaxed) - before >= g.bytes);
        assert_eq!(fs::read(c1.join(&sha)).unwrap(), b);
        assert!(!c1.join(format!("{sha}.part")).exists());
        // cached: nothing moves
        assert_eq!(fetch(&c1, &m, &[src.clone()]).unwrap().bytes, 0);
        // a source that sends wrong data is detected: tamper one piece hash in the expected list
        let mut m2 = m.clone();
        m2.hashes[1][0] ^= 1;
        let (id, key) = new_ticket();
        allow(&id, &key, &sha, true);
        assert!(fetch(&root.join("c2"), &m2, &[Source { id, key, ..src.clone() }]).is_err(), "bad piece refused");
        // fan-out cap: peers (non-head tickets) beyond blob_peer_fanout get BUSY
        std::env::set_var("CK_TUNE_BLOB_PEER_FANOUT", "1");
        ACTIVE_PEER.store(1, Relaxed);
        let (id, key) = new_ticket();
        allow(&id, &key, &sha, false);
        let e = fetch(&root.join("c3"), &m, &[Source { id, key, ..src.clone() }]).unwrap_err().to_string();
        assert!(e.contains("busy"), "{e}");
        ACTIVE_PEER.store(0, Relaxed);
        // a peer that is still downloading serves the pieces it has (K_NOPE for the rest); the fetcher asks again until whole
        let b2: Vec<u8> = data(2_000_000).iter().map(|x| x ^ 0x5a).collect();
        let (sha2, size2, _) = ingest(&mut &b2[..], &root.join("scratch"), 1 << 30).unwrap();
        let (piece2, hashes2) = read_idx(&root.join("scratch").join(format!("{sha2}.idx"))).unwrap();
        let m3 = Meta { sha: sha2.clone(), size: size2, piece: piece2, hashes: hashes2 };
        let pdir = root.join("peer");
        fs::create_dir_all(&pdir).unwrap();
        fs::write(pdir.join(format!("{sha2}.part")), &b2).unwrap();
        let n3 = size2.div_ceil(piece2) as usize;
        let shared = Arc::new(Mutex::new((0..n3).map(|i| i % 2 == 0).collect::<Vec<_>>()));
        PARTS.lock().unwrap().insert(sha2.clone(), Arc::new(Part { path: pdir.join(format!("{sha2}.part")), size: size2, piece: piece2, have: shared.clone() }));
        let (id, key) = new_ticket();
        allow(&id, &key, &sha2, false);
        let src3 = Source { addr: format!("127.0.0.1:{port}"), id, key, head: false };
        let sh = shared.clone();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(400));
            sh.lock().unwrap().fill(true); // the peer's own download moved on
        });
        let g = fetch(&root.join("c5"), &m3, &[src3]).unwrap();
        t.join().unwrap();
        assert_eq!((g.bytes, g.peer_bytes, g.from.as_str()), (size2, size2, "peer"));
        assert!(g.ms >= 150.0, "had to wait for the missing pieces: {}", g.ms);
        assert_eq!(fs::read(root.join("c5").join(&sha2)).unwrap(), b2);
        PARTS.lock().unwrap().remove(&sha2);
        // probe
        let (id, key) = new_ticket();
        allow(&id, &key, PROBE, true);
        let (down, up, rtt) = probe(&Source { id, key, ..src }, 2.0).unwrap();
        assert!(down > 0.0 && up > 0.0 && rtt >= 0.0);
        std::env::remove_var("CK_TUNE_BLOB_PEER_FANOUT");
    }

    #[test]
    fn stale_part_and_tmp_files_are_swept() {
        let d = tmp("sweep");
        let old = |n: &str| {
            fs::write(d.join(n), b"x").unwrap();
            OpenOptions::new().append(true).open(d.join(n)).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(7 * 3600)).unwrap();
        };
        old(".tmp-abc");
        old("deadbeef.part");
        fs::write(d.join("fresh.part"), b"x").unwrap();
        assert_eq!(sweep_stale(&d, Duration::from_secs(6 * 3600)), 2);
        assert!(d.join("fresh.part").exists() && !d.join(".tmp-abc").exists());
    }

    #[test]
    fn eviction_is_lru_and_keeps_the_new_one() {
        let d = tmp("evict");
        let mk = |n: u8, age: u64| {
            let sha = hex(&[n; 32]);
            let p = d.join(&sha);
            fs::write(&p, vec![n; 1000]).unwrap();
            fs::write(d.join(format!("{sha}.idx")), b"x").unwrap();
            let f = OpenOptions::new().append(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() - Duration::from_secs(age)).unwrap();
            sha
        };
        let (a, b, c) = (mk(1, 300), mk(2, 200), mk(3, 100));
        let gone = evict(&d, 2000, &[]);
        assert_eq!(gone, vec![a.clone()], "oldest goes");
        assert!(!d.join(format!("{a}.idx")).exists() && d.join(&b).exists());
        let gone = evict(&d, 500, &[c.clone()]);
        assert_eq!(gone, vec![b], "the kept blob survives even over the limit");
        assert!(d.join(&c).exists());
        // touch moves a blob to the back of the line
        let (x, y) = (mk(4, 300), mk(5, 200));
        touch(&d.join(&x));
        assert_eq!(evict(&d, 2000, &[c]), vec![y]);
        assert!(d.join(&x).exists());
    }

    #[test]
    fn upload_token_is_job_scoped_and_chunk_blobs_distinct() {
        let t = put_token(7);
        assert_eq!(check_put_token(&t), Some(7));
        assert_eq!(check_put_token(&t.replace(".7.", ".8.")), None, "forged for another job");
        assert_eq!(check_put_token("cbp.7.00"), None);
        assert_eq!(check_put_token("sometoken"), None);
        let (sh, shards) = (vec!["code".to_string()], vec!["s1".to_string(), "s2".to_string()]);
        assert_eq!(chunk_blobs(&sh, &shards, &[vec![1], vec![2], vec![], vec![1]], 0, 0, 4), vec!["code", "s1", "s2"]);
        assert_eq!(chunk_blobs(&sh, &shards, &[vec![1], vec![2], vec![], vec![1]], 0, 2, 3), vec!["code"]);
        assert_eq!(chunk_blobs(&sh, &shards, &[vec![1], vec![2]], 0, 1, 99), vec!["code", "s2"], "range past the end is clipped");
        assert_eq!(chunk_blobs(&sh, &shards, &[vec![1, 2], vec![]], 100, 100, 101), vec!["code", "s1", "s2"], "an item may name several blobs; ranges count from the base");
    }

    #[test]
    fn items_name_their_blobs() {
        let (a, b) = ("ab".repeat(32), "cd".repeat(32));
        assert_eq!(item_blobs(&format!("@blob:{a}:0:5")), vec![a.as_str()]);
        assert_eq!(item_blobs(&format!("@refs:{a},{b}|zAAAA")), vec![a.as_str(), b.as_str()]);
        assert_eq!(item_blobs(&format!("@refs:{a},nothex|zAAAA")), vec![a.as_str()]);
        assert!(item_blobs("zAAAA").is_empty() && item_blobs("@refs:|p").is_empty());
    }

    #[test]
    fn items_and_env() {
        let sha = "ab".repeat(32);
        assert_eq!(item_blob(&format!("@blob:{sha}:0:10")), Some(sha.as_str()));
        assert_eq!(item_blob("@blob:short"), None);
        assert_eq!(item_blob("plain"), None);
        assert_eq!(resolve_item(b"plain"), b"plain");
        let r = String::from_utf8(resolve_item(format!("@blob:{sha}:5:7").as_bytes())).unwrap();
        assert!(r.ends_with(&format!("{sha}\t5\t7")), "{r}");
        let r = String::from_utf8(resolve_item(format!("@blob:{sha}").as_bytes())).unwrap();
        assert!(r.ends_with(&sha) && !r.contains('\t'));
        assert_eq!(resolve_item(b"@blob:nothex"), b"@blob:nothex", "not a sha: left alone");
        assert!(env_for(987_654).is_empty());
    }

    #[test]
    fn rot_while_running_is_caught_on_use() {
        let d = std::env::temp_dir().join(format!("ck-verify-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let data = vec![7u8; 100_000];
        let sha = hex(&Sha256::digest(&data));
        let p = d.join(&sha);
        fs::write(&p, &data).unwrap();
        let stamp = |p: &Path| { let m = fs::metadata(p).unwrap(); (m.len(), m.modified().unwrap()) };
        let (l, t) = stamp(&p);
        assert!(verify(&d, &sha, l, t), "good file");
        // same length, bytes flipped, mtime moved on: caught
        let mut bad = data.clone();
        bad[500] ^= 1;
        fs::write(&p, &bad).unwrap();
        let f = OpenOptions::new().append(true).open(&p).unwrap();
        f.set_modified(t + Duration::from_secs(5)).unwrap();
        let (l2, t2) = stamp(&p);
        assert!(!verify(&d, &sha, l2, t2), "flipped byte");
        assert!(!p.exists(), "bad blob deleted");
        assert!(take_resync() && !take_resync());
        let _ = fs::remove_dir_all(&d);
    }
}
