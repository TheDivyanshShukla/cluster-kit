//! Wire protocol: ports, frame kinds, framing, the HELLO contract. Frozen contract: only the lead edits this file.
//!
//! Frame: [kind u8][len u32 LE][payload]. Numbers in payloads are u64 LE.
//! Protocol v2 (docs/CONSOLE_REQUIREMENTS.md, M0):
//! - HELLO_W payload = [threads u32][version u32][Hello as JSON]. A v1 head reads only `threads`.
//! - HEARTBEAT (empty) is sent by both sides every HEARTBEAT_EVERY. Each side sets a read timeout of LOST_AFTER:
//!   no frame of any kind for that long = the peer is lost (pulled cable, frozen process). Receivers ignore
//!   HEARTBEAT apart from resetting the timeout; it never counts as a chunk request.
//! - CTRL carries JSON control messages (reserved for M1).
//! - Ports: CK_PORT (default 7700) = TCP workers/clients, +1 = UDP discovery, +2 = HTTP dashboard.
//! - Beacon (UDP, every second): "ck/1 <cluster name> <head ip>:<tcp port>". Agents connect to the address in the text,
//!   not to the packet's sender, so a relay agent (`ck agent --relay`) can re-broadcast it on another subnet (FLT-05).
//!   The old form "ck/1 <name>" (no address) still means "the sender is the head".
//!   M4: the beacon ends with " e=<epoch>" (head term, repl.rs); agents follow the highest epoch they hear.

use serde::{Deserialize, Serialize};
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

pub const MAX_FRAME: usize = 1 << 28;
/// Chunks a worker holds: one computing, the rest queued (tunable "pipeline").
pub fn pipeline() -> usize {
    crate::tune::n("pipeline") as usize
}
pub const PROTO_VERSION: u32 = 3;
/// Tunable "heartbeat_s" (default 1 s).
pub fn heartbeat_every() -> Duration {
    crate::tune::secs("heartbeat_s")
}

/// Tunable "lost_after_s" (default 5 s).
pub fn lost_after() -> Duration {
    crate::tune::secs("lost_after_s")
}

pub const HELLO_W: u8 = 1; // worker -> head: threads u32, version u32, Hello JSON
pub const HELLO_C: u8 = 2; // client -> head
pub const REQ: u8 = 3; // worker -> head: give me a chunk
pub const TASK: u8 = 4; // head -> worker: job, start, end, kind u8, arg
pub const RESULT: u8 = 5; // worker -> head: job, start, end, cpu x10, mem used, mem total, compute us, body (also requests the next chunk)
pub const RESULT_HDR: usize = 56;
pub const SUBMIT: u8 = 6; // client -> head: start, end, chunk, kind u8, only len u8, only (worker IPs, comma-separated), arg
pub const PART: u8 = 7; // head -> client: start, end, body
pub const DONE: u8 = 8; // head -> client
pub const PING: u8 = 9;
pub const PONG: u8 = 10;
pub const ERR: u8 = 11; // head -> client: message
pub const CANCEL: u8 = 12; // head -> worker: job (stop its running chunk, skip its queued ones)
pub const HEARTBEAT: u8 = 13; // both ways, empty
pub const CTRL: u8 = 14; // JSON control message.
// head -> worker: {"cap_threads": N | null} (ORG-05: run chunks on at most N threads)
// worker -> head: {"presence": "busy" | "idle"} (SCH-10: someone is using this PC; the head sends it no new chunks while busy)
//                 {"calib_mhs": F} (FLT-06: SHA-256 MH/s from a 3 s benchmark at join)
pub const ACCEPTED: u8 = 15; // head -> client after SUBMIT: job id u64. Jobs now outlive the client (JOB-02).
pub const ATTACH: u8 = 16; // client -> head: job id u64. Head replies with every stored PART, then live PARTs, then DONE (or ERR).
// Fleet admission (FLT-04, SEC-02), protocol v3. Not used when the head runs with CK_FLEET=open.
//   new PC:   HELLO_W (hello.join_code = code) -> head checks the code -> KEY (32-byte node key, once; the agent saves it)
//   known PC: HELLO_W -> CHALLENGE (32-byte nonce) -> PROOF (auth::respond(key, nonce, node_id)) -> head verifies
//   refused:  DENIED (reason text), then the head closes. After KEY or a good PROOF every frame both ways is sealed.
pub const CHALLENGE: u8 = 17;
pub const PROOF: u8 = 18;
pub const KEY: u8 = 19;
pub const DENIED: u8 = 20;
/// worker -> head: job, start, end (u64 each), retry u8, message. The chunk failed (JOB-04/06); also requests the next chunk.
pub const FAILED: u8 = 21;
/// client -> head: a jobs::JobSpec as JSON (selector, priority, name...). Same replies as SUBMIT. `submitter` is set by the head.
pub const SUBMIT_JSON: u8 = 22;
// High availability (M4, repl.rs header has the flow).
pub const HELLO_S: u8 = 23; // standby -> primary: Hello JSON length u32 LE, Hello JSON, last applied seq u64
pub const SNAPSHOT: u8 = 24; // primary -> standby: epoch u64, seq u64, then the database file bytes
pub const REPL: u8 = 25; // primary -> standby: one repl::Entry as JSON
pub const REPL_ACK: u8 = 26; // standby -> primary: seq u64 applied
pub const ACK: u8 = 27; // head -> worker: job, start, end (u64 each): that result is durable, drop your copy
/// primary -> standby, for a database over SNAP_ONE bytes (one SNAPSHOT frame is capped by MAX_FRAME): SNAPSHOT_BEGIN
/// (epoch u64, seq u64, total length u64), then SNAPSHOT_MORE frames (raw file bytes, SNAP_PIECE at most) until `total` arrived.
pub const SNAPSHOT_BEGIN: u8 = 29;
pub const SNAPSHOT_MORE: u8 = 30;
pub const SNAP_ONE: u64 = 64 << 20;
pub const SNAP_PIECE: usize = 32 << 20;
pub const CANCEL_RANGE: u8 = 28; // head -> worker: job, start, end: stop or skip this one chunk (the other copy won)

pub const KIND_SHA: u8 = 0; // arg = prefix as nibbles; body = (n u64, sha256 [u8; 32])*
pub const KIND_CMD: u8 = 1; // arg = command line; body = its stdout
pub const KIND_SYNTH: u8 = 2; // arg = nanoseconds per number u64 LE; body = count of numbers u64 LE
pub const KIND_LIST: u8 = 3; // list job (JOB-01): arg = command line, 0x00, then the items of [a, b) joined by '\n'; items go to stdin

/// TCP port for workers and clients. UDP discovery is +1, the dashboard +2.
pub fn port() -> u16 {
    std::env::var("CK_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7700)
}
pub fn disc_port() -> u16 {
    port() + 1
}
pub fn dash_port() -> u16 {
    port() + 2
}

/// What a worker says about itself in HELLO_W (v2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    pub cpu_model: String,
    pub cores_physical: u32,
    pub cores_logical: u32,
    pub ram_bytes: u64,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub ck_version: String,
    /// MAC addresses of the wired/wireless NICs ("aa:bb:cc:dd:ee:ff"), for Wake-on-LAN (NET-06). Empty if unknown.
    #[serde(default)]
    pub macs: Vec<String>,
}

// CTRL JSON keys (wave 7, NET-06 / HA-10), head -> agent:
//   {"power": "sleep"}              suspend this PC now (CK_POWER_DRY=1: log only)
//   {"wol": ["aa:bb:..", ..]}       send Wake-on-LAN magic packets on this PC's subnet (a lab-local helper)
//   {"standby_of": "ip:port"}       start `ck head --standby` on this PC for that head (once; HA-10)
// agent -> head: {"standby_started": true} | {"standby_failed": "why"}
//   {"job": {"id": J, "mem_mb": M}}  wave 10b, head -> agent before the first TASK of job J on that agent: cap copies
//                                    of J's chunks at floor(free RAM x 0.8 / M) (M = 0: no cap)

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// Stable across IP changes and reconnects (identity::node_id). Virtual nodes append "/v<i>".
    pub node_id: String,
    pub host: String,
    #[serde(default)]
    pub virtual_node: bool,
    #[serde(default)]
    pub inv: Inventory,
    /// Only on a PC's first join (or after its key was lost): the code an operator created.
    #[serde(default)]
    pub join_code: Option<String>,
}

pub fn hello_payload(threads: u32, h: &Hello) -> Vec<u8> {
    let mut p = threads.to_le_bytes().to_vec();
    p.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    p.extend_from_slice(&serde_json::to_vec(h).unwrap_or_default());
    p
}

/// (threads, version, hello). A v1 worker sends only threads: version 1, no hello.
pub fn parse_hello(p: &[u8]) -> (u32, u32, Option<Hello>) {
    let u32_at = |i: usize| p.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let threads = u32_at(0).unwrap_or(1);
    match u32_at(4) {
        Some(v) => (threads, v, serde_json::from_slice(&p[8..]).ok()),
        None => (threads, 1, None),
    }
}

// ---------------------------------------------------------------- framing --
pub static SPIN: AtomicBool = AtomicBool::new(false);

/// Socket half that, with --spin, busy-polls a non-blocking socket instead of sleeping.
pub struct Sock(pub TcpStream);

impl Read for Sock {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(b) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && SPIN.load(Relaxed) => std::hint::spin_loop(),
                r => return r,
            }
        }
    }
}

impl Write for Sock {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        loop {
            match self.0.write(b) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && SPIN.load(Relaxed) => std::hint::spin_loop(),
                r => return r,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Buffered reader (one syscall usually fetches header + payload) and a writer for one connection.
pub fn conn(s: TcpStream) -> io::Result<(BufReader<Sock>, Sock)> {
    s.set_nodelay(true)?;
    s.set_nonblocking(SPIN.load(Relaxed))?;
    Ok((BufReader::with_capacity(64 << 10, Sock(s.try_clone()?)), Sock(s)))
}

pub fn send(s: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    s.write_all(&buf)
}

pub fn recv(s: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    recv_max(s, MAX_FRAME)
}

/// `recv` with a smaller frame limit, for the first frame of a connection nobody has vouched for yet.
pub fn recv_max(s: &mut impl Read, max: usize) -> io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    s.read_exact(&mut h)?;
    let len = u32::from_le_bytes(h[1..5].try_into().unwrap()) as usize;
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut p = vec![0; len];
    s.read_exact(&mut p)?;
    Ok((h[0], p))
}

/// `recv_max` with one overall deadline (not per read): a sender that drips a byte per second cannot hold the thread.
pub fn recv_by(s: &mut TcpStream, max: usize, total: Duration) -> io::Result<(u8, Vec<u8>)> {
    struct By<'a>(&'a mut TcpStream, std::time::Instant);
    impl Read for By<'_> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let left = self.1.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "frame deadline"));
            }
            self.0.set_read_timeout(Some(left))?;
            self.0.read(b)
        }
    }
    recv_max(&mut By(s, std::time::Instant::now() + total), max)
}

pub fn pack(nums: &[u64]) -> Vec<u8> {
    nums.iter().flat_map(|n| n.to_le_bytes()).collect()
}

pub fn u64_at(p: &[u8], i: usize) -> io::Result<u64> {
    p.get(i..i + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short frame"))
}

/// Fleet mode: `CK_FLEET=open` admits any agent without codes or keys (development, chaos tests on one machine).
pub fn fleet_open() -> bool {
    std::env::var("CK_FLEET").is_ok_and(|v| v == "open")
}

/// Sealed frame: payload + 40-byte tag (seq u64 LE + HMAC) from crate::auth::FrameMac. Use one FrameMac per
/// direction (the writer only seals, the reader only opens), both built from the node key.
pub fn send_sealed(s: &mut impl Write, mac: &mut crate::auth::FrameMac, kind: u8, payload: &[u8]) -> io::Result<()> {
    let tag = mac.seal(kind, payload);
    // one buffer: header, payload, tag (was three copies of a 4 MB piece)
    let mut p = Vec::with_capacity(payload.len() + 45);
    p.push(kind);
    p.extend_from_slice(&((payload.len() + 40) as u32).to_le_bytes());
    p.extend_from_slice(payload);
    p.extend_from_slice(&tag);
    s.write_all(&p)
}

/// Receive and check a sealed frame; a bad, replayed or reordered tag is an error (drop the connection).
pub fn recv_sealed(s: &mut impl Read, mac: &mut crate::auth::FrameMac) -> io::Result<(u8, Vec<u8>)> {
    let (k, mut p) = recv(s)?;
    if p.len() < 40 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsealed frame"));
    }
    let tag = p.split_off(p.len() - 40);
    if !mac.open(k, &p, &tag) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame tag"));
    }
    Ok((k, p))
}

pub fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?; // no packet is sent; just asks the OS which interface it would use
    s.local_addr().ok().map(|a| a.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip_and_v1() {
        let h = Hello { node_id: "abc/v3".into(), host: "lab-a-07".into(), virtual_node: true, inv: Inventory { cores_logical: 12, ..Default::default() }, join_code: None };
        assert_eq!(parse_hello(&hello_payload(12, &h)), (12, PROTO_VERSION, Some(h)));
        assert_eq!(parse_hello(&8u32.to_le_bytes()), (8, 1, None), "v1 worker");
        let mut buf = vec![];
        send(&mut buf, HEARTBEAT, &[]).unwrap();
        assert_eq!(recv(&mut &buf[..]).unwrap(), (HEARTBEAT, vec![]));
        let (mut a, mut b) = (crate::auth::FrameMac::new(b"k"), crate::auth::FrameMac::new(b"k"));
        let mut buf = vec![];
        send_sealed(&mut buf, &mut a, TASK, b"abc").unwrap();
        let copy = buf.clone();
        assert_eq!(recv_sealed(&mut &buf[..], &mut b).unwrap(), (TASK, b"abc".to_vec()));
        assert!(recv_sealed(&mut &copy[..], &mut b).is_err(), "replay refused");
        let mut bad = vec![];
        send_sealed(&mut bad, &mut a, TASK, b"abc").unwrap();
        bad[6] ^= 1; // flip a payload bit
        assert!(recv_sealed(&mut &bad[..], &mut b).is_err(), "tamper refused");
    }
}
