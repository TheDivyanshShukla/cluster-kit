//! Built-in work: SHA-256 prefix search (CPU/GPU), any-language commands, pushed scripts, synthetic tasks.
//! Owner: a5 (wave 3). `run_task` is the contract the agent calls; keep its signature.

use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[cfg(feature = "gpu")]
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{env, fs};

use crate::proto::{KIND_LIST, KIND_SHA, KIND_SYNTH};

pub static CANCELLED: AtomicU64 = AtomicU64::new(0); // last job the head cancelled
// CANCEL_RANGE (HA-06): the one chunk (job, start) the head cancelled last. ponytail: one slot per process; agents run one chunk at a time.
pub static CANCELLED_RANGE: (AtomicU64, AtomicU64) = (AtomicU64::new(u64::MAX), AtomicU64::new(0));

/// Is chunk (job, a) cancelled, as a whole job (CANCEL) or alone (CANCEL_RANGE)?
pub fn cancelled(job: u64, a: u64) -> bool {
    CANCELLED.load(Relaxed) == job || (CANCELLED_RANGE.0.load(Relaxed) == job && CANCELLED_RANGE.1.load(Relaxed) == a)
}

#[cfg(feature = "gpu")]
pub static GPU: OnceLock<Option<crate::gpu::Gpu>> = OnceLock::new();

/// Run one chunk [a, b) of a job and return the RESULT body.
/// Err((retry, message)): the chunk failed; retry = worth running again (JOB-04 exit code 75 or a timeout).
pub fn run_task(kind: u8, job: u64, a: u64, b: u64, arg: &[u8]) -> Result<Vec<u8>, (bool, String)> {
    run_task_out(kind, job, a, b, arg, None)
}

/// Like `run_task`; `out` = the chunk's CK_OUT directory (cmd and list jobs, JOB-08).
pub fn run_task_out(kind: u8, job: u64, a: u64, b: u64, arg: &[u8], out: Option<&std::path::Path>) -> Result<Vec<u8>, (bool, String)> {
    match kind {
        KIND_SHA => Ok(gpu_sha(job, a, b, arg).unwrap_or_else(|| sha_range(job, a, b, arg))),
        KIND_SYNTH => Ok(synth_range(job, a, b, arg)),
        KIND_LIST => run_list_par(job, arg, a, b, out),
        _ => run_cmd_par(job, arg, a, b, out),
    }
}

/// Synthetic task for tests and simulation: (b - a) * ns_per_number of compute, spread over the rayon
/// threads (each sleeps its share, so wall time = compute / threads). Sleeps in <= 50 ms slices and stops
/// early when the job is cancelled. Body = count of numbers u64 LE.
pub fn synth_range(job: u64, a: u64, b: u64, arg: &[u8]) -> Vec<u8> {
    let ns = arg.get(..8).map_or(0, |x| u64::from_le_bytes(x.try_into().unwrap()));
    let threads = rayon::current_num_threads() as u64;
    let share = Duration::from_nanos(((b - a) as u128 * ns as u128 / threads as u128).min(u64::MAX as u128) as u64);
    (0..threads).into_par_iter().for_each(|_| {
        let end = Instant::now() + share;
        while !cancelled(job, a) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    });
    (b - a).to_le_bytes().to_vec()
}

pub fn dec(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
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

pub fn sha_range(job: u64, a: u64, b: u64, nibbles: &[u8]) -> Vec<u8> {
    if nibbles.len() > 64 {
        return vec![];
    }
    let (mask, want) = prefix_words(nibbles);
    let hits: Vec<u64> = (0..(b - a) as usize)
        .into_par_iter()
        .with_min_len(1 << 10)
        .filter_map(|k| {
            if cancelled(job, a) {
                return None; // chunk cancelled: rush through the rest of the chunk
            }
            let n = a + k as u64;
            let h = sha_words(n);
            (0..8).all(|i| h[i] & mask[i] == want[i]).then_some(n)
        })
        .collect();
    encode_hits(hits)
}

// Wave 10b: per-job memory hint (CTRL {"job": {"id", "mem_mb"}}); bounded, oldest ids dropped.
static JOB_MEM: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());

/// Parse a CTRL body; Some((job, mem_mb)) for {"job": {"id": J, "mem_mb": M}}.
pub fn parse_job_mem(p: &[u8]) -> Option<(u64, u64)> {
    let v: serde_json::Value = serde_json::from_slice(p).ok()?;
    let j = v.get("job")?;
    Some((j.get("id")?.as_u64()?, j.get("mem_mb").and_then(|m| m.as_u64()).unwrap_or(0)))
}

/// Remember mem_mb for the job in a CTRL body (no-op for other CTRL messages).
pub fn note_job_ctrl(p: &[u8]) {
    if let Some((id, m)) = parse_job_mem(p) {
        let mut v = JOB_MEM.lock().unwrap();
        v.retain(|x| x.0 != id);
        v.push((id, m));
        if v.len() > 64 {
            v.remove(0);
        }
    }
}

/// Copies of the program to run: min(threads, max(1, floor(free_mb * 0.8 / mem_mb))); mem_mb 0 = no cap.
pub fn copy_count(threads: u64, free_mb: u64, mem_mb: u64) -> u64 {
    if mem_mb == 0 { threads.max(1) } else { threads.min((free_mb * 4 / 5 / mem_mb).max(1)).max(1) }
}

fn copies_for(job: u64, threads: u64) -> u64 {
    let m = JOB_MEM.lock().unwrap().iter().find(|x| x.0 == job).map_or(0, |x| x.1);
    if m == 0 {
        return threads;
    }
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    copy_count(threads, s.available_memory() / 1048576, m)
}

// A process start costs 10-40 ms (python3 more). Splitting a chunk of cheap items over every thread starts many
// processes for nothing (2,000 SDK items: 640 python starts). So from a job's second chunk on, a piece must
// carry about PIECE_SECS of work: pieces = ceil(items x measured seconds per item / PIECE_SECS), at most `copies`.
// The first chunk of a job (no measurement yet) still uses every copy. The measured cost includes the start itself,
// so cheap items settle at one or two pieces and slow items (>= PIECE_SECS x copies / items) keep all copies.
const PIECE_SECS: f64 = 0.25;
static PIECE_COST: std::sync::Mutex<Vec<(u64, f64)>> = std::sync::Mutex::new(Vec::new());

fn pieces_for(job: u64, items: u64, copies: u64) -> u64 {
    let c = PIECE_COST.lock().unwrap().iter().find(|x| x.0 == job).map(|x| x.1);
    match c {
        Some(c) => ((items as f64 * c / PIECE_SECS).ceil() as u64).clamp(1, copies.max(1)),
        None => copies,
    }
}

/// Forget all measurements (the agent calls this when it joins a head: job ids restart with each head).
pub fn reset_piece_costs() {
    PIECE_COST.lock().unwrap().clear();
}

/// Remember seconds per item (all pieces' busy time / items) for the job; bounded like JOB_MEM.
fn note_piece_cost(job: u64, items: u64, parts: u64, wall: f64) {
    if items == 0 {
        return;
    }
    let busy = wall * parts.min(rayon::current_num_threads() as u64) as f64;
    let x = busy / items as f64;
    let mut v = PIECE_COST.lock().unwrap();
    match v.iter_mut().find(|e| e.0 == job) {
        Some(e) => e.1 = 0.5 * e.1 + 0.5 * x,
        None => {
            v.push((job, x));
            if v.len() > 64 {
                v.remove(0);
            }
        }
    }
}

// Split the chunk into one piece per thread and run the program on each piece in parallel,
// so single-threaded programs use every core. Multi-threaded programs: start the worker with --threads 1.
fn run_cmd_par(job: u64, cmd: &[u8], a: u64, b: u64, out: Option<&std::path::Path>) -> Result<Vec<u8>, (bool, String)> {
    let parts = pieces_for(job, b - a, copies_for(job, rayon::current_num_threads() as u64)).min(b - a).max(1);
    let step = (b - a).div_ceil(parts);
    let t = Instant::now();
    let res: Vec<_> = (0..parts)
        .into_par_iter()
        .map(|i| run_cmd(cmd, job, parts, a + i * step, (a + (i + 1) * step).min(b), a, out))
        .collect();
    note_piece_cost(job, b - a, parts, t.elapsed().as_secs_f64());
    let fails: Vec<&(bool, String)> = res.iter().filter_map(|r| r.as_ref().err()).collect();
    if let Some(first) = fails.first() {
        return Err((fails.iter().all(|f| f.0), first.1.clone()));
    }
    Ok(res.into_iter().flatten().flatten().collect())
}

/// List job (JOB-01): arg = command, 0x00, items joined by '\n' for [a, b). One contiguous piece of items per thread,
/// the command runs once per piece with its items on stdin (one per line); stdout joined in order.
fn run_list_par(job: u64, arg: &[u8], a: u64, b: u64, out: Option<&std::path::Path>) -> Result<Vec<u8>, (bool, String)> {
    let split = arg.iter().position(|c| *c == 0).unwrap_or(arg.len());
    let (cmd, items) = (&arg[..split], arg.get(split + 1..).unwrap_or(&[]));
    let lines: Vec<std::borrow::Cow<[u8]>> = if items.is_empty() {
        vec![]
    } else {
        items.split(|c| *c == b'\n').map(|l| if l.starts_with(b"@blob:") { std::borrow::Cow::Owned(crate::blobs::resolve_item(l)) } else { std::borrow::Cow::Borrowed(l) }).collect()
    };
    let n = lines.len() as u64;
    let parts = pieces_for(job, n, copies_for(job, rayon::current_num_threads() as u64)).min(n).max(1);
    let step = n.div_ceil(parts).max(1);
    let t = Instant::now();
    let res: Vec<_> = (0..parts)
        .into_par_iter()
        .filter(|i| i * step < n)
        .map(|i| {
            let (lo, hi) = ((i * step) as usize, ((i + 1) * step).min(n) as usize);
            let mut stdin = lines[lo..hi].join(&b'\n');
            stdin.push(b'\n');
            run_cmd_in(cmd, job, parts, a + lo as u64, a + hi as u64, a, Some(&stdin), out)
        })
        .collect();
    note_piece_cost(job, n, parts, t.elapsed().as_secs_f64());
    let _ = b; // the item count, not b, drives the split; the head sends exactly b - a items
    let fails: Vec<&(bool, String)> = res.iter().filter_map(|r| r.as_ref().err()).collect();
    if let Some(first) = fails.first() {
        return Err((fails.iter().all(|f| f.0), first.1.clone()));
    }
    Ok(res.into_iter().flatten().flatten().collect())
}

/// Agent calibration (FLT-06): SHA-256 MH/s measured over about `secs` seconds on all rayon threads.
pub fn calibrate_mhs(secs: f64) -> f64 {
    let t = Instant::now();
    let (mut n, step) = (0u64, 1u64 << 22);
    while t.elapsed().as_secs_f64() < secs {
        sha_range(u64::MAX, n, n + step, &[0xf; 40]); // a 40-nibble prefix: no hits, nothing allocated
        n += step;
    }
    n as f64 / t.elapsed().as_secs_f64() / 1e6
}

// `--push FILE`: the head ships the script itself, so a worker needs no copy of it. The argument is
// "\x01PUSH\n<interpreter>\n<file name>\n<script bytes>". The worker saves the script once under its temp folder
// (named by a hash of the contents) and runs `<interpreter> <saved path> START END`.
pub const PUSH_MARK: &[u8] = b"\x01PUSH\n";
pub const PUSH_MAX: usize = 64 * 1024;

fn command_base(cmd: &[u8]) -> String {
    let Some(rest) = cmd.strip_prefix(PUSH_MARK) else { return String::from_utf8_lossy(cmd).into_owned() };
    let mut lines = rest.splitn(3, |b| *b == b'\n');
    let interp = String::from_utf8_lossy(lines.next().unwrap_or(b"")).into_owned();
    let name: String = String::from_utf8_lossy(lines.next().unwrap_or(b"script"))
        .chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
    let body = lines.next().unwrap_or(b"");
    let name = if name.is_empty() { "script".to_string() } else { name };
    let hash = body.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let dir = env::temp_dir().join(format!("ck-push-{hash:016x}"));
    let path = dir.join(&name);
    if !path.exists() {
        let _ = fs::create_dir_all(&dir);
        let tmp = dir.join(format!("{name}.{}", std::process::id()));
        if fs::write(&tmp, body).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
    format!("{interp} \"{}\"", path.display())
}

fn out_max() -> usize {
    (crate::tune::get("out_max_mb") * 1048576.0) as usize // stdout per piece
}
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A chunk's `CK_OUT` directory (JOB-08): empty at the start, removed when dropped.
pub struct OutDir(pub std::path::PathBuf);

impl OutDir {
    pub fn new() -> Option<OutDir> {
        let d = env::temp_dir().join(format!("ck-out-{}-{}", std::process::id(), SCRATCH_SEQ.fetch_add(1, Relaxed)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).ok().map(|_| OutDir(d))
    }
    /// Regular files in the directory and its sub-directories (symlinks are never followed), sorted: (relative name with `/`, path, size).
    /// Err(message) = over `limit_mb`, too many, or a bad name.
    pub fn files(&self, limit_mb: u64) -> Result<Vec<(String, std::path::PathBuf, u64)>, String> {
        let mut v = vec![];
        let mut total = 0u64;
        let mut todo = vec![(self.0.clone(), String::new(), 0u8)];
        while let Some((dir, prefix, depth)) = todo.pop() {
            for e in fs::read_dir(&dir).map_err(|e| format!("CK_OUT: {e}"))?.flatten() {
                let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                if m.is_dir() && depth < 8 {
                    if name.is_empty() || name.starts_with('.') {
                        return Err(format!("output directory name {name:?} is not allowed (no leading dot)"));
                    }
                    todo.push((e.path(), format!("{prefix}{name}/"), depth + 1));
                    continue;
                }
                if !m.is_file() {
                    continue;
                }
                let rel = format!("{prefix}{name}");
                if name.is_empty() || rel.len() > 200 || name.starts_with('.') || name.contains(['/', '\\']) {
                    return Err(format!("output file name {rel:?} is not allowed (1 to 200 characters, no leading dot, no backslashes)"));
                }
                total += m.len();
                if total > limit_mb << 20 {
                    return Err(format!("output files are over the {limit_mb} MB limit (out_files_mb)"));
                }
                v.push((rel, e.path(), m.len()));
                if v.len() > 1000 {
                    return Err("more than 1000 output files in one chunk".into());
                }
            }
        }
        v.sort();
        Ok(v)
    }
}

/// Bytes of regular files under `d` (symlinks not followed), for the running size check.
fn dir_bytes(d: &std::path::Path, depth: u8) -> u64 {
    let Ok(rd) = fs::read_dir(d) else { return 0 };
    rd.flatten().map(|e| match fs::symlink_metadata(e.path()) {
        Ok(m) if m.is_file() => m.len(),
        Ok(m) if m.is_dir() && depth < 8 => dir_bytes(&e.path(), depth + 1),
        _ => 0,
    }).sum()
}

/// Physical RAM of this PC in bytes (0 = unknown).
fn total_ram() -> u64 {
    static R: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        if let Ok(m) = fs::read_to_string("/proc/meminfo") {
            return m.lines().find_map(|l| l.strip_prefix("MemTotal:")?.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0) * 1024;
        }
        Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0)
    })
}

/// Resident memory (bytes) of every process in group `pgid`. Linux: /proc/<pid>/stat; elsewhere `ps`.
/// ponytail: Linux page size assumed 4096; the sum counts shared pages once per process.
fn group_rss(pgid: u32) -> u64 {
    if let Ok(rd) = fs::read_dir("/proc") {
        let mut sum = 0u64;
        for e in rd.flatten() {
            if !e.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit) { continue }
            let Ok(st) = fs::read_to_string(e.path().join("stat")) else { continue };
            let Some(rest) = st.rsplit_once(')').map(|x| x.1) else { continue };
            let f: Vec<&str> = rest.split_whitespace().collect(); // [0]=state [1]=ppid [2]=pgrp ... [21]=rss
            if f.get(2).and_then(|x| x.parse::<u32>().ok()) == Some(pgid) {
                sum += f.get(21).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0) * 4096;
            }
        }
        return sum;
    }
    let Ok(o) = Command::new("ps").args(["-axo", "pgid=,rss="]).output() else { return 0 };
    String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
        let mut w = l.split_whitespace();
        (w.next()?.parse::<u32>().ok()? == pgid).then(|| w.next()?.parse::<u64>().ok()).flatten()
    }).sum::<u64>() * 1024
}

/// (pid, resident bytes) of every process of this user whose environment carries `CK_RUN_ID=<tag>`: the program, its
/// children, and also ones that left the process group (setsid) or double-forked and reparented to init.
/// Linux: /proc/<pid>/environ; elsewhere `ps -E` (appends the environment to the command).
/// ponytail: a program that clears its own environment escapes (and a daemon that execs with env -i); no cgroup yet.
fn tagged(tag: &str) -> Vec<(u32, u64)> {
    let needle = format!("CK_RUN_ID={tag}");
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|x| x.parse::<u32>().ok()) else { continue };
            let Ok(env) = fs::read(e.path().join("environ")) else { continue }; // other users: not readable
            if !env.split(|&b| b == 0).any(|kv| kv == needle.as_bytes()) { continue }
            let rss = fs::read_to_string(e.path().join("stat")).ok().and_then(|st| {
                st.rsplit_once(')')?.1.split_whitespace().nth(21)?.parse::<u64>().ok()
            }).unwrap_or(0);
            v.push((pid, rss * 4096));
        }
        return v;
    }
    let Ok(o) = Command::new("ps").args(["-axEww", "-o", "pid=,rss=,command="]).output() else { return v };
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        if !l.split_whitespace().any(|w| w == needle) { continue }
        let mut w = l.split_whitespace();
        if let (Some(p), Some(r)) = (w.next().and_then(|x| x.parse::<u32>().ok()), w.next().and_then(|x| x.parse::<u64>().ok())) {
            v.push((p, r * 1024));
        }
    }
    v
}

/// SIGKILL every tagged process, rescanning (a few rounds) so a program forking while it dies is caught.
fn kill_tagged(tag: &str) {
    #[cfg(unix)]
    for _ in 0..5 {
        let v = tagged(tag);
        if v.is_empty() { return }
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        for (pid, _) in v {
            // SAFETY: plain libc call on a pid we just saw carrying our unique tag.
            unsafe { kill(pid as i32, 9) };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    #[cfg(not(unix))]
    let _ = tag;
}

static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

/// What the watchdog enforces while a program runs (0 / None = off).
#[derive(Default, Clone, Copy)]
struct Limits<'a> {
    mem_bytes: u64,
    out: Option<(&'a std::path::Path, u64)>,
}

impl Drop for OutDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One piece of a `--cmd` job (JOB-04): env CK_*, exit 0 = ok, 75 = retry, anything else = failed.
fn run_cmd(cmd: &[u8], job: u64, pieces: u64, a: u64, b: u64, chunk_a: u64, out: Option<&std::path::Path>) -> Result<Vec<u8>, (bool, String)> {
    run_cmd_in(cmd, job, pieces, a, b, chunk_a, None, out)
}

/// `stdin` = Some(items): a list job piece. The command gets no START END arguments, only the env vars and the items.
/// `chunk_a` = start of the whole chunk (a piece's `a` differs): a cancel of the job or of that chunk kills the program.
fn run_cmd_in(cmd: &[u8], job: u64, pieces: u64, a: u64, b: u64, chunk_a: u64, stdin: Option<&[u8]>, out: Option<&std::path::Path>) -> Result<Vec<u8>, (bool, String)> {
    crate::blobs::check_job(job).map_err(|e| (true, e))?;
    let scratch = env::temp_dir().join(format!("ck-scratch-{}-{}", std::process::id(), SCRATCH_SEQ.fetch_add(1, Relaxed)));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|e| (false, format!("scratch dir: {e}")))?;
    let line = if stdin.is_some() { command_base(cmd) } else { format!("{} {a} {b}", command_base(cmd)) };
    let mut c = if cfg!(windows) { let mut c = Command::new("cmd"); c.args(["/C", &line]); c } else { let mut c = Command::new("sh"); c.args(["-c", &line]); c };
    // ponytail: the head owns the attempt count; CK_ATTEMPT is only what the agent's environment says (default 1)
    let attempt = env::var("CK_ATTEMPT").unwrap_or_else(|_| "1".into());
    c.env("CK_JOB", job.to_string()).env("CK_TASK", format!("{job}:{a}-{b}")).env("CK_ATTEMPT", attempt)
        .env("CK_THREADS", pieces.to_string()).env("CK_START", a.to_string()).env("CK_END", b.to_string()).env("CK_SCRATCH", &scratch).env("CK_OUT", out.unwrap_or(&scratch));
    let warm = crate::tune::n("warm_idle_s");
    c.env("CK_WARM_IDLE_S", warm.to_string());
    if warm == 0 { c.env("CK_WARM", "off"); }
    c.envs(crate::blobs::env_for(job)); // wave 13: CK_BLOBS, CK_BLOB_<i>
    let cap = out_max();
    let mem_bytes = (total_ram() as f64 * crate::tune::get("job_mem_pct") / 100.0) as u64;
    let lim = Limits { mem_bytes, out: out.map(|p| (p, crate::tune::n("out_files_mb") << 20)) };
    let out = run_capped(&mut c, stdin, cap, &|| cancelled(job, chunk_a), lim);
    let _ = fs::remove_dir_all(&scratch);
    let (code, stdout, stderr, over, why) = out.map_err(|e| (false, format!("could not start the program: {e}")))?;
    if let Some(w) = why {
        return Err((false, w));
    }
    if over {
        return Err((false, format!("output over the {} MiB limit (program stopped)", cap >> 20)));
    }
    match code {
        Some(0) => Ok(stdout),
        code => {
            let err = String::from_utf8_lossy(&stderr);
            let last: String = err.trim().lines().last().unwrap_or("").chars().take(300).collect();
            let what = code.map_or("killed by a signal".to_string(), |c| format!("exit {c}"));
            Err((code == Some(75), format!("{what}: {last}")))
        }
    }
}

/// Run `c` in its own process group, reading stdout as it comes: past `cap` bytes, or when `stop()` turns true (job or
/// chunk cancelled), the whole group is killed (a program printing 1 GB never sits in RAM; a hung one frees the PC).
/// Children the program left behind are killed when it exits. Returns (exit code, stdout, last 64 KiB of stderr, over-cap);
/// a stopped run returns code None.
/// Every program carries env CK_RUN_ID=<unique>; processes that left the group (setsid, double fork) are found by that tag
/// (scanned at most every 1 s while it runs, and at stop / exit) and killed / counted in the memory cap.
/// ponytail: a program that clears its own environment still escapes; unix only (Windows: no process groups, only the shell is killed on over-cap).
fn run_capped(c: &mut Command, stdin: Option<&[u8]>, cap: usize, stop: &dyn Fn() -> bool, lim: Limits) -> std::io::Result<(Option<i32>, Vec<u8>, Vec<u8>, bool, Option<String>)> {
    use std::io::Read;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(c, 0);
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let tag = format!("{:x}-{:x}-{:x}", std::process::id(), RUN_SEQ.fetch_add(1, Relaxed), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    c.env("CK_RUN_ID", &tag);
    let mut ch = c.spawn()?;
    let pid = ch.id();
    let killgrp = move || {
        #[cfg(unix)]
        {
            extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // SAFETY: plain libc call; a negative pid addresses the process group we created for the child.
            unsafe { kill(-(pid as i32), 9) };
        }
        #[cfg(not(unix))]
        let _ = pid;
    };
    if let Some(items) = stdin {
        let mut si = ch.stdin.take().unwrap();
        let items = items.to_vec();
        // a thread, so a program that fills its output pipe before reading all input cannot deadlock us
        std::thread::spawn(move || { let _ = si.write_all(&items); }); // a closed pipe (program ignored stdin) is fine
    }
    let mut se = ch.stderr.take().unwrap();
    let (err_tx, err_rx) = channel();
    std::thread::spawn(move || {
        let (mut tail, mut buf) = (Vec::<u8>::new(), [0u8; 8192]);
        while let Ok(n) = se.read(&mut buf) {
            if n == 0 { break; }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 131072 { tail.drain(..tail.len() - 65536); }
        }
        let _ = err_tx.send(tail);
    });
    let mut so = ch.stdout.take().unwrap();
    let (out_tx, out_rx) = channel();
    std::thread::spawn(move || {
        let (mut out, mut buf, mut over) = (Vec::new(), vec![0u8; 65536], false);
        loop {
            let n = so.read(&mut buf).unwrap_or(0);
            if n == 0 { break; }
            if out.len() + n > cap { over = true; killgrp(); break; }
            out.extend_from_slice(&buf[..n]);
        }
        let _ = out_tx.send((out, over));
    });
    // Wait for stdout to close (normally the moment the program exits), checking every 20 ms for a cancel and for a program
    // that exited while a child it left behind still holds the pipe (killing the group closes it).
    // The same tick is the memory / CK_OUT watchdog: sampled every 300 / 500 ms, and only once the program has run 250 ms,
    // so a short piece never pays for it.
    let (born, mut mem_at, mut out_at, mut tag_at, mut exited) = (Instant::now(), Instant::now(), Instant::now(), Instant::now(), false);
    let (mut stopped, mut why) = (false, None::<String>);
    let (out, over) = loop {
        match out_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(v) => break v,
            Err(RecvTimeoutError::Timeout) if !stopped && stop() => {
                stopped = true;
                killgrp();
                kill_tagged(&tag);
                let _ = ch.kill(); // off unix there is no group: at least the program itself
            }
            Err(RecvTimeoutError::Timeout) => {
                if !stopped && born.elapsed() >= Duration::from_millis(250) {
                    if lim.mem_bytes > 0 && mem_at.elapsed() >= Duration::from_millis(300) {
                        mem_at = Instant::now();
                        if group_rss(pid) > lim.mem_bytes {
                            why = Some(format!("memory over the {} MB limit (job_mem_pct), program stopped", lim.mem_bytes >> 20));
                        }
                    }
                    if why.is_none() && lim.mem_bytes > 0 && tag_at.elapsed() >= Duration::from_secs(1) {
                        tag_at = Instant::now();
                        if tagged(&tag).iter().map(|x| x.1).sum::<u64>() > lim.mem_bytes {
                            why = Some(format!("memory over the {} MB limit (job_mem_pct), program stopped", lim.mem_bytes >> 20));
                        }
                    }
                    if let (None, Some((d, max)), true) = (&why, lim.out, out_at.elapsed() >= Duration::from_millis(500)) {
                        out_at = Instant::now();
                        if dir_bytes(d, 0) > max {
                            why = Some(format!("output files are over the {} MB limit (out_files_mb), program stopped", max >> 20));
                        }
                    }
                    if why.is_some() {
                        stopped = true;
                        killgrp();
                        kill_tagged(&tag);
                        let _ = ch.kill();
                    }
                }
                if ch.try_wait()?.is_some() {
                    killgrp();
                    // a leftover that left the group may still hold stdout: kill it at once, then rescan at most every 1 s
                    if !exited || tag_at.elapsed() >= Duration::from_secs(1) { exited = true; tag_at = Instant::now(); kill_tagged(&tag); }
                }
            }
            Err(_) => break (Vec::new(), false),
        }
    };
    let status = ch.wait()?;
    killgrp(); // background children of a finished chunk
    kill_tagged(&tag); // and the ones that left the group
    if over {
        return Ok((None, Vec::new(), Vec::new(), true, None));
    }
    if stopped {
        return Ok((None, Vec::new(), Vec::new(), false, why));
    }
    let tail = err_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    Ok((status.code(), out, tail, false, None))
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

    #[cfg(unix)]
    #[test]
    fn cmd_output_cap_stops_the_program() {
        let t = Instant::now();
        let mut c = Command::new("sh");
        c.args(["-c", "yes | head -c 1000000000"]);
        let (_, out, _, over, _) = run_capped(&mut c, None, 1 << 20, &|| false, Limits::default()).unwrap();
        assert!(over && out.is_empty() && t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        let mut c = Command::new("sh");
        c.args(["-c", "cat; echo e >&2; exit 3"]);
        let (code, out, err, over, _) = run_capped(&mut c, Some(b"hi"), 1 << 20, &|| false, Limits::default()).unwrap();
        assert_eq!((code, out, err, over), (Some(3), b"hi".to_vec(), b"e\n".to_vec(), false));
        // a cancel kills the whole group: a hung program and its background child, both holding stdout
        let t = Instant::now();
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 100 & sleep 100"]);
        let (code, _, _, over, _) = run_capped(&mut c, None, 1 << 20, &|| t.elapsed() > Duration::from_millis(200), Limits::default()).unwrap();
        assert!(code.is_none() && !over && t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
        // a program that exits leaving a child on the pipe returns at once (the group is killed)
        let t = Instant::now();
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 100 & echo done"]);
        let (code, out, _, _, _) = run_capped(&mut c, None, 1 << 20, &|| false, Limits::default()).unwrap();
        assert!(code == Some(0) && out == b"done\n" && t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_kills_memory_hog_and_runaway_output() {
        // ~300 MB resident against a 100 MB cap
        let t = Instant::now();
        let mut c = Command::new("sh");
        c.args(["-c", "exec python3 -c 'b = bytearray(300 * 1024 * 1024); b[::4096] = b\"x\" * len(b[::4096]); import time; time.sleep(30)'"]);
        let (code, _, _, _, why) = run_capped(&mut c, None, 1 << 20, &|| false, Limits { mem_bytes: 100 << 20, out: None }).unwrap();
        assert!(code.is_none() && why.as_deref().is_some_and(|w| w.contains("memory over the 100 MB")) && t.elapsed().as_secs() < 10, "{why:?} {:?}", t.elapsed());
        // growing CK_OUT past 1 MB while running
        let d = OutDir::new().unwrap();
        let t = Instant::now();
        let mut c = Command::new("sh");
        c.env("CK_OUT", &d.0).args(["-c", "mkdir $CK_OUT/n; while :; do head -c 600000 /dev/zero >> $CK_OUT/n/f; sleep 0.05; done"]);
        let (code, _, _, _, why) = run_capped(&mut c, None, 1 << 20, &|| false, Limits { mem_bytes: 0, out: Some((&d.0, 1 << 20)) }).unwrap();
        assert!(code.is_none() && why.as_deref().is_some_and(|w| w.contains("out_files_mb")) && t.elapsed().as_secs() < 10, "{why:?}");
        // an honest program is left alone
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 0.6; echo ok"]);
        let (code, out, _, _, why) = run_capped(&mut c, None, 1 << 20, &|| false, Limits { mem_bytes: 1 << 30, out: Some((&d.0, 1 << 40)) }).unwrap();
        assert_eq!((code, out, why), (Some(0), b"ok\n".to_vec(), None));
    }

    /// Runs `sh -c "python3 -c $PY & <tail>"`: the python forks, setsid's, forks again (the grandchild reparents to init)
    /// and allocates `mb` MB; returns the run result and the pids still carrying the test marker.
    #[cfg(unix)]
    fn run_daemon(marker: &str, mb: u32, tail: &str, stop: &dyn Fn() -> bool, mem: u64) -> (Option<i32>, Option<String>, Instant) {
        let py = format!("import os,time\nif os.fork()==0:\n os.setsid()\n if os.fork()==0:\n  b=bytearray({mb}*1024*1024); b[::4096]=b'x'*len(b[::4096]); time.sleep(300)\n os._exit(0)\ntime.sleep(300)\n");
        let mut c = Command::new("sh");
        c.env("CK_TEST_MARK", marker).env("PY", py).args(["-c", &format!("python3 -c \"$PY\" & {tail}")]);
        let t = Instant::now();
        let (code, _, _, _, why) = run_capped(&mut c, None, 1 << 20, stop, Limits { mem_bytes: mem, out: None }).unwrap();
        (code, why, t)
    }

    #[cfg(unix)]
    fn left(marker: &str) -> Vec<String> {
        let o = Command::new("ps").args(["-axEww", "-o", "pid=,command="]).output().unwrap();
        let m = format!("CK_TEST_MARK={marker}");
        String::from_utf8_lossy(&o.stdout).lines().filter(|l| l.split_whitespace().any(|w| w == m) && l.contains("python")).map(String::from).collect()
    }

    #[cfg(unix)]
    #[test]
    fn setsid_daemons_die_on_cancel_exit_and_memory_cap() {
        let marker = format!("m{}", std::process::id());
        // exit: the program ends, the setsid grandchild must be gone
        let (code, _, t) = run_daemon(&marker, 0, "sleep 1.2; echo hi", &|| false, 0);
        assert_eq!(code, Some(0));
        assert!(t.elapsed().as_secs() < 8);
        std::thread::sleep(Duration::from_millis(300));
        assert!(left(&marker).is_empty(), "after exit: {:?}", left(&marker));
        // cancel
        let t0 = Instant::now();
        let (code, _, _) = run_daemon(&marker, 0, "sleep 100", &|| t0.elapsed() > Duration::from_millis(1500), 0);
        assert!(code.is_none());
        std::thread::sleep(Duration::from_millis(300));
        assert!(left(&marker).is_empty(), "after cancel: {:?}", left(&marker));
        // memory: only the setsid grandchild holds 300 MB; the cap is 100 MB
        let (code, why, t) = run_daemon(&marker, 300, "sleep 100", &|| false, 100 << 20);
        assert!(code.is_none() && why.as_deref().is_some_and(|w| w.contains("memory over the 100 MB")) && t.elapsed().as_secs() < 15, "{why:?} {:?}", t.elapsed());
        std::thread::sleep(Duration::from_millis(300));
        assert!(left(&marker).is_empty(), "after memory kill: {:?}", left(&marker));
    }

    #[test]
    fn cheap_items_get_fewer_pieces() {
        assert_eq!(pieces_for(9_000_001, 31, 10), 10, "no measurement: every copy");
        note_piece_cost(9_000_001, 31, 10, 0.02); // 0.2 s busy for 31 items
        assert_eq!(pieces_for(9_000_001, 31, 10), 1);
        note_piece_cost(9_000_002, 31, 10, 2.0); // 20 s busy for 31 items: slow
        assert_eq!(pieces_for(9_000_002, 31, 10), 10);
    }

    #[test]
    fn synth_duration_and_cancel() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let ns = 1_000_000u64.to_le_bytes(); // 1 ms per number
        let t = Instant::now();
        let body = pool.install(|| synth_range(u64::MAX - 1, 0, 400, &ns)); // 400 ms compute / 4 threads
        assert_eq!(body, 400u64.to_le_bytes());
        let d = t.elapsed();
        assert!(d >= Duration::from_millis(90) && d < Duration::from_millis(400), "{d:?}");
        CANCELLED.store(77, Relaxed);
        let t = Instant::now();
        pool.install(|| synth_range(77, 0, 40_000, &ns)); // would take 10 s
        assert!(t.elapsed() < Duration::from_millis(200));
        CANCELLED.store(0, Relaxed);
    }

    #[test]
    fn range_cancel_is_per_chunk() {
        CANCELLED_RANGE.1.store(10, Relaxed);
        CANCELLED_RANGE.0.store(555, Relaxed);
        assert!(cancelled(555, 10) && !cancelled(555, 20) && !cancelled(556, 10));
        CANCELLED_RANGE.0.store(u64::MAX, Relaxed);
        assert!(!cancelled(555, 10));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_matches_cpu() {
        let Some(g) = crate::gpu::Gpu::new() else { return eprintln!("no GPU, skipped") };
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

    #[cfg(unix)]
    fn cmd(script: &str, a: u64, b: u64) -> Result<Vec<u8>, (bool, String)> {
        run_cmd(format!("sh -c '{script}' x").as_bytes(), 7, 2, a, b, a, None)
    }

    #[cfg(unix)]
    #[test]
    fn cmd_env_and_ok() {
        assert_eq!(cmd("echo $CK_JOB $CK_TASK $CK_START $CK_END $CK_THREADS $CK_ATTEMPT", 5, 9).unwrap(), b"7 7:5-9 5 9 2 1\n");
        assert_eq!(cmd("echo \"$1-$2\"", 5, 9).unwrap(), b"5-9\n");
    }

    #[cfg(unix)]
    #[test]
    fn copy_count_caps_by_ram() {
        assert_eq!(copy_count(8, 10_000, 0), 8);
        assert_eq!(copy_count(8, 10_000, 2_000), 4); // 8000 / 2000
        assert_eq!(copy_count(8, 100, 2_000), 1);
        assert_eq!(copy_count(2, 100_000, 100), 2);
    }

    #[test]
    fn ctrl_job_parsed() {
        assert_eq!(parse_job_mem(br#"{"job": {"id": 9, "mem_mb": 512}}"#), Some((9, 512)));
        assert_eq!(parse_job_mem(br#"{"tune": {}}"#), None);
        note_job_ctrl(br#"{"job": {"id": 4242, "mem_mb": 300}}"#);
        assert!(JOB_MEM.lock().unwrap().contains(&(4242, 300)));
    }

    #[test]
    fn cmd_exit_codes() {
        let e = cmd("echo oops >&2; exit 75", 0, 1).unwrap_err();
        assert!(e.0 && e.1.contains("75") && e.1.contains("oops"), "{e:?}");
        let e = cmd("echo a >&2; echo last >&2; exit 3", 0, 1).unwrap_err();
        assert!(!e.0 && e.1 == "exit 3: last", "{e:?}");
        assert!(run_cmd(b"/no/such/program-x", 1, 1, 0, 1, 0, None).is_err());
        // mixed pieces: retry only if every failing piece said 75
        assert_eq!(run_cmd_par(1, b"sh -c 'exit 75' x", 0, 4, None).unwrap_err().0, true);
    }

    #[cfg(unix)]
    #[test]
    fn list_split_and_stdin() {
        let items = (0..10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let arg = |c: &str| [c.as_bytes(), b"\0", items.as_bytes()].concat();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        // 10 items over 3 threads: pieces of 4, 4, 2 lines
        let out = pool.install(|| run_list_par(7700, &arg("sh -c 'wc -l'"), 100, 110, None)).unwrap();
        let counts: Vec<u32> = String::from_utf8(out).unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
        assert_eq!(counts, [4, 4, 2]);
        // order, env range and stdin content
        let out = pool.install(|| run_list_par(7702, &arg("sh -c 'echo $CK_START-$CK_END; cat'"), 100, 110, None)).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "100-104\n0\n1\n2\n3\n104-108\n4\n5\n6\n7\n108-110\n8\n9\n");
        // exit codes
        let e = pool.install(|| run_list_par(7703, &arg("sh -c 'exit 75'"), 0, 10, None)).unwrap_err();
        assert!(e.0, "{e:?}");
        let e = pool.install(|| run_list_par(7704, &arg("sh -c 'echo bad >&2; exit 2'"), 0, 10, None)).unwrap_err();
        assert!(!e.0 && e.1 == "exit 2: bad", "{e:?}");
        // a program that ignores stdin is fine
        assert!(run_list_par(7705, &arg("true"), 0, 10, None).is_ok());
    }

    #[test]
    fn calibration_runs() {
        assert!(calibrate_mhs(0.2) > 0.1);
    }

    #[cfg(unix)]
    #[test]
    fn cmd_scratch_removed() {
        let p = String::from_utf8(cmd("touch $CK_SCRATCH/f; echo $CK_SCRATCH", 0, 1).unwrap()).unwrap();
        assert!(p.contains("ck-scratch-") && !std::path::Path::new(p.trim()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn ck_out_files_are_collected_and_limited() {
        let d = OutDir::new().unwrap();
        let p = d.0.clone();
        run_cmd(b"sh -c 'echo hi > $CK_OUT/a.txt; echo yo > $CK_OUT/b_$CK_START.txt; mkdir $CK_OUT/sub; echo x > $CK_OUT/sub/c' x", 7, 1, 4, 9, 4, Some(&d.0)).unwrap();
        let f = d.files(1).unwrap();
        assert_eq!(f.iter().map(|x| (x.0.as_str(), x.2)).collect::<Vec<_>>(), vec![("a.txt", 3), ("b_4.txt", 3), ("sub/c", 2)], "nested files kept with their relative path");
        std::fs::write(d.0.join("big"), vec![0u8; 2 << 20]).unwrap();
        assert!(d.files(1).unwrap_err().contains("over the 1 MB"));
        std::fs::write(d.0.join(".hidden"), b"x").unwrap();
        std::fs::remove_file(d.0.join("big")).unwrap();
        assert!(d.files(1).unwrap_err().contains("not allowed"));
        drop(d);
        assert!(!p.exists(), "dir removed on drop");
    }
}
