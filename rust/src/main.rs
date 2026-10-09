//! ck: Dask-free cluster over raw TCP with a binary protocol.
//!
//!   ck head   [--name lab] [--threads N] [--no-local-worker] [--spin] [--gpu]
//!   ck worker [--name lab] [--threads N] [--head IP] [--spin] [--gpu]
//!   ck run    [--head IP] --n 1e9 [--from 1] [--chunk 1e7] (--sha PREFIX | --cmd "prog")
//!   ck ping   [--head IP] [--count 10000] [--size 8] [--spin]   (round-trip latency / throughput)
//!
//! Workers pull chunks over one persistent TCP connection and keep PIPELINE chunks in
//! flight: while one computes, the next is already queued, so a worker never waits on
//! the network. A RESULT doubles as the next request. Each chunk runs on all cores
//! (rayon). --spin busy-polls sockets instead of sleeping in the kernel: lower latency,
//! but every open connection burns a core while it waits. Dashboard: http://HEAD_IP:7702
//!
//! Frame: [kind u8][len u32 LE][payload]. Numbers in payloads are u64 LE.

#[cfg(feature = "gpu")]
mod gpu;

use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Condvar, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};
use std::{env, thread};

const PORT: u16 = 7700; // TCP: workers + clients
const DISC_PORT: u16 = 7701; // UDP: discovery beacon
const DASH_PORT: u16 = 7702; // HTTP: dashboard
const MAX_FRAME: usize = 1 << 28;
const MAX_EVENTS: usize = 2000; // finished chunks kept for the dashboard's task stream
const PIPELINE: usize = 2; // chunks a worker holds: one computing, one queued

const HELLO_W: u8 = 1; // worker -> head: threads u32
const HELLO_C: u8 = 2; // client -> head
const REQ: u8 = 3; // worker -> head: give me a chunk
const TASK: u8 = 4; // head -> worker: job, start, end, kind u8, arg
const RESULT: u8 = 5; // worker -> head: job, start, end, cpu x10, mem used, mem total, compute us, body (also requests the next chunk)
const RESULT_HDR: usize = 56;
const SUBMIT: u8 = 6; // client -> head: start, end, chunk, kind u8, arg
const PART: u8 = 7; // head -> client: start, end, body
const DONE: u8 = 8; // head -> client
const PING: u8 = 9;
const PONG: u8 = 10;
const ERR: u8 = 11; // head -> client: message
const CANCEL: u8 = 12; // head -> worker: job (stop its running chunk, skip its queued ones)

const KIND_SHA: u8 = 0; // arg = prefix as nibbles; body = (n u64, sha256 [u8; 32])*
const KIND_CMD: u8 = 1; // arg = command line; body = its stdout

// ---------------------------------------------------------------- framing --
static SPIN: AtomicBool = AtomicBool::new(false);

/// Socket half that, with --spin, busy-polls a non-blocking socket instead of sleeping.
struct Sock(TcpStream);

impl Read for Sock {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(b) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::hint::spin_loop(),
                r => return r,
            }
        }
    }
}

impl Write for Sock {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        loop {
            match self.0.write(b) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::hint::spin_loop(),
                r => return r,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Buffered reader (one syscall usually fetches header + payload) and a writer for one connection.
fn conn(s: TcpStream) -> io::Result<(BufReader<Sock>, Sock)> {
    s.set_nodelay(true)?;
    s.set_nonblocking(SPIN.load(Relaxed))?;
    Ok((BufReader::with_capacity(64 << 10, Sock(s.try_clone()?)), Sock(s)))
}

fn send(s: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    s.write_all(&buf)
}

fn recv(s: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    s.read_exact(&mut h)?;
    let len = u32::from_le_bytes(h[1..5].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut p = vec![0; len];
    s.read_exact(&mut p)?;
    Ok((h[0], p))
}

fn pack(nums: &[u64]) -> Vec<u8> {
    nums.iter().flat_map(|n| n.to_le_bytes()).collect()
}

fn u64_at(p: &[u8], i: usize) -> io::Result<u64> {
    p.get(i..i + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short frame"))
}

// ------------------------------------------------------------------- work --
fn dec(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return &buf[i..];
        }
    }
}

fn encode_hits(ns: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let mut out = vec![];
    for n in ns {
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&Sha256::digest(dec(n, &mut [0; 20])));
    }
    out
}

#[cfg(feature = "gpu")]
static GPU: OnceLock<Option<gpu::Gpu>> = OnceLock::new();

/// GPU path for a SHA chunk, or None to use the CPU (no --gpu, no GPU, or out of its limits).
fn gpu_sha(job: u64, a: u64, b: u64, nibbles: &[u8]) -> Option<Vec<u8>> {
    #[cfg(feature = "gpu")]
    if let Some(Some(g)) = GPU.get() {
        return g.matches(a, b, nibbles, &CANCELLED, job).map(encode_hits);
    }
    let _ = (job, a, b, nibbles);
    None
}

const SHA256_INIT: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/// SHA-256 state words of a number's decimal string. At most 20 bytes always fit one 64-byte
/// block, so this pads by hand and runs the compression (SHA instructions) once, skipping the
/// general-purpose hasher's buffering.
fn sha_words(n: u64) -> [u32; 8] {
    let mut buf = [0u8; 20];
    let msg = dec(n, &mut buf);
    let mut block = [0u8; 64];
    block[..msg.len()].copy_from_slice(msg);
    block[msg.len()] = 0x80;
    block[56..].copy_from_slice(&(msg.len() as u64 * 8).to_be_bytes());
    let mut st = SHA256_INIT;
    sha2::compress256(&mut st, &[*sha2::digest::generic_array::GenericArray::from_slice(&block)]);
    st
}

/// Prefix nibbles as per-word (mask, want), so a candidate is checked without building bytes.
fn prefix_words(nibbles: &[u8]) -> ([u32; 8], [u32; 8]) {
    let (mut mask, mut want) = ([0u32; 8], [0u32; 8]);
    for (i, &c) in nibbles.iter().enumerate() {
        let shift = 28 - 4 * (i % 8) as u32;
        mask[i / 8] |= 0xf << shift;
        want[i / 8] |= (c as u32) << shift;
    }
    (mask, want)
}

fn sha_range(job: u64, a: u64, b: u64, nibbles: &[u8]) -> Vec<u8> {
    if nibbles.len() > 64 {
        return vec![];
    }
    let (mask, want) = prefix_words(nibbles);
    let hits: Vec<u64> = (0..(b - a) as usize)
        .into_par_iter()
        .with_min_len(1 << 10)
        .filter_map(|k| {
            if CANCELLED.load(Relaxed) == job {
                return None; // job cancelled: rush through the rest of the chunk
            }
            let n = a + k as u64;
            let h = sha_words(n);
            (0..8).all(|i| h[i] & mask[i] == want[i]).then_some(n)
        })
        .collect();
    encode_hits(hits)
}

// Split the chunk into one piece per thread and run the program on each piece in parallel,
// so single-threaded programs use every core. Multi-threaded programs: start the worker with --threads 1.
fn run_cmd_par(cmd: &[u8], a: u64, b: u64) -> Vec<u8> {
    let parts = (rayon::current_num_threads() as u64).min(b - a).max(1);
    let step = (b - a).div_ceil(parts);
    (0..parts)
        .into_par_iter()
        .map(|i| run_cmd(cmd, a + i * step, (a + (i + 1) * step).min(b)))
        .collect::<Vec<_>>()
        .concat()
}

fn run_cmd(cmd: &[u8], a: u64, b: u64) -> Vec<u8> {
    let line = format!("{} {a} {b}", String::from_utf8_lossy(cmd));
    let out = if cfg!(windows) {
        Command::new("cmd").args(["/C", &line]).output()
    } else {
        Command::new("sh").args(["-c", &line]).output()
    };
    match out {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => format!("ERROR chunk {a}..{b}: {}\n", String::from_utf8_lossy(&o.stderr).trim()).into_bytes(),
        Err(e) => format!("ERROR chunk {a}..{b}: {e}\n").into_bytes(),
    }
}

// ------------------------------------------------------------------- head --
struct Job {
    id: u64,
    kind: u8,
    arg: Vec<u8>,
    next: u64,
    end: u64,
    chunk: u64,
    total: u64,
    left: u64,
    requeue: Vec<(u64, u64)>, // chunks of workers that disconnected mid-chunk
    out: mpsc::Sender<(u8, Vec<u8>)>,
    started: Instant,
}

#[derive(Default)]
struct Worker {
    addr: String,
    threads: u32,
    chunks: u64,
    numbers: u64,
    rate: f64,
    inflight: Vec<(u64, u64, u64)>, // (job, start, end) sent and not yet returned
    out: Option<Arc<Mutex<Sock>>>,  // writer shared by the feeder (TASK) and cancel (CANCEL)
    cpu: f64,
    mem_used: u64,
    mem_total: u64,
}

#[derive(Default)]
struct State {
    job: Option<Job>,
    last: Option<String>,
    next_id: u64,
    workers: HashMap<u64, Worker>,
    events: VecDeque<(u64, f64, f64)>, // (worker id, chunk start, chunk end) in secs since head start
}

static START: OnceLock<Instant> = OnceLock::new();
fn now() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

type Shared = Arc<(Mutex<State>, Condvar)>;

fn take(st: &mut State) -> Option<(u64, u64, u64, u8, Vec<u8>)> {
    let j = st.job.as_mut()?;
    let (a, b) = match j.requeue.pop() {
        Some(r) => r,
        None if j.next < j.end => {
            let a = j.next;
            j.next = (a + j.chunk).min(j.end);
            (a, j.next)
        }
        None => return None,
    };
    Some((j.id, a, b, j.kind, j.arg.clone()))
}

fn finish(sh: &Shared, wid: u64, p: &[u8]) -> io::Result<()> {
    let (job, a, b) = (u64_at(p, 0)?, u64_at(p, 8)?, u64_at(p, 16)?);
    let (cpu, mem_used, mem_total) = (u64_at(p, 24)?, u64_at(p, 32)?, u64_at(p, 40)?);
    let secs = u64_at(p, 48)? as f64 / 1e6; // compute time measured by the worker (excludes queueing)
    let t1 = now();
    let t0 = t1 - secs;
    let mut st = sh.0.lock().unwrap();
    if let Some(w) = st.workers.get_mut(&wid) {
        w.chunks += 1;
        w.numbers += b - a;
        w.rate = (b - a) as f64 / secs.max(1e-9);
        w.inflight.retain(|&t| t != (job, a, b));
        (w.cpu, w.mem_used, w.mem_total) = (cpu as f64 / 10.0, mem_used, mem_total);
    }
    if st.events.len() == MAX_EVENTS {
        st.events.pop_front();
    }
    st.events.push_back((wid, t0, t1));
    let Some(j) = st.job.as_mut().filter(|j| j.id == job) else { return Ok(()) }; // stale (cancelled job)
    let mut part = pack(&[a, b]);
    part.extend_from_slice(&p[RESULT_HDR..]);
    let _ = j.out.send((PART, part));
    j.left -= b - a;
    if j.left == 0 {
        let secs = j.started.elapsed().as_secs_f64();
        let msg = format!("job {}: {} numbers in {secs:.3}s ({:.1} M/s)", j.id, j.total, j.total as f64 / secs / 1e6);
        let _ = j.out.send((DONE, vec![]));
        println!("{msg}");
        st.last = Some(msg);
        st.job = None;
    }
    Ok(())
}

fn serve_worker(s: TcpStream, sh: Shared, hello: &[u8], wid: u64) -> io::Result<()> {
    let threads = hello.get(..4).map_or(1, |b| u32::from_le_bytes(b.try_into().unwrap()));
    let addr = s.peer_addr()?.ip().to_string();
    println!("+ worker {addr} ({threads} threads)");
    sh.0.lock().unwrap().workers.insert(wid, Worker { addr: addr.clone(), threads, ..Default::default() });

    // Reader (this thread) turns each REQ / RESULT into a credit; the feeder thread spends
    // credits on chunks. Two threads so results keep flowing while the feeder waits for work.
    let (mut r, w) = conn(s)?;
    let w = Arc::new(Mutex::new(w));
    sh.0.lock().unwrap().workers.get_mut(&wid).unwrap().out = Some(w.clone());
    let (credit, credits) = mpsc::channel();
    let sh2 = sh.clone();
    let feeder = thread::spawn(move || feed(w, &sh2, wid, credits));
    let err = read_results(&mut r, &sh, wid, credit).err();

    let mut st = sh.0.lock().unwrap();
    let gone = st.workers.remove(&wid).map(|w| w.inflight).unwrap_or_default();
    if let Some(j) = st.job.as_mut() {
        j.requeue.extend(gone.iter().filter(|t| t.0 == j.id).map(|t| (t.1, t.2)));
    }
    sh.1.notify_all(); // hands requeued chunks to others and lets our feeder see we are gone
    drop(st);
    let _ = r.get_ref().0.shutdown(std::net::Shutdown::Both);
    let _ = feeder.join();
    println!("- worker {addr} ({})", err.map_or("closed".into(), |e| e.to_string()));
    Ok(())
}

fn read_results(r: &mut impl Read, sh: &Shared, wid: u64, credit: mpsc::Sender<()>) -> io::Result<()> {
    loop {
        let (k, p) = recv(r)?;
        if k == RESULT {
            finish(sh, wid, &p)?;
        }
        if k == REQ || k == RESULT {
            let _ = credit.send(());
        }
    }
}

fn feed(w: Arc<Mutex<Sock>>, sh: &Shared, wid: u64, credits: mpsc::Receiver<()>) {
    while credits.recv().is_ok() {
        let (job, a, b, kind, arg) = {
            let mut st = sh.0.lock().unwrap();
            loop {
                if !st.workers.contains_key(&wid) {
                    return; // disconnected; the reader already requeued our chunks
                }
                if let Some(t) = take(&mut st) {
                    st.workers.get_mut(&wid).unwrap().inflight.push((t.0, t.1, t.2));
                    break t;
                }
                st = sh.1.wait(st).unwrap();
            }
        };
        let mut p = pack(&[job, a, b]);
        p.push(kind);
        p.extend_from_slice(&arg);
        if send(&mut *w.lock().unwrap(), TASK, &p).is_err() {
            return; // the reader sees the disconnect and requeues
        }
    }
}

fn cancel(sh: &Shared, id: u64) {
    let outs: Vec<_> = {
        let mut st = sh.0.lock().unwrap();
        if !st.job.as_ref().is_some_and(|j| j.id == id) {
            return;
        }
        st.job = None;
        println!("job {id} cancelled (client left)");
        for w in st.workers.values_mut() {
            w.inflight.retain(|t| t.0 != id);
        }
        st.workers.values().filter_map(|w| w.out.clone()).collect()
    };
    for o in outs {
        let _ = send(&mut *o.lock().unwrap(), CANCEL, &id.to_le_bytes());
    }
}

fn serve_client(s: TcpStream, sh: Shared) -> io::Result<()> {
    let (mut r, mut w) = conn(s)?;
    loop {
        let (k, p) = recv(&mut r)?;
        match k {
            PING => send(&mut w, PONG, &p)?,
            SUBMIT => return run_job(r, w, &sh, &p),
            _ => {}
        }
    }
}

fn run_job(mut r: BufReader<Sock>, mut s: Sock, sh: &Shared, p: &[u8]) -> io::Result<()> {
    let (from, end, chunk) = (u64_at(p, 0)?, u64_at(p, 8)?, u64_at(p, 16)?);
    let (kind, arg) = (*p.get(24).unwrap_or(&KIND_SHA), p.get(25..).unwrap_or(&[]).to_vec());
    let total = end.saturating_sub(from);
    if total == 0 {
        return send(&mut s, DONE, &[]);
    }
    let (tx, rx) = mpsc::channel();
    let id = {
        let mut st = sh.0.lock().unwrap();
        if st.job.is_some() {
            return send(&mut s, ERR, b"a job is already running");
        }
        let pcs = st.workers.len().max(1) as u64;
        // ~16 chunks per PC for load balancing, capped so cancel / the next job waits under a second
        let chunk = if chunk > 0 { chunk } else { (total / (pcs * 16)).clamp(10_000, 20_000_000) };
        st.next_id += 1;
        let id = st.next_id;
        println!("job {id}: {total} numbers, chunk {chunk}, {pcs} workers");
        st.job = Some(Job { id, kind, arg, next: from, end, chunk, total, left: total, requeue: vec![], out: tx, started: Instant::now() });
        sh.1.notify_all();
        id
    };
    // Client closing its socket (Ctrl+C) cancels the job; running chunks finish and are dropped.
    let sh2 = sh.clone();
    thread::spawn(move || {
        let _ = r.read(&mut [0u8; 1]);
        cancel(&sh2, id);
    });
    for (k, body) in rx {
        if let Err(e) = send(&mut s, k, &body) {
            cancel(sh, id);
            return Err(e);
        }
        if k == DONE {
            break;
        }
    }
    Ok(())
}

fn head(o: &Opts) {
    now(); // start the dashboard clock
    let sh: Shared = Arc::default();
    let l = TcpListener::bind(("0.0.0.0", PORT)).unwrap_or_else(|e| die(&format!("TCP {PORT}: {e}")));
    let name = o.name.clone();
    thread::spawn(move || beacon(&name));
    let sh2 = sh.clone();
    thread::spawn(move || dashboard(sh2));
    if !o.no_local {
        let (name, threads) = (o.name.clone(), o.threads);
        let use_gpu = o.gpu;
        thread::spawn(move || worker(Some("127.0.0.1".into()), &name, threads, use_gpu));
    }
    let ip = local_ip().map_or("?".into(), |i| i.to_string());
    println!("HEAD ready  tcp://{ip}:{PORT}  dashboard http://{ip}:{DASH_PORT}  cluster '{}'", o.name);

    let mut next_conn = 0u64;
    for s in l.incoming().flatten() {
        next_conn += 1;
        let (sh, wid) = (sh.clone(), next_conn);
        thread::spawn(move || {
            let mut s = s;
            let _ = s.set_nodelay(true);
            let _ = match recv(&mut s) { // hello is read unbuffered so nothing is left in a buffer
                Ok((HELLO_W, p)) => serve_worker(s, sh, &p, wid),
                Ok((HELLO_C, _)) => serve_client(s, sh),
                _ => Ok(()),
            };
        });
    }
}

// -------------------------------------------------------------- discovery --
fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?; // no packet is sent; just asks the OS which interface it would use
    s.local_addr().ok().map(|a| a.ip())
}

fn beacon(name: &str) {
    let s = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
    s.set_broadcast(true).expect("broadcast");
    let msg = format!("ck/1 {name}");
    loop {
        let mut targets = vec!["255.255.255.255".to_string()];
        if let Some(IpAddr::V4(ip)) = local_ip() {
            let o = ip.octets();
            targets.push(format!("{}.{}.{}.255", o[0], o[1], o[2])); // ponytail: assumes /24, use --head on other subnets
        }
        for t in &targets {
            let _ = s.send_to(msg.as_bytes(), (t.as_str(), DISC_PORT));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn discover(name: &str) -> String {
    let s = UdpSocket::bind(("0.0.0.0", DISC_PORT)).unwrap_or_else(|e| die(&format!("UDP {DISC_PORT}: {e}")));
    let want = format!("ck/1 {name}");
    println!("looking for head '{name}' on UDP {DISC_PORT} ...");
    let mut b = [0u8; 256];
    loop {
        if let Ok((n, from)) = s.recv_from(&mut b) {
            if &b[..n] == want.as_bytes() {
                return from.ip().to_string();
            }
        }
    }
}

// ----------------------------------------------------------------- worker --
// Sampled once a second in the background and sent with every RESULT.
static CPU_X10: AtomicU64 = AtomicU64::new(0);
static MEM_USED: AtomicU64 = AtomicU64::new(0);
static MEM_TOTAL: AtomicU64 = AtomicU64::new(0);
static CANCELLED: AtomicU64 = AtomicU64::new(0); // last job the head cancelled

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

fn worker(head: Option<String>, name: &str, threads: usize, use_gpu: bool) {
    if use_gpu {
        #[cfg(feature = "gpu")]
        match GPU.get_or_init(gpu::Gpu::new) {
            Some(g) => println!("GPU: {} (SHA jobs run on it)", g.name),
            None => println!("GPU: none found, using the CPU"),
        }
        #[cfg(not(feature = "gpu"))]
        println!("GPU: this ck was built without the gpu feature, using the CPU");
    }
    static SAMPLER: Once = Once::new();
    SAMPLER.call_once(|| {
        thread::spawn(sampler);
    });
    loop {
        let ip = head.clone().unwrap_or_else(|| discover(name));
        match TcpStream::connect((ip.as_str(), PORT)) {
            Ok(s) => {
                println!("connected to head {ip} ({threads} threads)");
                if let Err(e) = work(s, threads as u32) {
                    println!("lost head {ip}: {e}");
                }
            }
            Err(e) => println!("cannot reach head {ip}: {e}"),
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn work(s: TcpStream, threads: u32) -> io::Result<()> {
    let (mut r, mut w) = conn(s)?;
    CANCELLED.store(0, Relaxed); // job ids restart with each head
    send(&mut w, HELLO_W, &threads.to_le_bytes())?;
    for _ in 0..PIPELINE {
        send(&mut w, REQ, &[])?;
    }
    // Reader thread: queues TASKs and applies CANCEL at once, even mid-chunk.
    let (tx, tasks) = mpsc::channel();
    thread::spawn(move || {
        while let Ok((k, p)) = recv(&mut r) {
            if k == CANCEL {
                CANCELLED.store(u64_at(&p, 0).unwrap_or(0), Relaxed);
            } else if k == TASK && tx.send(p).is_err() {
                break;
            }
        }
    });
    for p in tasks {
        let (job, a, b) = (u64_at(&p, 0)?, u64_at(&p, 8)?, u64_at(&p, 16)?);
        if CANCELLED.load(Relaxed) == job {
            send(&mut w, REQ, &[])?; // skip it, but keep the pipeline full
            continue;
        }
        let (kind, arg) = (p.get(24).copied().unwrap_or(KIND_SHA), p.get(25..).unwrap_or(&[]));
        let t = Instant::now();
        let body = if kind == KIND_SHA {
            gpu_sha(job, a, b, arg).unwrap_or_else(|| sha_range(job, a, b, arg))
        } else {
            run_cmd_par(arg, a, b)
        };
        if CANCELLED.load(Relaxed) == job {
            send(&mut w, REQ, &[])?;
            continue;
        }
        let us = t.elapsed().as_micros() as u64;
        let mut res = pack(&[job, a, b, CPU_X10.load(Relaxed), MEM_USED.load(Relaxed), MEM_TOTAL.load(Relaxed), us]);
        res.extend_from_slice(&body);
        send(&mut w, RESULT, &res)?;
    }
    Err(io::Error::new(io::ErrorKind::ConnectionAborted, "head closed the connection"))
}

// ----------------------------------------------------------------- client --
fn connect_client(o: &Opts) -> (BufReader<Sock>, Sock) {
    let ip = o.head.as_deref().unwrap_or("127.0.0.1");
    let s = TcpStream::connect((ip, PORT)).unwrap_or_else(|e| die(&format!("cannot reach head {ip}:{PORT}: {e}")));
    let (r, mut w) = conn(s).unwrap();
    send(&mut w, HELLO_C, &[]).unwrap();
    (r, w)
}

fn run(o: &Opts) {
    let (kind, arg) = match (&o.sha, &o.cmd) {
        (Some(p), None) => {
            let nib: Option<Vec<u8>> = p.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect();
            match nib.filter(|n| n.len() <= 64) {
                Some(n) => (KIND_SHA, n),
                None => die("--sha must be up to 64 hex digits"),
            }
        }
        (None, Some(c)) => (KIND_CMD, c.as_bytes().to_vec()),
        _ => die("give exactly one of --sha PREFIX or --cmd \"program\""),
    };
    let end = o.from.checked_add(o.n).unwrap_or_else(|| die("--from + --n overflows"));
    let (mut r, mut s) = connect_client(o);
    let mut p = pack(&[o.from, end, o.chunk]);
    p.push(kind);
    p.extend_from_slice(&arg);
    let t = Instant::now();
    send(&mut s, SUBMIT, &p).unwrap();

    let mut hits: Vec<(u64, [u8; 32])> = vec![];
    let mut stdout = io::stdout().lock();
    loop {
        let (k, body) = recv(&mut r).unwrap_or_else(|e| die(&format!("lost head: {e}")));
        match k {
            PART if kind == KIND_SHA => {
                for r in body[16..].chunks_exact(40) {
                    hits.push((u64::from_le_bytes(r[..8].try_into().unwrap()), r[8..].try_into().unwrap()));
                }
            }
            PART => stdout.write_all(&body[16..]).unwrap(), // chunks arrive in completion order
            DONE => break,
            ERR => die(&String::from_utf8_lossy(&body)),
            _ => {}
        }
    }
    let secs = t.elapsed().as_secs_f64();
    if kind == KIND_SHA {
        hits.sort_unstable();
        println!("{} hashes start with '{}' in {}..{}", hits.len(), o.sha.as_ref().unwrap(), o.from, end - 1);
        for (n, d) in hits.iter().take(10) {
            println!("  {n:>14}  {}", d.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
    }
    eprintln!("{} numbers in {secs:.3}s = {:.1} M/s", o.n, o.n as f64 / secs / 1e6);
}

fn ping(o: &Opts) {
    let (mut r, mut s) = connect_client(o);
    let buf = vec![0u8; o.size];
    let mut v: Vec<f64> = (0..o.count)
        .map(|_| {
            let t = Instant::now();
            send(&mut s, PING, &buf).unwrap();
            recv(&mut r).unwrap();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    v.sort_by(f64::total_cmp);
    let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
    let mbs = 2.0 * o.size as f64 / q(0.5); // bytes per us == MB/s, both directions
    println!("{} B round trip over {} pings: min {:.1} us  median {:.1} us  p99 {:.1} us  ({mbs:.0} MB/s)", o.size, v.len(), q(0.0), q(0.5), q(0.99));
}

// -------------------------------------------------------------- dashboard --
fn dashboard(sh: Shared) {
    let l = TcpListener::bind(("0.0.0.0", DASH_PORT)).unwrap_or_else(|e| die(&format!("TCP {DASH_PORT}: {e}")));
    for s in l.incoming().flatten() {
        let sh = sh.clone();
        thread::spawn(move || http(s, &sh));
    }
}

fn http(mut s: TcpStream, sh: &Shared) -> io::Result<()> {
    let mut b = [0u8; 1024];
    let n = s.read(&mut b)?;
    let (ct, body) = if b[..n].starts_with(b"GET /stats") {
        ("application/json", stats(sh, b[..n].starts_with(b"GET /stats?detail=1")))
    } else {
        ("text/html; charset=utf-8", include_str!("dash.html").to_string())
    };
    write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn stats(sh: &Shared, detail: bool) -> String {
    let st = sh.0.lock().unwrap();
    let job = st.job.as_ref().map_or("null".into(), |j| {
        let kind = if j.kind == KIND_SHA { "sha256" } else { "cmd" };
        format!(r#"{{"id":{},"kind":"{kind}","done":{},"total":{},"secs":{:.3}}}"#, j.id, j.total - j.left, j.total, j.started.elapsed().as_secs_f64())
    });
    let last = st.last.as_ref().map_or("null".into(), |m| format!("\"{m}\""));
    let ws: Vec<String> = st
        .workers
        .iter()
        .map(|(id, w)| {
            format!(
                r#"{{"id":{id},"addr":"{}","threads":{},"chunks":{},"numbers":{},"rate":{:.0},"busy":{},"cpu":{:.1},"mem_used":{},"mem_total":{}}}"#,
                w.addr, w.threads, w.chunks, w.numbers, w.rate, !w.inflight.is_empty(), w.cpu, w.mem_used, w.mem_total
            )
        })
        .collect();
    // Task stream data only when the dashboard's Details toggle is on.
    let events = if detail {
        let ev: Vec<String> = st.events.iter().map(|(w, a, b)| format!("[{w},{a:.3},{b:.3}]")).collect();
        format!(r#","events":[{}]"#, ev.join(","))
    } else {
        String::new()
    };
    format!(r#"{{"now":{:.3},"job":{job},"last":{last},"workers":[{}]{events}}}"#, now(), ws.join(","))
}

// ------------------------------------------------------------------- main --
struct Opts {
    name: String,
    head: Option<String>,
    threads: usize,
    no_local: bool,
    n: u64,
    from: u64,
    chunk: u64,
    sha: Option<String>,
    cmd: Option<String>,
    count: usize,
    size: usize,
    spin: bool,
    gpu: bool,
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1)
}

fn num(s: Option<String>) -> u64 {
    let s = s.unwrap_or_else(|| die("missing number"));
    s.parse::<u64>().ok().or_else(|| s.parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64)).unwrap_or_else(|| die(&format!("bad number: {s}")))
}

fn main() {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let mut o = Opts {
        name: "lab".into(),
        head: None,
        threads: thread::available_parallelism().map_or(1, |n| n.get()),
        no_local: false,
        n: 1_000_000,
        from: 1,
        chunk: 0,
        sha: None,
        cmd: None,
        count: 10_000,
        size: 8,
        spin: false,
        gpu: false,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--name" => o.name = args.next().unwrap_or_else(|| die("--name needs a value")),
            "--head" => o.head = args.next(),
            "--threads" => o.threads = num(args.next()).max(1) as usize,
            "--no-local-worker" => o.no_local = true,
            "--spin" => o.spin = true,
            "--gpu" => o.gpu = true,
            "--n" => o.n = num(args.next()),
            "--from" => o.from = num(args.next()),
            "--chunk" => o.chunk = num(args.next()),
            "--sha" => o.sha = args.next(),
            "--cmd" => o.cmd = args.next(),
            "--size" => o.size = num(args.next()).min(MAX_FRAME as u64) as usize,
            "--count" => o.count = num(args.next()).max(1) as usize,
            _ => die(&format!("unknown option {a}")),
        }
    }
    SPIN.store(o.spin, Relaxed);
    rayon::ThreadPoolBuilder::new().num_threads(o.threads).build_global().unwrap();
    match cmd.as_str() {
        "head" => head(&o),
        "worker" => worker(o.head.clone(), &o.name, o.threads, o.gpu),
        "run" => run(&o),
        "ping" => ping(&o),
        _ => die("usage: ck head|worker|run|ping [options]  (see the top of src/main.rs)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_and_prefix() {
        assert_eq!(dec(0, &mut [0; 20]), b"0");
        assert_eq!(dec(u64::MAX, &mut [0; 20]), b"18446744073709551615");
        let (mask, want) = prefix_words(&[6, 0xb, 8, 6]); // sha256("1") = 6b86b273...
        assert_eq!(sha_words(1)[0] & mask[0], want[0]);
        assert_ne!(sha_words(1)[0] & mask[0], prefix_words(&[6, 0xb, 8, 7]).1[0]);
        for n in [0, 1, 9, 10, 999_999_999, 1_000_000_000, 12_345_678_901_234, u64::MAX] {
            let words: Vec<u8> = sha_words(n).iter().flat_map(|w| w.to_be_bytes()).collect();
            assert_eq!(words[..], Sha256::digest(dec(n, &mut [0; 20]))[..], "n = {n}");
        }
        let (mask, want) = prefix_words(&[6, 0xb, 8, 6]);
        assert_eq!((mask[0], want[0], mask[1]), (0xffff_0000, 0x6b86_0000, 0));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_matches_cpu() {
        let Some(g) = gpu::Gpu::new() else { return eprintln!("no GPU, skipped") };
        let never = AtomicU64::new(0);
        // includes 0, the 1e9 boundary (hi/lo split) and 14-digit numbers (zero-padded lo)
        for (a, b, prefix) in [(0, 20_000_000, "00000"), (999_000_000, 1_001_000_000, "000"), (12_345_678_900_000, 12_345_680_000_000, "000")] {
            let nib: Vec<u8> = prefix.bytes().map(|c| (c as char).to_digit(16).unwrap() as u8).collect();
            let cpu: Vec<u64> = sha_range(u64::MAX, a, b, &nib).chunks_exact(40).map(|r| u64::from_le_bytes(r[..8].try_into().unwrap())).collect();
            let gpu = g.matches(a, b, &nib, &never, u64::MAX).unwrap();
            assert!(!cpu.is_empty());
            assert_eq!(cpu, gpu, "range {a}..{b} prefix {prefix}");
        }
    }
}
