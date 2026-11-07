//! Structured log: one JSON object per line in <CK_DATA|ck-data>/logs/<role>-YYYYMMDD.jsonl (UTC day). See docs/LOGGING.md.
//! Owner: g1. Callers never block: `emit` does one `try_send` into a bounded channel; a writer thread batches, writes
//! through a BufWriter, flushes every `log_flush_ms` and rotates by size (`log_max_mb`, `log_keep`). A full channel
//! counts a drop; the writer later logs one `log.dropped` line with the count. Human output stays on stdout (`say!`).
//! Privacy: fields whose name looks secret are replaced by "h:" + 8 hex of their SHA-256 before anything is written.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

impl Level {
    pub fn name(self) -> &'static str {
        ["off", "error", "warn", "info", "debug"][self as usize]
    }
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "error" => Level::Error,
            "warn" => Level::Warn,
            "info" => Level::Info,
            "debug" => Level::Debug,
            _ => return None,
        })
    }
}

const ROLES: [&str; 4] = ["head", "agent", "standby", "client"];
const CAP: usize = 16_384;

struct Rec {
    ts_ms: i64,
    level: Level,
    role: u8,
    kind: String,
    fields: Value,
}

enum Msg {
    Rec(Rec),
    Flush(SyncSender<()>),
}

struct Logger {
    tx: SyncSender<Msg>,
    dropped: AtomicU64,
    level: AtomicU8,   // 0 = off; refreshed from the tunables by the writer thread
    chunks: AtomicBool,
    role: AtomicU8,
}

fn cell() -> &'static OnceLock<Logger> {
    static L: OnceLock<Logger> = OnceLock::new();
    &L
}

fn lg() -> Option<&'static Logger> {
    cell().get()
}

pub fn logs_dir() -> PathBuf {
    PathBuf::from(std::env::var("CK_DATA").unwrap_or_else(|_| "ck-data".into())).join("logs")
}

fn role_idx(role: &str) -> u8 {
    ROLES.iter().position(|r| *r == role).unwrap_or(0) as u8
}

/// Start logging for this process (idempotent). Role: head | agent | standby | client.
pub fn init(role: &str, node: &str) {
    let (tx, rx) = sync_channel(CAP);
    let l = Logger { tx, dropped: AtomicU64::new(0), level: AtomicU8::new(0), chunks: AtomicBool::new(false), role: AtomicU8::new(role_idx(role)) };
    if cell().set(l).is_err() {
        return;
    }
    refresh();
    let node = node.to_string();
    std::thread::Builder::new().name("ck-log".into()).spawn(move || writer(rx, node, logs_dir())).ok();
}

/// A standby that takes over becomes the head.
pub fn set_role(role: &str) {
    if let Some(l) = lg() {
        l.role.store(role_idx(role), Relaxed);
    }
}

fn refresh() {
    if let Some(l) = lg() {
        l.level.store(crate::tune::n("log_level") as u8, Relaxed);
        l.chunks.store(crate::tune::n("log_chunks") != 0, Relaxed);
    }
}

#[inline]
pub fn enabled(level: Level) -> bool {
    lg().is_some_and(|l| level as u8 <= l.level.load(Relaxed))
}

/// Chunk-level lines (dispatch, result): debug, and only when log_chunks = 1.
#[inline]
pub fn chunks_on() -> bool {
    lg().is_some_and(|l| l.chunks.load(Relaxed) && l.level.load(Relaxed) > 0)
}

pub fn emit(level: Level, kind: &str, fields: Value) {
    let Some(l) = lg() else { return };
    let rec = Rec { ts_ms: crate::jobs::now_ms(), level, role: l.role.load(Relaxed), kind: kind.to_string(), fields };
    if let Err(TrySendError::Full(_)) = l.tx.try_send(Msg::Rec(rec)) {
        l.dropped.fetch_add(1, Relaxed);
    }
}

/// Log a plain message (used by `say!`).
pub fn msg(level: Level, kind: &str, m: String) {
    if enabled(level) {
        emit(level, kind, json!({ "msg": m }));
    }
}

/// Head events (also stored in SQLite): the single call site is Store::log_event.
pub fn event(kind: &str, actor: &str, detail: &str) {
    if enabled(Level::Info) {
        emit(Level::Info, kind, json!({ "actor": actor, "detail": detail }));
    }
}

/// Block until everything queued so far is on disk (shutdown, tests).
pub fn flush() {
    let Some(l) = lg() else { return };
    let (tx, rx) = sync_channel(1);
    if l.tx.send(Msg::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

/// Print for the operator AND log: `say!("kind", "text {x}")`. Info level.
#[macro_export]
macro_rules! say {
    ($kind:expr, $($arg:tt)+) => {{ let m = format!($($arg)+); println!("{m}"); $crate::log::msg($crate::log::Level::Info, $kind, m); }};
}
/// Same on stderr, warn level: `warnl!("kind", "text {x}")`.
#[macro_export]
macro_rules! warnl {
    ($kind:expr, $($arg:tt)+) => {{ let m = format!($($arg)+); eprintln!("{m}"); $crate::log::msg($crate::log::Level::Warn, $kind, m); }};
}
/// Structured line, built only when the level is on: `lg!(Info, "job.done", {"job": id})`.
#[macro_export]
macro_rules! lg {
    ($lvl:ident, $kind:expr, $json:tt) => { if $crate::log::enabled($crate::log::Level::$lvl) { $crate::log::emit($crate::log::Level::$lvl, $kind, serde_json::json!($json)) } };
}
/// Chunk-level line (debug, needs log_chunks=1): `lgc!("chunk.done", {"job": id})`.
#[macro_export]
macro_rules! lgc {
    ($kind:expr, $json:tt) => { if $crate::log::chunks_on() { $crate::log::emit($crate::log::Level::Debug, $kind, serde_json::json!($json)) } };
}

// ---------- privacy ----------

fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["token", "password", "passwd", "secret", "key", "join_code", "joincode", "cookie", "bearer", "auth", "hmac", "mac_key"].iter().any(|s| k.contains(s))
}

fn hash8(v: &Value) -> String {
    let t = v.as_str().map_or_else(|| v.to_string(), str::to_string);
    let d = Sha256::digest(t.as_bytes());
    format!("h:{}", d[..4].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if secret_key(k) {
                    *x = Value::String(hash8(x));
                } else {
                    scrub(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}

// ---------- line format ----------

fn line(ts_ms: i64, level: Level, role: &str, node: &str, kind: &str, mut fields: Value) -> String {
    scrub(&mut fields);
    let mut s = format!("{{\"ts_ms\":{ts_ms},\"level\":\"{}\",\"role\":\"{role}\",\"node\":{},\"kind\":{}", level.name(), json!(node), json!(kind));
    if let Value::Object(m) = &fields {
        for (k, x) in m {
            if !["ts_ms", "level", "role", "node", "kind"].contains(&k.as_str()) {
                s.push_str(&format!(",{}:{}", json!(k), x));
            }
        }
    }
    s.push('}');
    s
}

/// UTC YYYYMMDD of a unix-ms time (days-from-civil, no date crate).
fn day_of(ts_ms: i64) -> String {
    let z = ts_ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}{:02}{:02}", yoe + era * 400 + (m <= 2) as i64, m, d)
}

// ---------- files ----------

pub struct Sink {
    dir: PathBuf,
    role: String,
    max: u64,
    keep: usize,
    day: String,
    out: Option<BufWriter<File>>,
    size: u64,
}

impl Sink {
    pub fn new(dir: &Path, role: &str, max: u64, keep: usize) -> Sink {
        Sink { dir: dir.into(), role: role.into(), max, keep: keep.max(1), day: String::new(), out: None, size: 0 }
    }
    fn path(&self) -> PathBuf {
        self.dir.join(format!("{}-{}.jsonl", self.role, self.day))
    }
    pub fn set_limits(&mut self, max: u64, keep: usize) {
        self.max = max;
        self.keep = keep.max(1);
    }
    fn open(&mut self) {
        let _ = fs::create_dir_all(&self.dir);
        self.size = fs::metadata(self.path()).map_or(0, |m| m.len());
        self.out = OpenOptions::new().create(true).append(true).open(self.path()).ok().map(BufWriter::new);
        self.prune();
    }
    pub fn write(&mut self, ts_ms: i64, text: &str) {
        let day = day_of(ts_ms);
        if self.out.is_none() || day != self.day {
            self.flush();
            self.day = day;
            self.open();
        } else if self.size >= self.max {
            self.flush();
            let to = self.dir.join(format!("{}-{}.{}.jsonl", self.role, self.day, ts_ms));
            let _ = fs::rename(self.path(), to);
            self.open();
        }
        if let Some(o) = &mut self.out {
            if o.write_all(text.as_bytes()).and_then(|_| o.write_all(b"\n")).is_ok() {
                self.size += text.len() as u64 + 1;
            }
        }
    }
    pub fn flush(&mut self) {
        if let Some(o) = &mut self.out {
            let _ = o.flush();
        }
    }
    /// Delete the oldest files of this role beyond `keep` (the current one counts).
    fn prune(&self) {
        let mut v = files(&self.dir, Some(&self.role));
        if v.len() > self.keep {
            v.sort_by_key(|f| f.1);
            for f in &v[..v.len() - self.keep] {
                let _ = fs::remove_file(&f.0);
            }
        }
    }
}

/// (path, mtime) of log files, optionally of one role.
fn files(dir: &Path, role: Option<&str>) -> Vec<(PathBuf, std::time::SystemTime)> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(".jsonl") && role.map_or(true, |r| n.starts_with(&format!("{r}-")))
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect()
}

fn writer(rx: Receiver<Msg>, node: String, dir: PathBuf) {
    let mut sinks: Vec<Option<Sink>> = (0..ROLES.len()).map(|_| None).collect();
    let mut last = Instant::now();
    let mut flush_every = Duration::from_millis(200);
    let (mut max, mut keep) = (64u64 << 20, 14usize);
    let l = lg().expect("logger set before the writer starts");
    loop {
        let mut flush_to: Vec<SyncSender<()>> = vec![];
        let mut n = 0;
        let mut next = rx.recv_timeout(flush_every);
        loop {
            match next {
                Ok(Msg::Rec(r)) => {
                    let text = line(r.ts_ms, r.level, ROLES[r.role as usize], &node, &r.kind, r.fields);
                    let s = sinks[r.role as usize].get_or_insert_with(|| Sink::new(&dir, ROLES[r.role as usize], max, keep));
                    s.write(r.ts_ms, &text);
                }
                Ok(Msg::Flush(a)) => flush_to.push(a),
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
            n += 1;
            if n >= 4096 {
                break;
            }
            next = rx.try_recv().map_err(|_| RecvTimeoutError::Timeout);
        }
        let dropped = l.dropped.swap(0, Relaxed);
        if dropped > 0 {
            let r = l.role.load(Relaxed);
            let text = line(crate::jobs::now_ms(), Level::Warn, ROLES[r as usize], &node, "log.dropped", json!({ "count": dropped }));
            sinks[r as usize].get_or_insert_with(|| Sink::new(&dir, ROLES[r as usize], max, keep)).write(crate::jobs::now_ms(), &text);
        }
        if last.elapsed() >= flush_every || !flush_to.is_empty() {
            sinks.iter_mut().flatten().for_each(Sink::flush);
            last = Instant::now();
            refresh();
            flush_every = Duration::from_millis(crate::tune::n("log_flush_ms").max(10));
            (max, keep) = (crate::tune::n("log_max_mb") << 20, crate::tune::n("log_keep") as usize);
            sinks.iter_mut().flatten().for_each(|s| s.set_limits(max, keep));
        }
        flush_to.into_iter().for_each(|a| { let _ = a.send(()); });
    }
}

// ---------- reading ----------

pub struct Query {
    pub role: Option<String>,
    pub since_ms: i64,
    pub kind: Option<String>,
    pub level: Level,
    pub limit: usize,
}

fn matches(q: &Query, v: &Value) -> bool {
    v["ts_ms"].as_i64().unwrap_or(0) >= q.since_ms
        && q.kind.as_deref().map_or(true, |k| v["kind"].as_str().unwrap_or("").starts_with(k))
        && Level::parse(v["level"].as_str().unwrap_or("")).map_or(false, |l| l <= q.level)
}

/// The newest `limit` matching lines (oldest first), read from the files in `dir`.
pub fn query(dir: &Path, q: &Query) -> Vec<String> {
    let mut fs = files(dir, q.role.as_deref());
    fs.sort_by_key(|f| f.1);
    let mut out: VecDeque<String> = VecDeque::new();
    for (p, mtime) in fs {
        let old = mtime.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64) < q.since_ms;
        let Ok(f) = File::open(&p) else { continue };
        if old {
            continue; // nothing in it is newer than since
        }
        for l in BufReader::new(f).lines().map_while(Result::ok) {
            if serde_json::from_str::<Value>(&l).map_or(false, |v| matches(q, &v)) {
                out.push_back(l);
                if out.len() > q.limit {
                    out.pop_front();
                }
            }
        }
    }
    out.into()
}

/// `ck logs [--role R] [--since 10m] [--kind PREFIX] [--level L] [--limit N] [--follow]`, from the local files.
pub fn cli() {
    let env = |k: &str| std::env::var(k).ok();
    let since = env("CK_LOGS_SINCE").map_or(0, |s| parse_since(&s).unwrap_or_else(|| crate::die("--since needs a number and s|m|h|d, e.g. 10m")));
    let mut q = Query {
        role: env("CK_LOGS_ROLE"),
        since_ms: if since > 0 { crate::jobs::now_ms() - since } else { 0 },
        kind: env("CK_LOGS_KIND"),
        level: env("CK_LOGS_LEVEL").map_or(Level::Debug, |l| Level::parse(&l).unwrap_or_else(|| crate::die("--level is error|warn|info|debug"))),
        limit: env("CK_LOGS_LIMIT").and_then(|s| s.parse().ok()).unwrap_or(1000),
    };
    let dir = logs_dir();
    let mut seen_ts = 0i64;
    for l in query(&dir, &q) {
        seen_ts = seen_ts.max(serde_json::from_str::<Value>(&l).map_or(0, |v| v["ts_ms"].as_i64().unwrap_or(0)));
        println!("{l}");
    }
    if env("CK_LOGS_FOLLOW").is_some() {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            q.since_ms = seen_ts + 1; // ponytail: lines sharing one millisecond with the last printed one may be skipped
            for l in query(&dir, &q) {
                seen_ts = seen_ts.max(serde_json::from_str::<Value>(&l).map_or(0, |v| v["ts_ms"].as_i64().unwrap_or(0)));
                println!("{l}");
            }
        }
    }
}

pub fn parse_since(s: &str) -> Option<i64> {
    let (n, u) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok()?;
    Some(n * match u { "s" => 1000, "m" => 60_000, "h" => 3_600_000, "d" => 86_400_000, _ => return None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-log-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn line_format_and_day() {
        let l = line(1_762_300_800_123, Level::Info, "head", "n1", "job.submitted", json!({"job": 7, "user": "ann"}));
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!((v["ts_ms"].as_i64(), v["level"].as_str(), v["role"].as_str(), v["node"].as_str(), v["kind"].as_str(), v["job"].as_i64()),
                   (Some(1_762_300_800_123), Some("info"), Some("head"), Some("n1"), Some("job.submitted"), Some(7)));
        assert!(l.starts_with("{\"ts_ms\":"), "fixed key order: {l}");
        assert_eq!(day_of(1_762_300_800_123), "20251105");
        assert_eq!(day_of(0), "19700101");
    }

    #[test]
    fn secrets_are_hashed() {
        let l = line(1, Level::Info, "agent", "n", "x", json!({"token": "ckt_SECRET", "node_key": "KEYDATA", "join_code": "ABCD1234", "nest": {"password": "pw123456"}, "ok": "fine"}));
        for s in ["ckt_SECRET", "KEYDATA", "ABCD1234", "pw123456"] {
            assert!(!l.contains(s), "{s} leaked: {l}");
        }
        assert!(l.contains("\"ok\":\"fine\"") && l.contains("h:"));
    }

    #[test]
    fn rotation_keeps_newest_files() {
        let d = tmp("rot");
        let mut s = Sink::new(&d, "head", 200, 3);
        for i in 0..40 {
            let ts = 1_762_300_800_000 + i * 10;
            s.write(ts, &line(ts, Level::Info, "head", "n", "t", json!({"i": i, "pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"})));
            std::thread::sleep(Duration::from_millis(2)); // distinct mtimes
        }
        s.flush();
        let n = files(&d, Some("head")).len();
        assert_eq!(n, 3, "keep=3 files");
        let all = query(&d, &Query { role: Some("head".into()), since_ms: 0, kind: None, level: Level::Debug, limit: 1000 });
        assert!(all.last().unwrap().contains("\"i\":39"), "newest line survives");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn query_filters() {
        let d = tmp("q");
        let mut s = Sink::new(&d, "head", 1 << 20, 5);
        let now = crate::jobs::now_ms();
        s.write(now, &line(now, Level::Info, "head", "n", "node.lost", json!({})));
        s.write(now, &line(now, Level::Debug, "head", "n", "chunk.done", json!({})));
        s.write(now, &line(now, Level::Info, "head", "n", "job.done", json!({})));
        s.flush();
        let q = |kind: Option<&str>, level| query(&d, &Query { role: Some("head".into()), since_ms: now - 1000, kind: kind.map(Into::into), level, limit: 10 });
        assert_eq!(q(None, Level::Debug).len(), 3);
        assert_eq!(q(None, Level::Info).len(), 2);
        assert_eq!(q(Some("node."), Level::Debug).len(), 1);
        assert_eq!(query(&d, &Query { role: None, since_ms: now - 1000, kind: None, level: Level::Debug, limit: 2 }).len(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn full_channel_counts_drops_and_never_blocks() {
        // a logger with no writer: the channel fills, emit must not block and must count
        let (tx, _rx) = sync_channel(4);
        let l = Logger { tx, dropped: AtomicU64::new(0), level: AtomicU8::new(4), chunks: AtomicBool::new(true), role: AtomicU8::new(0) };
        let t = Instant::now();
        for i in 0..100 {
            let rec = Rec { ts_ms: i, level: Level::Info, role: 0, kind: "x".into(), fields: json!({}) };
            if let Err(TrySendError::Full(_)) = l.tx.try_send(Msg::Rec(rec)) {
                l.dropped.fetch_add(1, Relaxed);
            }
        }
        assert_eq!(l.dropped.load(Relaxed), 96);
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn since_parse() {
        assert_eq!(parse_since("10m"), Some(600_000));
        assert_eq!(parse_since("2h"), Some(7_200_000));
        assert_eq!(parse_since("x"), None);
    }
}
