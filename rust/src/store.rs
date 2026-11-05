//! Head state in one SQLite file (WAL), numbered migrations (D4, HA-02). Owner: a1.
//! Contract: keep every pub signature. Errors are plain strings (the API maps them).
//! Parts are idempotent: adding the same (job, start, end) twice keeps one row and returns false.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::jobs::{CapRow, EventRow, JobRow, JobSpec, JobState, LabRow, NodeRow, PoolRow, ReservationRow, ReserveRow, UserRow, WindowRow};

pub struct Store {
    conn: Mutex<rusqlite::Connection>,
}

/// `CK_DATA` dir (default `./ck-data`) + `head.db`.
pub fn default_path() -> PathBuf {
    PathBuf::from(std::env::var("CK_DATA").unwrap_or_else(|_| "ck-data".into())).join("head.db")
}

/// Retention (wave 14): delete old events and, if `parts_days` > 0, parts of long-finished jobs, in batches of 2,000
/// rows on its own connection so dispatch never waits long. Returns (events, parts) deleted.
pub fn prune(path: &Path, events_days: u64, parts_days: u64, now_ms: i64) -> Result<(u64, u64), String> {
    let c = Connection::open(path).map_err(e)?;
    c.busy_timeout(std::time::Duration::from_millis(5000)).map_err(e)?;
    let day = 86_400_000i64;
    let batch = |sql: &str, cut: i64| -> Result<u64, String> {
        let mut total = 0u64;
        loop {
            let n = c.execute(sql, [cut]).map_err(e)? as u64;
            total += n;
            if n < 2000 { return Ok(total); }
            std::thread::sleep(std::time::Duration::from_millis(50)); // let the head write between batches
        }
    };
    let ev = batch("DELETE FROM events WHERE id IN (SELECT id FROM events WHERE ts_ms<? ORDER BY id LIMIT 2000)", now_ms - events_days as i64 * day)?;
    let parts = if parts_days == 0 { 0 } else {
        batch("DELETE FROM parts WHERE rowid IN (SELECT parts.rowid FROM parts JOIN jobs ON jobs.id=parts.job_id WHERE jobs.finished_ms IS NOT NULL AND jobs.finished_ms<? LIMIT 2000)", now_ms - parts_days as i64 * day)?
    };
    Ok((ev, parts))
}

/// A consistent copy of the database (VACUUM INTO), safe while the head runs. `dest` must not exist.
pub fn backup(path: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() { return Err(format!("{} already exists", dest.display())); }
    if let Some(d) = dest.parent().filter(|d| !d.as_os_str().is_empty()) { std::fs::create_dir_all(d).map_err(|x| format!("create {}: {x}", d.display()))?; }
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(e)?;
    c.busy_timeout(std::time::Duration::from_millis(30_000)).map_err(e)?;
    let tmp = dest.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    c.execute("VACUUM INTO ?", [tmp.to_string_lossy().as_ref()]).map_err(e)?;
    std::fs::rename(&tmp, dest).map_err(|x| x.to_string())
}

/// Scheduled backup: when the newest `head-*.db` in `dir` is older than `every_h`, write a new one, then keep `keep` files.
pub fn scheduled_backup(path: &Path, dir: &Path, every_h: u64, keep: usize, now_s: u64) -> Result<Option<PathBuf>, String> {
    let mut files: Vec<(u64, PathBuf)> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|f| {
        let n = f.file_name().to_string_lossy().into_owned();
        Some((n.strip_prefix("head-")?.strip_suffix(".db")?.parse().ok()?, f.path()))
    }).collect();
    files.sort();
    if files.last().is_some_and(|(t, _)| now_s < t + every_h * 3600) { return Ok(None); }
    let dest = dir.join(format!("head-{now_s}.db"));
    backup(path, &dest)?;
    files.push((now_s, dest.clone()));
    for (_, f) in files.iter().take(files.len().saturating_sub(keep.max(1))) { let _ = std::fs::remove_file(f); }
    Ok(Some(dest))
}

/// Background thread of a running head: retention every 10 minutes, backups when `backup_every_h` > 0.
pub fn maintenance_thread() {
    std::thread::spawn(|| {
        let path = default_path();
        let mut last_prune = std::time::Instant::now() - std::time::Duration::from_secs(3600);
        std::thread::sleep(std::time::Duration::from_secs(30)); // after the head's own startup and migrations
        loop {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
            if last_prune.elapsed().as_secs() >= 600 {
                last_prune = std::time::Instant::now();
                match prune(&path, crate::tune::n("events_keep_days"), crate::tune::n("parts_keep_days"), now) {
                    Ok((0, 0)) => {}
                    Ok((ev, parts)) => crate::say!("store.prune", "retention: deleted {ev} events, {parts} parts"),
                    Err(x) => crate::say!("store.prune", "retention failed: {x}"),
                }
            }
            let h = crate::tune::n("backup_every_h");
            if h > 0 {
                let dir = path.parent().map(|d| d.join("backups")).unwrap_or_else(|| PathBuf::from("backups"));
                match scheduled_backup(&path, &dir, h, crate::tune::n("backup_keep") as usize, (now / 1000) as u64) {
                    Ok(Some(f)) => crate::say!("store.backup", "backup written: {}", f.display()),
                    Ok(None) => {}
                    Err(x) => crate::say!("store.backup", "backup failed: {x}"),
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
}

/// Append-only: entry N moves user_version from N to N+1. Never edit an applied entry.
const MIGRATIONS: &[&str] = &["
CREATE TABLE jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, kind INTEGER NOT NULL, from_n INTEGER NOT NULL,
  end_n INTEGER NOT NULL, chunk INTEGER NOT NULL, arg BLOB NOT NULL, only_json TEXT NOT NULL, submitter TEXT NOT NULL,
  state TEXT NOT NULL, created_ms INTEGER NOT NULL, started_ms INTEGER, finished_ms INTEGER, error TEXT);
CREATE INDEX jobs_state ON jobs(state);
CREATE TABLE parts (job_id INTEGER NOT NULL, start INTEGER NOT NULL, \"end\" INTEGER NOT NULL, body BLOB NOT NULL,
  PRIMARY KEY (job_id, start, \"end\"));
CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, kind TEXT NOT NULL, actor TEXT NOT NULL, detail TEXT NOT NULL);
CREATE TABLE nodes (node_id TEXT PRIMARY KEY, host TEXT NOT NULL, addr TEXT NOT NULL, inventory TEXT NOT NULL,
  first_seen_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL);
CREATE TABLE users (name TEXT PRIMARY KEY, role TEXT NOT NULL, pw_hash TEXT NOT NULL, created_ms INTEGER NOT NULL);
CREATE TABLE tokens (sha256 TEXT PRIMARY KEY, user TEXT NOT NULL REFERENCES users(name), created_ms INTEGER NOT NULL);
", "
CREATE TABLE labs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, w INTEGER NOT NULL, h INTEGER NOT NULL, created_ms INTEGER NOT NULL);
CREATE TABLE join_codes (sha256 TEXT PRIMARY KEY, uses_left INTEGER NOT NULL, expires_ms INTEGER NOT NULL, auto_approve INTEGER NOT NULL,
  created_by TEXT NOT NULL, created_ms INTEGER NOT NULL);
ALTER TABLE nodes ADD COLUMN state TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE nodes ADD COLUMN state_reason TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN state_by TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN lab_id INTEGER REFERENCES labs(id) ON DELETE SET NULL;
ALTER TABLE nodes ADD COLUMN x INTEGER;
ALTER TABLE nodes ADD COLUMN y INTEGER;
ALTER TABLE nodes ADD COLUMN key BLOB;
UPDATE nodes SET state='active';
CREATE UNIQUE INDEX nodes_seat ON nodes(lab_id, x, y) WHERE lab_id IS NOT NULL;
", "
ALTER TABLE jobs ADD COLUMN selector TEXT NOT NULL DEFAULT '';
ALTER TABLE jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 2;
CREATE TABLE labels (node_id TEXT NOT NULL REFERENCES nodes(node_id) ON DELETE CASCADE, key TEXT NOT NULL, value TEXT NOT NULL,
  PRIMARY KEY (node_id, key));
CREATE TABLE pools (name TEXT PRIMARY KEY, note TEXT NOT NULL);
CREATE TABLE pool_members (pool TEXT NOT NULL REFERENCES pools(name) ON DELETE CASCADE,
  node_id TEXT NOT NULL REFERENCES nodes(node_id) ON DELETE CASCADE, PRIMARY KEY (pool, node_id));
CREATE TABLE caps (id INTEGER PRIMARY KEY AUTOINCREMENT, selector TEXT NOT NULL, max_threads INTEGER, note TEXT NOT NULL);
CREATE TABLE reserves (id INTEGER PRIMARY KEY AUTOINCREMENT, selector TEXT NOT NULL, keep_n INTEGER NOT NULL, keep_pct INTEGER NOT NULL, note TEXT NOT NULL);
", "
ALTER TABLE jobs ADD COLUMN policy TEXT NOT NULL DEFAULT '';
ALTER TABLE jobs ADD COLUMN reducer TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN calib_mhs REAL;
CREATE TABLE windows (id INTEGER PRIMARY KEY AUTOINCREMENT, selector TEXT NOT NULL, days TEXT NOT NULL, start TEXT NOT NULL,
  \"end\" TEXT NOT NULL, action TEXT NOT NULL, cap_threads INTEGER, note TEXT NOT NULL);
CREATE TABLE reservations (id INTEGER PRIMARY KEY AUTOINCREMENT, selector TEXT NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, note TEXT NOT NULL);
CREATE TABLE items (job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, idx INTEGER NOT NULL, item TEXT NOT NULL, PRIMARY KEY (job_id, idx));
", "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
", "
ALTER TABLE jobs ADD COLUMN max_pcs INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN placement TEXT NOT NULL DEFAULT '';
ALTER TABLE jobs ADD COLUMN mem_mb INTEGER NOT NULL DEFAULT 0;
", "
CREATE TABLE alerts (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, severity TEXT NOT NULL, node_id TEXT, detail TEXT NOT NULL,
  since_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL, active INTEGER NOT NULL DEFAULT 1, acked_by TEXT);
CREATE UNIQUE INDEX alerts_active ON alerts(kind, IFNULL(node_id, '')) WHERE active = 1;
CREATE TABLE experiments (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, submitter TEXT NOT NULL, created_ms INTEGER NOT NULL,
  configs INTEGER NOT NULL, repeats INTEGER NOT NULL, virtual INTEGER NOT NULL, commit_hash TEXT NOT NULL, pcs_max INTEGER NOT NULL,
  raw_csv TEXT NOT NULL, runs_jsonl TEXT NOT NULL, sim TEXT);
CREATE TABLE samples (ts_ms INTEGER PRIMARY KEY, online INTEGER NOT NULL, busy INTEGER NOT NULL, rate REAL NOT NULL, energy_j REAL NOT NULL);
CREATE TABLE job_stats (job_id INTEGER PRIMARY KEY, body TEXT NOT NULL);
", "
CREATE TABLE blobs (sha TEXT PRIMARY KEY, size INTEGER NOT NULL, created_ms INTEGER NOT NULL, last_used_ms INTEGER NOT NULL);
ALTER TABLE jobs ADD COLUMN blobs TEXT NOT NULL DEFAULT '[]';
ALTER TABLE jobs ADD COLUMN blob_mode TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN net_mbps REAL;
ALTER TABLE nodes ADD COLUMN rtt_ms REAL;
ALTER TABLE nodes ADD COLUMN probed_ms INTEGER;
"];

const NODE_SEL: &str = "SELECT node_id,host,addr,inventory,first_seen_ms,last_seen_ms,state,state_reason,state_by,lab_id,x,y,calib_mhs,net_mbps,rtt_ms,probed_ms FROM nodes";

fn node_row(r: &Row) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        node_id: r.get(0)?,
        host: r.get(1)?,
        addr: r.get(2)?,
        inventory: r.get(3)?,
        first_seen_ms: r.get(4)?,
        last_seen_ms: r.get(5)?,
        state: r.get(6)?,
        state_reason: r.get(7)?,
        state_by: r.get(8)?,
        lab_id: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        x: r.get::<_, Option<i64>>(10)?.map(|v| v as u32),
        y: r.get::<_, Option<i64>>(11)?.map(|v| v as u32),
        calib_mhs: r.get(12)?,
        net_mbps: r.get(13)?,
        rtt_ms: r.get(14)?,
        probed_ms: r.get(15)?,
    })
}

fn lab_row(r: &Row) -> rusqlite::Result<LabRow> {
    Ok(LabRow { id: r.get::<_, i64>(0)? as u64, name: r.get(1)?, w: r.get::<_, i64>(2)? as u32, h: r.get::<_, i64>(3)? as u32, created_ms: r.get(4)? })
}

fn check_lab(name: &str, w: u32, h: u32) -> Result<(), String> {
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err("lab name must be 1 to 80 characters".into());
    }
    if !(1..=64).contains(&w) || !(1..=64).contains(&h) {
        return Err("lab width and height must be 1 to 64".into());
    }
    Ok(())
}

const ROLES: [&str; 4] = ["viewer", "researcher", "operator", "admin"];
const JOB_SEL: &str = "SELECT id,name,kind,from_n,end_n,chunk,state,submitter,created_ms,started_ms,finished_ms,error,
  (SELECT COALESCE(SUM(\"end\"-start),0) FROM parts WHERE job_id=jobs.id),selector,priority,policy,reducer FROM jobs";

fn e(x: rusqlite::Error) -> String {
    x.to_string()
}

fn job_row(r: &Row) -> rusqlite::Result<JobRow> {
    let (from, end): (i64, i64) = (r.get(3)?, r.get(4)?);
    let st: String = r.get(6)?;
    Ok(JobRow {
        id: r.get::<_, i64>(0)? as u64,
        name: r.get(1)?,
        kind: crate::jobs::kind_name(r.get::<_, i64>(2)? as u8).to_string(),
        from: from as u64,
        end: end as u64,
        chunk: r.get::<_, i64>(5)? as u64,
        total: (end - from).max(0) as u64,
        done: r.get::<_, i64>(12)? as u64,
        state: JobState::parse(&st).unwrap_or(JobState::Failed),
        submitter: r.get(7)?,
        created_ms: r.get(8)?,
        started_ms: r.get(9)?,
        finished_ms: r.get(10)?,
        error: r.get(11)?,
        selector: r.get(13)?,
        priority: r.get::<_, i64>(14)?.clamp(0, 255) as u8,
        policy: r.get(15)?,
        reducer: r.get(16)?,
        eta_ms: None,
    })
}

fn user_row(r: &Row) -> rusqlite::Result<UserRow> {
    Ok(UserRow { name: r.get(0)?, role: r.get(1)?, pw_hash: r.get(2)?, created_ms: r.get(3)? })
}

fn migrate(c: &mut Connection) -> Result<(), String> {
    let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).map_err(e)? as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v) {
        let tx = c.transaction().map_err(e)?;
        tx.execute_batch(sql).map_err(e)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1)).map_err(e)?;
        tx.commit().map_err(e)?;
    }
    Ok(())
}

fn setup(mut c: Connection) -> Result<Store, String> {
    c.busy_timeout(std::time::Duration::from_millis(5000)).map_err(e)?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;").map_err(e)?;
    migrate(&mut c)?;
    Ok(Store { conn: Mutex::new(c) })
}

impl Store {
    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Create the directory if needed, open, set WAL + synchronous=NORMAL + foreign keys, run migrations.
    pub fn open(path: &Path) -> Result<Store, String> {
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|x| format!("create {}: {x}", d.display()))?;
        }
        setup(Connection::open(path).map_err(e)?)
    }
    /// Same schema in memory, for tests.
    pub fn memory() -> Store {
        setup(Connection::open_in_memory().expect("in-memory sqlite")).expect("migrations")
    }

    // ---- jobs
    pub fn create_job(&self, spec: &JobSpec, now_ms: i64) -> Result<JobRow, String> {
        let only = serde_json::to_string(&spec.only).map_err(|x| x.to_string())?;
        let c = self.db();
        c.execute(
            "INSERT INTO jobs (name,kind,from_n,end_n,chunk,arg,only_json,submitter,selector,priority,policy,reducer,max_pcs,placement,mem_mb,state,created_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'queued',?)",
            params![spec.name, spec.kind as i64, spec.from as i64, spec.end as i64, spec.chunk as i64, spec.arg, only, spec.submitter, spec.selector, spec.priority as i64, spec.policy, spec.reducer, spec.max_pcs as i64, spec.placement, spec.mem_mb as i64, now_ms],
        )
        .map_err(e)?;
        let id = c.last_insert_rowid();
        drop(c);
        self.set_job_blobs(id as u64, spec);
        if !spec.items.is_empty() {
            if let Err(x) = self.put_items(id as u64, &spec.items) {
                let _ = self.db().execute("DELETE FROM jobs WHERE id=?", [id]);
                return Err(x);
            }
        }
        let c = self.db();
        c.query_row(&format!("{JOB_SEL} WHERE id=?"), [id], job_row).map_err(e)
    }
    /// Sets started_ms when entering Running, finished_ms when entering a finished state.
    pub fn set_state(&self, id: u64, state: JobState, now_ms: i64, error: Option<&str>) -> Result<JobRow, String> {
        let c = self.db();
        let n = c
            .execute(
                "UPDATE jobs SET state=?1, error=?2,
                   started_ms = CASE WHEN ?1='running' THEN COALESCE(started_ms, ?3) ELSE started_ms END,
                   finished_ms = CASE WHEN ?4 THEN ?3 ELSE finished_ms END WHERE id=?5",
                params![state.as_str(), error, now_ms, state.finished(), id as i64],
            )
            .map_err(e)?;
        if n == 0 {
            return Err(format!("no job {id}"));
        }
        c.query_row(&format!("{JOB_SEL} WHERE id=?"), [id as i64], job_row).map_err(e)
    }
    /// The spec a job was created with (arg and only included), for resuming after a restart.
    pub fn spec(&self, id: u64) -> Option<JobSpec> {
        self.db()
            .query_row("SELECT name,kind,from_n,end_n,chunk,arg,only_json,submitter,selector,priority,policy,reducer,max_pcs,placement,mem_mb,blobs,blob_mode FROM jobs WHERE id=?", [id as i64], |r| {
                let only: String = r.get(6)?;
                let blobs: String = r.get(15)?;
                Ok(JobSpec {
                    name: r.get(0)?,
                    kind: r.get::<_, i64>(1)? as u8,
                    from: r.get::<_, i64>(2)? as u64,
                    end: r.get::<_, i64>(3)? as u64,
                    chunk: r.get::<_, i64>(4)? as u64,
                    arg: r.get(5)?,
                    only: serde_json::from_str(&only).unwrap_or_default(),
                    submitter: r.get(7)?,
                    selector: r.get(8)?,
                    priority: r.get::<_, i64>(9)?.clamp(0, 255) as u8,
                    policy: r.get(10)?,
                    reducer: r.get(11)?,
                    max_pcs: r.get::<_, i64>(12)?.clamp(0, u32::MAX as i64) as u32,
                    placement: r.get(13)?,
                    mem_mb: r.get::<_, i64>(14)?.max(0) as u64,
                    blobs: serde_json::from_str(&blobs).unwrap_or_default(),
                    blob_mode: r.get(16)?,
                    ..Default::default() // items via items()
                })
            })
            .optional()
            .ok()
            .flatten()
    }
    /// (max_pcs, placement, mem_mb) of a job, for the job JSON.
    pub fn job_limits(&self, id: u64) -> (u32, String, u64) {
        self.spec(id).map(|s| (s.max_pcs, s.placement, s.mem_mb)).unwrap_or_default()
    }
    pub fn job(&self, id: u64) -> Option<JobRow> {
        self.db().query_row(&format!("{JOB_SEL} WHERE id=?"), [id as i64], job_row).optional().ok().flatten()
    }
    /// Newest first; `before` = only ids below it.
    pub fn jobs(&self, limit: usize, before: Option<u64>) -> Vec<JobRow> {
        let c = self.db();
        let q = format!("{JOB_SEL} WHERE id < ? ORDER BY id DESC LIMIT ?");
        let Ok(mut st) = c.prepare(&q) else { return vec![] };
        let b = before.map_or(i64::MAX, |b| b.min(i64::MAX as u64) as i64);
        st.query_map(params![b, limit.min(500) as i64], job_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    /// Queued and running jobs, oldest first.
    pub fn unfinished(&self) -> Vec<JobRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare(&format!("{JOB_SEL} WHERE state IN ('queued','running') ORDER BY id")) else { return vec![] };
        st.query_map([], job_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }

    // ---- results
    pub fn add_part(&self, job: u64, start: u64, end: u64, body: &[u8]) -> Result<bool, String> {
        let n = self
            .db()
            .execute("INSERT OR IGNORE INTO parts (job_id,start,\"end\",body) VALUES (?,?,?,?)", params![job as i64, start as i64, end as i64, body])
            .map_err(e)?;
        Ok(n == 1)
    }
    /// Ordered by start.
    pub fn parts(&self, job: u64) -> Vec<(u64, u64, Vec<u8>)> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT start,\"end\",body FROM parts WHERE job_id=? ORDER BY start,\"end\"") else { return vec![] };
        st.query_map([job as i64], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64, r.get(2)?)))
            .map(|m| m.flatten().collect())
            .unwrap_or_default()
    }
    /// How many parts a job has stored (a COUNT: GET /jobs/ID used to read every part body just to take `.len()`).
    pub fn part_count(&self, job: u64) -> u64 {
        self.db().query_row("SELECT COUNT(*) FROM parts WHERE job_id=?", [job as i64], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    }
    /// Completed ranges of a job, merged and ordered ([1,5) + [5,9) = [1,9)).
    pub fn done_ranges(&self, job: u64) -> Vec<(u64, u64)> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT start,\"end\" FROM parts WHERE job_id=? ORDER BY start,\"end\"") else { return vec![] };
        let rows = st.query_map([job as i64], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)));
        let mut out: Vec<(u64, u64)> = vec![];
        for (s, en) in rows.map(|m| m.flatten().collect::<Vec<_>>()).unwrap_or_default() {
            match out.last_mut() {
                Some(l) if s <= l.1 => l.1 = l.1.max(en),
                _ => out.push((s, en)),
            }
        }
        out
    }

    /// Like done_ranges but only the part that touches [a, b): the primary key answers in O(log parts) instead of
    /// reading every part of the job (a result used to cost O(parts), so a 30,000-part job slowed down 2x).
    /// Relies on stored parts being disjoint (the head drops overlapping results).
    pub fn done_in(&self, job: u64, a: u64, b: u64) -> Vec<(u64, u64)> {
        let c = self.db();
        let row = |r: &rusqlite::Row| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64));
        let mut out: Vec<(u64, u64)> = vec![];
        if let Ok(mut st) = c.prepare_cached("SELECT start,\"end\" FROM parts WHERE job_id=? AND start<=? ORDER BY start DESC LIMIT 1") {
            if let Ok(Some(p)) = st.query_row(params![job as i64, a as i64], row).map(Some).or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) }) {
                if p.1 > a {
                    out.push(p);
                }
            }
        }
        if let Ok(mut st) = c.prepare_cached("SELECT start,\"end\" FROM parts WHERE job_id=? AND start>? AND start<? ORDER BY start") {
            if let Ok(rows) = st.query_map(params![job as i64, a as i64, b as i64], row) {
                for (s, en) in rows.flatten() {
                    match out.last_mut() {
                        Some(l) if s <= l.1 => l.1 = l.1.max(en),
                        _ => out.push((s, en)),
                    }
                }
            }
        }
        out
    }

    // ---- events / audit (OBS-04)
    pub fn log_event(&self, ts_ms: i64, kind: &str, actor: &str, detail: &str) -> Result<EventRow, String> {
        crate::log::event(kind, actor, detail); // the one place head events reach the log
        let c = self.db();
        c.execute("INSERT INTO events (ts_ms,kind,actor,detail) VALUES (?,?,?,?)", params![ts_ms, kind, actor, detail]).map_err(e)?;
        Ok(EventRow { id: c.last_insert_rowid() as u64, ts_ms, kind: kind.into(), actor: actor.into(), detail: detail.into() })
    }
    /// Newest first.
    pub fn events(&self, limit: usize, before: Option<u64>) -> Vec<EventRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,ts_ms,kind,actor,detail FROM events WHERE id < ? ORDER BY id DESC LIMIT ?") else { return vec![] };
        let b = before.map_or(i64::MAX, |b| b.min(i64::MAX as u64) as i64);
        st.query_map(params![b, limit.min(500) as i64], |r| {
            Ok(EventRow { id: r.get::<_, i64>(0)? as u64, ts_ms: r.get(1)?, kind: r.get(2)?, actor: r.get(3)?, detail: r.get(4)? })
        })
        .map(|m| m.flatten().collect())
        .unwrap_or_default()
    }

    // ---- nodes (seen PCs, keyed by node_id)
    pub fn upsert_node(&self, node_id: &str, host: &str, addr: &str, inventory_json: &str, now_ms: i64) -> Result<(), String> {
        self.db()
            .execute(
                "INSERT INTO nodes (node_id,host,addr,inventory,first_seen_ms,last_seen_ms) VALUES (?1,?2,?3,?4,?5,?5)
                 ON CONFLICT(node_id) DO UPDATE SET host=?2, addr=?3, inventory=?4, last_seen_ms=?5",
                params![node_id, host, addr, inventory_json, now_ms],
            )
            .map(|_| ())
            .map_err(e)
    }
    pub fn nodes(&self) -> Vec<NodeRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare(&format!("{NODE_SEL} ORDER BY first_seen_ms,node_id")) else { return vec![] };
        st.query_map([], node_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }

    // ---- users and API tokens (tokens stored as sha256 hex, never raw)
    pub fn create_user(&self, name: &str, pw_hash: &str, role: &str, now_ms: i64) -> Result<UserRow, String> {
        if !ROLES.contains(&role) {
            return Err(format!("bad role {role}"));
        }
        let n = self
            .db()
            .execute("INSERT OR IGNORE INTO users (name,role,pw_hash,created_ms) VALUES (?,?,?,?)", params![name, role, pw_hash, now_ms])
            .map_err(e)?;
        if n == 0 {
            return Err("user exists".into());
        }
        Ok(UserRow { name: name.into(), role: role.into(), pw_hash: pw_hash.into(), created_ms: now_ms })
    }
    pub fn user(&self, name: &str) -> Option<UserRow> {
        self.db().query_row("SELECT name,role,pw_hash,created_ms FROM users WHERE name=?", [name], user_row).optional().ok().flatten()
    }
    pub fn users(&self) -> Vec<UserRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT name,role,pw_hash,created_ms FROM users ORDER BY created_ms,name") else { return vec![] };
        st.query_map([], user_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    pub fn add_token(&self, token_sha256_hex: &str, user: &str, now_ms: i64) -> Result<(), String> {
        self.db()
            .execute("INSERT INTO tokens (sha256,user,created_ms) VALUES (?,?,?)", params![token_sha256_hex, user, now_ms])
            .map(|_| ())
            .map_err(e)
    }
    /// The user a token hash belongs to.
    pub fn token_user(&self, token_sha256_hex: &str) -> Option<String> {
        self.db().query_row("SELECT user FROM tokens WHERE sha256=?", [token_sha256_hex], |r| r.get(0)).optional().ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> JobSpec {
        JobSpec { name: name.into(), kind: crate::proto::KIND_SHA, from: 100, end: 200, chunk: 10, arg: vec![1, 2, 255], only: vec!["a:1".into()], submitter: "bob".into(), ..Default::default() }
    }

    #[test]
    fn jobs_lifecycle_and_parts() {
        let s = Store::memory();
        let j = s.create_job(&spec("x"), 5).unwrap();
        assert_eq!((j.id, j.kind.as_str(), j.total, j.done, j.state, j.created_ms), (1, "sha", 100, 0, JobState::Queued, 5));
        assert_eq!(s.spec(1).unwrap(), spec("x"));
        assert!(s.spec(9).is_none() && s.job(9).is_none());
        assert!(s.set_state(9, JobState::Done, 1, None).is_err());

        let r = s.set_state(1, JobState::Running, 10, None).unwrap();
        assert_eq!((r.started_ms, r.finished_ms), (Some(10), None));
        let r = s.set_state(1, JobState::Running, 20, None).unwrap();
        assert_eq!(r.started_ms, Some(10), "started_ms only set once");
        assert_eq!(s.unfinished().len(), 1);

        assert!(s.add_part(1, 100, 110, b"a").unwrap());
        assert!(!s.add_part(1, 100, 110, b"zzz").unwrap(), "idempotent");
        assert!(s.add_part(1, 110, 120, b"b").unwrap());
        assert!(s.add_part(1, 150, 160, b"c").unwrap());
        assert_eq!(s.parts(1), vec![(100, 110, b"a".to_vec()), (110, 120, b"b".to_vec()), (150, 160, b"c".to_vec())]);
        assert_eq!(s.done_ranges(1), vec![(100, 120), (150, 160)]);
        s.add_part(1, 115, 155, b"o").unwrap(); // overlapping
        assert_eq!(s.done_ranges(1), vec![(100, 160)]);
        assert_eq!(s.job(1).unwrap().done, 10 + 10 + 10 + 40);

        let r = s.set_state(1, JobState::Failed, 30, Some("boom")).unwrap();
        assert_eq!((r.finished_ms, r.error.as_deref(), r.state), (Some(30), Some("boom"), JobState::Failed));
        assert!(s.unfinished().is_empty());
    }

    #[test]
    fn pagination_and_unfinished_order() {
        let s = Store::memory();
        for i in 0..7 {
            s.create_job(&spec(&format!("j{i}")), i).unwrap();
            s.log_event(i, "k", "a", &format!("d{i}")).unwrap();
        }
        s.set_state(2, JobState::Done, 9, None).unwrap();
        let ids = |v: Vec<JobRow>| v.into_iter().map(|j| j.id).collect::<Vec<_>>();
        assert_eq!(ids(s.jobs(3, None)), vec![7, 6, 5]);
        assert_eq!(ids(s.jobs(3, Some(5))), vec![4, 3, 2]);
        assert_eq!(ids(s.jobs(0, None)), Vec::<u64>::new());
        assert_eq!(ids(s.unfinished()), vec![1, 3, 4, 5, 6, 7]);
        let ev = s.events(2, None);
        assert_eq!((ev[0].id, ev[1].id, ev[0].detail.as_str()), (7, 6, "d6"));
        assert_eq!(s.events(100, Some(3)).iter().map(|x| x.id).collect::<Vec<_>>(), vec![2, 1]);
        for _ in 0..600 {
            s.log_event(0, "k", "a", "").unwrap();
        }
        assert_eq!(s.events(10_000, None).len(), 500, "limit capped");
    }

    #[test]
    fn nodes_users_tokens() {
        let s = Store::memory();
        s.upsert_node("n1", "h1", "1.1.1.1:1", "{}", 10).unwrap();
        s.upsert_node("n1", "h2", "2.2.2.2:2", "{\"a\":1}", 20).unwrap();
        s.upsert_node("n2", "h", "a", "{}", 15).unwrap();
        let n = s.nodes();
        assert_eq!(n.len(), 2);
        assert_eq!((n[0].node_id.as_str(), n[0].host.as_str(), n[0].first_seen_ms, n[0].last_seen_ms), ("n1", "h2", 10, 20));

        let u = s.create_user("ann", "pw", "admin", 1).unwrap();
        assert_eq!(s.user("ann").unwrap(), u);
        assert_eq!(s.create_user("ann", "x", "viewer", 2).unwrap_err(), "user exists");
        assert!(s.create_user("bo", "x", "root", 2).is_err());
        s.create_user("bo", "x", "viewer", 2).unwrap();
        assert_eq!(s.users().len(), 2);
        assert!(s.user("zed").is_none());

        s.add_token("abc", "ann", 3).unwrap();
        assert_eq!(s.token_user("abc").as_deref(), Some("ann"));
        assert!(s.token_user("nope").is_none());
        assert!(s.add_token("def", "ghost", 3).is_err(), "foreign key");
    }

    #[test]
    fn reopen_keeps_data_and_migrations_run_once() {
        let dir = std::env::temp_dir().join(format!("ck-store-{}-{}", std::process::id(), crate::jobs::now_ms()));
        let path = dir.join("sub").join("head.db");
        {
            let s = Store::open(&path).unwrap();
            s.create_job(&spec("keep"), 1).unwrap();
            s.add_part(1, 100, 110, b"p").unwrap();
            s.create_user("ann", "pw", "admin", 1).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.job(1).unwrap().name, "keep");
        assert_eq!(s.done_ranges(1), vec![(100, 110)]);
        assert_eq!(s.user("ann").unwrap().role, "admin");
        assert_eq!(s.create_job(&spec("two"), 2).unwrap().id, 2);
        let v: i64 = s.db().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- wave 3: labs, placement, node states, node keys, join codes
impl Store {
    pub fn create_lab(&self, name: &str, w: u32, h: u32, now_ms: i64) -> Result<LabRow, String> {
        check_lab(name, w, h)?;
        let c = self.db();
        c.execute("INSERT INTO labs (name,w,h,created_ms) VALUES (?,?,?,?)", params![name, w, h, now_ms]).map_err(e)?;
        let id = c.last_insert_rowid();
        c.query_row("SELECT id,name,w,h,created_ms FROM labs WHERE id=?", [id], lab_row).map_err(e)
    }
    /// None = keep. Shrinking a lab unplaces nodes that fall outside it.
    pub fn update_lab(&self, id: u64, name: Option<&str>, w: Option<u32>, h: Option<u32>) -> Result<LabRow, String> {
        let c = self.db();
        let cur = c.query_row("SELECT id,name,w,h,created_ms FROM labs WHERE id=?", [id as i64], lab_row).optional().map_err(e)?;
        let cur = cur.ok_or_else(|| format!("no lab {id}"))?;
        let (n, w, h) = (name.unwrap_or(&cur.name), w.unwrap_or(cur.w), h.unwrap_or(cur.h));
        check_lab(n, w, h)?;
        c.execute("UPDATE labs SET name=?,w=?,h=? WHERE id=?", params![n, w, h, id as i64]).map_err(e)?;
        c.execute("UPDATE nodes SET lab_id=NULL,x=NULL,y=NULL WHERE lab_id=? AND (x>=? OR y>=?)", params![id as i64, w, h]).map_err(e)?;
        c.query_row("SELECT id,name,w,h,created_ms FROM labs WHERE id=?", [id as i64], lab_row).map_err(e)
    }
    /// Deletes the lab and unplaces its nodes.
    pub fn delete_lab(&self, id: u64) -> Result<(), String> {
        let c = self.db();
        // foreign key SET NULL clears lab_id; x and y are cleared here so no stale seat stays
        c.execute("UPDATE nodes SET lab_id=NULL,x=NULL,y=NULL WHERE lab_id=?", [id as i64]).map_err(e)?;
        match c.execute("DELETE FROM labs WHERE id=?", [id as i64]).map_err(e)? {
            0 => Err(format!("no lab {id}")),
            _ => Ok(()),
        }
    }
    pub fn labs(&self) -> Vec<LabRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,name,w,h,created_ms FROM labs ORDER BY id") else { return vec![] };
        st.query_map([], lab_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    /// Some((lab, x, y)) seats the node (x < w, y < h, one node per cell: Err("seat taken")); None unplaces it.
    pub fn place_node(&self, node_id: &str, place: Option<(u64, u32, u32)>) -> Result<NodeRow, String> {
        let c = self.db();
        let known: Option<i64> = c.query_row("SELECT 1 FROM nodes WHERE node_id=?", [node_id], |r| r.get(0)).optional().map_err(e)?;
        known.ok_or_else(|| format!("no node {node_id}"))?;
        match place {
            None => {
                c.execute("UPDATE nodes SET lab_id=NULL,x=NULL,y=NULL WHERE node_id=?", [node_id]).map_err(e)?;
            }
            Some((lab, x, y)) => {
                let (w, h): (i64, i64) = c
                    .query_row("SELECT w,h FROM labs WHERE id=?", [lab as i64], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .map_err(e)?
                    .ok_or_else(|| format!("no lab {lab}"))?;
                if x as i64 >= w || y as i64 >= h {
                    return Err("outside the lab".into());
                }
                let taken: Option<String> = c
                    .query_row("SELECT node_id FROM nodes WHERE lab_id=? AND x=? AND y=? AND node_id<>?", params![lab as i64, x, y, node_id], |r| r.get(0))
                    .optional()
                    .map_err(e)?;
                if taken.is_some() {
                    return Err("seat taken".into());
                }
                c.execute("UPDATE nodes SET lab_id=?,x=?,y=? WHERE node_id=?", params![lab as i64, x, y, node_id]).map_err(e)?;
            }
        }
        c.query_row(&format!("{NODE_SEL} WHERE node_id=?"), [node_id], node_row).map_err(e)
    }
    /// state must be in jobs::NODE_STATES.
    pub fn set_node_state(&self, node_id: &str, state: &str, reason: &str, by: &str, now_ms: i64) -> Result<NodeRow, String> {
        let _ = now_ms; // no state timestamp column; the audit event carries the time
        if !crate::jobs::NODE_STATES.contains(&state) {
            return Err(format!("bad node state {state}"));
        }
        let c = self.db();
        let n = c.execute("UPDATE nodes SET state=?,state_reason=?,state_by=? WHERE node_id=?", params![state, reason, by, node_id]).map_err(e)?;
        if n == 0 {
            return Err(format!("no node {node_id}"));
        }
        c.query_row(&format!("{NODE_SEL} WHERE node_id=?"), [node_id], node_row).map_err(e)
    }
    pub fn node(&self, node_id: &str) -> Option<NodeRow> {
        self.db().query_row(&format!("{NODE_SEL} WHERE node_id=?"), [node_id], node_row).optional().ok().flatten()
    }
    /// Per-node credential (SEC-02), issued at admission. Stored as given (the head needs the raw key to verify).
    pub fn set_node_key(&self, node_id: &str, key: &[u8]) -> Result<(), String> {
        match self.db().execute("UPDATE nodes SET key=? WHERE node_id=?", params![key, node_id]).map_err(e)? {
            0 => Err(format!("no node {node_id}")),
            _ => Ok(()),
        }
    }
    pub fn node_key(&self, node_id: &str) -> Option<Vec<u8>> {
        self.db().query_row("SELECT key FROM nodes WHERE node_id=?", [node_id], |r| r.get::<_, Option<Vec<u8>>>(0)).optional().ok().flatten().flatten()
    }
    /// Join code stored as sha256 hex. `uses` = how many PCs may join with it.
    pub fn add_join_code(&self, code_sha256_hex: &str, uses: u32, expires_ms: i64, auto_approve: bool, by: &str, now_ms: i64) -> Result<(), String> {
        self.db()
            .execute(
                "INSERT INTO join_codes (sha256,uses_left,expires_ms,auto_approve,created_by,created_ms) VALUES (?,?,?,?,?,?)",
                params![code_sha256_hex, uses, expires_ms, auto_approve, by, now_ms],
            )
            .map(|_| ())
            .map_err(e)
    }
    /// Spends one use. Some(auto_approve) when valid; None when unknown, expired or used up.
    pub fn use_join_code(&self, code_sha256_hex: &str, now_ms: i64) -> Option<bool> {
        self.db()
            .query_row(
                "UPDATE join_codes SET uses_left=uses_left-1 WHERE sha256=? AND uses_left>0 AND expires_ms>? RETURNING auto_approve",
                params![code_sha256_hex, now_ms],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests3 {
    use super::*;

    fn with_nodes() -> Store {
        let s = Store::memory();
        for (i, n) in ["a", "b", "c"].iter().enumerate() {
            s.upsert_node(n, "h", "addr", "{}", i as i64).unwrap();
        }
        s
    }

    #[test]
    fn migration_2_from_v1_marks_nodes_active() {
        let dir = std::env::temp_dir().join(format!("ck-store-m2-{}-{}", std::process::id(), crate::jobs::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch("PRAGMA user_version = 1").unwrap();
            c.execute("INSERT INTO nodes VALUES ('old','h','a','{}',1,2)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let n = s.node("old").unwrap();
        assert_eq!((n.state.as_str(), n.lab_id, n.x), ("active", None, None));
        s.upsert_node("new", "h", "a", "{}", 5).unwrap();
        assert_eq!(s.node("new").unwrap().state, "pending");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn labs_placement_and_unplacing() {
        let s = with_nodes();
        assert!(s.create_lab("", 3, 3, 1).is_err() && s.create_lab(&"x".repeat(81), 3, 3, 1).is_err());
        assert!(s.create_lab("L", 0, 3, 1).is_err() && s.create_lab("L", 3, 65, 1).is_err());
        let lab = s.create_lab("Lab 1", 4, 4, 7).unwrap();
        assert_eq!((lab.id, lab.w, lab.h, lab.created_ms), (1, 4, 4, 7));
        assert_eq!(s.labs(), vec![lab.clone()]);

        let n = s.place_node("a", Some((1, 3, 3))).unwrap();
        assert_eq!((n.lab_id, n.x, n.y), (Some(1), Some(3), Some(3)));
        assert_eq!(s.place_node("b", Some((1, 3, 3))).unwrap_err(), "seat taken");
        assert!(s.place_node("b", Some((1, 4, 0))).is_err() && s.place_node("b", Some((9, 0, 0))).is_err());
        assert!(s.place_node("zz", Some((1, 0, 0))).is_err());
        s.place_node("a", Some((1, 3, 3))).unwrap(); // same seat again is fine
        s.place_node("b", Some((1, 0, 0))).unwrap();
        s.place_node("c", Some((1, 1, 0))).unwrap();

        let l = s.update_lab(1, Some("Renamed"), Some(2), None).unwrap();
        assert_eq!((l.name.as_str(), l.w, l.h), ("Renamed", 2, 4));
        assert_eq!(s.node("a").unwrap().lab_id, None, "outside after shrink");
        assert_eq!(s.node("a").unwrap().x, None);
        assert_eq!(s.node("c").unwrap().lab_id, Some(1));
        assert!(s.update_lab(1, None, Some(65), None).is_err() && s.update_lab(9, None, None, None).is_err());

        let n = s.place_node("c", None).unwrap();
        assert_eq!((n.lab_id, n.x, n.y), (None, None, None));
        s.place_node("c", Some((1, 1, 1))).unwrap();
        s.delete_lab(1).unwrap();
        let n = s.node("b").unwrap();
        assert_eq!((n.lab_id, n.x, n.y), (None, None, None));
        assert_eq!(s.node("c").unwrap().y, None);
        assert!(s.labs().is_empty() && s.delete_lab(1).is_err());
    }

    #[test]
    fn states_keys_and_upsert_keeps_them() {
        let s = with_nodes();
        let lab = s.create_lab("L", 2, 2, 1).unwrap();
        s.place_node("a", Some((lab.id, 1, 1))).unwrap();
        s.set_node_key("a", &[0, 1, 255]).unwrap();
        let n = s.set_node_state("a", "cordoned", "fan noise", "ann", 5).unwrap();
        assert_eq!((n.state.as_str(), n.state_reason.as_str(), n.state_by.as_str()), ("cordoned", "fan noise", "ann"));
        assert!(s.set_node_state("a", "sleepy", "", "", 1).is_err() && s.set_node_state("zz", "active", "", "", 1).is_err());
        for st in crate::jobs::NODE_STATES {
            s.set_node_state("b", st, "", "x", 1).unwrap();
        }
        assert!(s.set_node_key("zz", b"k").is_err());

        s.upsert_node("a", "h2", "new", "{\"x\":1}", 99).unwrap();
        let n = s.node("a").unwrap();
        assert_eq!((n.host.as_str(), n.last_seen_ms, n.state.as_str(), n.lab_id, n.x, n.y), ("h2", 99, "cordoned", Some(1), Some(1), Some(1)));
        assert_eq!(s.node_key("a"), Some(vec![0, 1, 255]));
        assert_eq!(s.node_key("b"), None);
        assert_eq!(s.node_key("zz"), None);
        assert!(s.node("zz").is_none());
        assert_eq!(s.nodes().len(), 3);
    }

    #[test]
    fn join_codes_uses_and_expiry() {
        let s = Store::memory();
        s.add_join_code("h1", 2, 1000, true, "ann", 1).unwrap();
        assert!(s.add_join_code("h1", 1, 1, false, "ann", 1).is_err(), "duplicate");
        assert_eq!(s.use_join_code("h1", 10), Some(true));
        assert_eq!(s.use_join_code("h1", 11), Some(true));
        assert_eq!(s.use_join_code("h1", 12), None, "used up");
        assert_eq!(s.use_join_code("nope", 12), None);
        s.add_join_code("h2", 5, 100, false, "ann", 1).unwrap();
        assert_eq!(s.use_join_code("h2", 99), Some(false));
        assert_eq!(s.use_join_code("h2", 100), None, "expired");
    }
}

// ---------------------------------------------------------------- M2: labels, pools, caps, reserves
fn check_name(what: &str, v: &str, max: usize) -> Result<(), String> {
    if v.is_empty() || v.len() > max || !v.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-')) {
        return Err(format!("{what} must be 1 to {max} characters of a-z 0-9 _ . -"));
    }
    Ok(())
}

impl Store {
    /// Set (or with value None remove) one free label. Keys: lower case [a-z0-9_.-], 1..=40; values 1..=120 chars.
    pub fn set_label(&self, node_id: &str, key: &str, value: Option<&str>) -> Result<(), String> {
        check_name("label key", key, 40)?;
        let c = self.db();
        match value {
            None => {
                c.execute("DELETE FROM labels WHERE node_id=? AND key=?", params![node_id, key]).map_err(e)?;
            }
            Some(v) => {
                if v.is_empty() || v.chars().count() > 120 {
                    return Err("label value must be 1 to 120 characters".into());
                }
                let known: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM nodes WHERE node_id=?)", [node_id], |r| r.get(0)).map_err(e)?;
                if !known {
                    return Err(format!("no node {node_id}"));
                }
                c.execute(
                    "INSERT INTO labels (node_id,key,value) VALUES (?,?,?) ON CONFLICT(node_id,key) DO UPDATE SET value=excluded.value",
                    params![node_id, key, v],
                )
                .map_err(e)?;
            }
        }
        Ok(())
    }
    /// Every label of every node: (node_id, key, value), ordered.
    pub fn labels(&self) -> Vec<(String, String, String)> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT node_id,key,value FROM labels ORDER BY node_id,key") else { return vec![] };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    /// Create or replace a pool (name 1..=40 chars [a-z0-9_.-]); members must be known nodes.
    pub fn put_pool(&self, name: &str, members: &[String], note: &str) -> Result<PoolRow, String> {
        check_name("pool name", name, 40)?;
        let mut c = self.db();
        let tx = c.transaction().map_err(e)?;
        for m in members {
            let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM nodes WHERE node_id=?)", [m], |r| r.get(0)).map_err(e)?;
            if !known {
                return Err(format!("unknown node {m}"));
            }
        }
        tx.execute("INSERT INTO pools (name,note) VALUES (?1,?2) ON CONFLICT(name) DO UPDATE SET note=?2", params![name, note]).map_err(e)?;
        tx.execute("DELETE FROM pool_members WHERE pool=?", [name]).map_err(e)?;
        for m in members {
            tx.execute("INSERT OR IGNORE INTO pool_members (pool,node_id) VALUES (?,?)", params![name, m]).map_err(e)?;
        }
        tx.commit().map_err(e)?;
        Ok(pool_of(&c, name, note))
    }
    pub fn delete_pool(&self, name: &str) -> Result<(), String> {
        match self.db().execute("DELETE FROM pools WHERE name=?", [name]).map_err(e)? {
            0 => Err(format!("no pool {name}")),
            _ => Ok(()),
        }
    }
    pub fn pools(&self) -> Vec<PoolRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT name,note FROM pools ORDER BY name") else { return vec![] };
        let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|m| m.flatten().collect()).unwrap_or_default();
        rows.into_iter().map(|(n, note)| pool_of(&c, &n, &note)).collect()
    }
    pub fn add_cap(&self, selector: &str, max_threads: Option<u32>, note: &str) -> Result<CapRow, String> {
        if max_threads == Some(0) {
            return Err("max_threads must be at least 1".into());
        }
        let c = self.db();
        c.execute("INSERT INTO caps (selector,max_threads,note) VALUES (?,?,?)", params![selector, max_threads.map(|v| v as i64), note]).map_err(e)?;
        Ok(CapRow { id: c.last_insert_rowid() as u64, selector: selector.into(), max_threads, note: note.into() })
    }
    pub fn delete_cap(&self, id: u64) -> Result<(), String> {
        match self.db().execute("DELETE FROM caps WHERE id=?", [id as i64]).map_err(e)? {
            0 => Err(format!("no cap {id}")),
            _ => Ok(()),
        }
    }
    pub fn caps(&self) -> Vec<CapRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,selector,max_threads,note FROM caps ORDER BY id") else { return vec![] };
        st.query_map([], |r| {
            Ok(CapRow { id: r.get::<_, i64>(0)? as u64, selector: r.get(1)?, max_threads: r.get::<_, Option<i64>>(2)?.map(|v| v as u32), note: r.get(3)? })
        })
        .map(|m| m.flatten().collect())
        .unwrap_or_default()
    }
    /// keep_pct 0..=100.
    pub fn add_reserve(&self, selector: &str, keep_n: u32, keep_pct: u32, note: &str) -> Result<ReserveRow, String> {
        if keep_pct > 100 {
            return Err("keep_pct must be 0 to 100".into());
        }
        let c = self.db();
        c.execute("INSERT INTO reserves (selector,keep_n,keep_pct,note) VALUES (?,?,?,?)", params![selector, keep_n as i64, keep_pct as i64, note]).map_err(e)?;
        Ok(ReserveRow { id: c.last_insert_rowid() as u64, selector: selector.into(), keep_n, keep_pct, note: note.into() })
    }
    pub fn delete_reserve(&self, id: u64) -> Result<(), String> {
        match self.db().execute("DELETE FROM reserves WHERE id=?", [id as i64]).map_err(e)? {
            0 => Err(format!("no reserve {id}")),
            _ => Ok(()),
        }
    }
    pub fn reserves(&self) -> Vec<ReserveRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,selector,keep_n,keep_pct,note FROM reserves ORDER BY id") else { return vec![] };
        st.query_map([], |r| {
            Ok(ReserveRow { id: r.get::<_, i64>(0)? as u64, selector: r.get(1)?, keep_n: r.get::<_, i64>(2)? as u32, keep_pct: r.get::<_, i64>(3)? as u32, note: r.get(4)? })
        })
        .map(|m| m.flatten().collect())
        .unwrap_or_default()
    }
}

fn pool_of(c: &Connection, name: &str, note: &str) -> PoolRow {
    let members = c
        .prepare("SELECT node_id FROM pool_members WHERE pool=? ORDER BY node_id")
        .and_then(|mut st| st.query_map([name], |r| r.get(0)).map(|m| m.flatten().collect()))
        .unwrap_or_default();
    PoolRow { name: name.into(), members, note: note.into() }
}

#[cfg(test)]
mod tests4 {
    use super::*;

    fn with_nodes() -> Store {
        let s = Store::memory();
        for n in ["a", "b", "c"] {
            s.upsert_node(n, "h", "addr", "{}", 1).unwrap();
        }
        s
    }

    #[test]
    fn job_selector_priority_round_trip() {
        let s = Store::memory();
        let sp = JobSpec { name: "j".into(), selector: "gpu & lab:1".into(), priority: 0, ..Default::default() };
        let j = s.create_job(&sp, 1).unwrap();
        assert_eq!((j.selector.as_str(), j.priority), ("gpu & lab:1", 0));
        assert_eq!(s.job(j.id).unwrap().priority, 0);
        let back = s.spec(j.id).unwrap();
        assert_eq!((back.selector.as_str(), back.priority), ("gpu & lab:1", 0));
        assert_eq!(s.unfinished()[0].selector, "gpu & lab:1");
        let d = s.create_job(&JobSpec::default(), 2).unwrap();
        assert_eq!((d.selector.as_str(), d.priority), ("", 2));
    }

    #[test]
    fn labels_validate_set_remove_cascade() {
        let s = with_nodes();
        s.set_label("a", "room", Some("101")).unwrap();
        s.set_label("a", "room", Some("102")).unwrap();
        s.set_label("b", "gpu", Some("yes")).unwrap();
        assert_eq!(s.labels(), vec![("a".into(), "room".into(), "102".into()), ("b".into(), "gpu".into(), "yes".into())]);
        assert!(s.set_label("a", "Bad", Some("x")).is_err() && s.set_label("a", "", Some("x")).is_err());
        assert!(s.set_label("a", &"k".repeat(41), Some("x")).is_err());
        assert!(s.set_label("a", "k", Some("")).is_err() && s.set_label("a", "k", Some(&"v".repeat(121))).is_err());
        assert!(s.set_label("zz", "k", Some("v")).is_err());
        s.set_label("a", "room", None).unwrap();
        s.set_label("a", "nothing", None).unwrap();
        assert_eq!(s.labels().len(), 1);
    }

    #[test]
    fn pools_replace_validate_cascade() {
        let s = with_nodes();
        let p = s.put_pool("lab-1", &["b".into(), "a".into()], "n").unwrap();
        assert_eq!((p.members.clone(), p.note.as_str()), (vec!["a".to_string(), "b".to_string()], "n"));
        let p = s.put_pool("lab-1", &["c".into()], "m").unwrap();
        assert_eq!(p.members, vec!["c".to_string()]);
        assert!(s.put_pool("Bad", &[], "").is_err() && s.put_pool("", &[], "").is_err() && s.put_pool(&"p".repeat(41), &[], "").is_err());
        assert!(s.put_pool("x", &["zz".into()], "").is_err());
        assert_eq!(s.pools().len(), 1, "failed put created nothing");
        s.put_pool("other", &["c".into()], "").unwrap();
        assert_eq!(s.pools().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["lab-1", "other"]);
        s.delete_pool("lab-1").unwrap();
        let n: i64 = s.db().query_row("SELECT COUNT(*) FROM pool_members WHERE pool='lab-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(s.delete_pool("lab-1").is_err());
        s.db().execute("DELETE FROM nodes WHERE node_id='c'", []).unwrap();
        assert!(s.pools()[0].members.is_empty(), "node delete cascades");
    }

    #[test]
    fn caps_and_reserves() {
        let s = Store::memory();
        assert!(s.add_cap("all", Some(0), "").is_err());
        let a = s.add_cap("lab:1", Some(4), "n").unwrap();
        let b = s.add_cap("lab:2", None, "").unwrap();
        assert_eq!(s.caps(), vec![a.clone(), b]);
        s.delete_cap(a.id).unwrap();
        assert!(s.delete_cap(a.id).is_err());
        assert_eq!(s.caps().len(), 1);
        assert!(s.add_reserve("all", 1, 101, "").is_err());
        let r = s.add_reserve("all", 2, 100, "x").unwrap();
        assert_eq!(s.reserves(), vec![r.clone()]);
        s.delete_reserve(r.id).unwrap();
        assert!(s.delete_reserve(r.id).is_err() && s.reserves().is_empty());
    }

    #[test]
    fn migration_3_from_v2_keeps_jobs_at_normal_priority() {
        let dir = std::env::temp_dir().join(format!("ck-store-m3-{}-{}", std::process::id(), crate::jobs::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch(MIGRATIONS[1]).unwrap();
            c.execute_batch("PRAGMA user_version = 2").unwrap();
            c.execute("INSERT INTO jobs (name,kind,from_n,end_n,chunk,arg,only_json,submitter,state,created_ms) VALUES ('old',0,0,10,5,x'',  '[]','u','queued',1)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let j = s.job(1).unwrap();
        assert_eq!((j.priority, j.selector.as_str()), (2, ""));
        assert_eq!(s.spec(1).unwrap().priority, 2);
        s.put_pool("p", &[], "").unwrap();
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------- M3 (windows, reservations, list items, node calibration)
fn day_idx(d: &str) -> Option<usize> {
    ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|x| *x == d)
}

/// Lower-cased days if every comma part is a day name or a range "a-b" of them.
fn check_days(days: &str) -> Result<String, String> {
    let d = days.trim().to_lowercase();
    let ok = !d.is_empty()
        && d.split(',').all(|p| {
            let p = p.trim();
            match p.split_once('-') {
                Some((a, b)) => day_idx(a.trim()).is_some() && day_idx(b.trim()).is_some(),
                None => day_idx(p).is_some(),
            }
        });
    if ok { Ok(d) } else { Err(format!("bad days '{days}' (mon..sun, commas, ranges like mon-fri)")) }
}

fn check_hhmm(t: &str) -> Result<(), String> {
    let ok = t.len() == 5
        && t.as_bytes()[2] == b':'
        && t[..2].bytes().all(|b| b.is_ascii_digit())
        && t[3..].bytes().all(|b| b.is_ascii_digit())
        && t[..2].parse::<u8>().unwrap_or(99) < 24
        && t[3..].parse::<u8>().unwrap_or(99) < 60;
    if ok { Ok(()) } else { Err(format!("bad time '{t}' (HH:MM, 00:00 to 23:59)")) }
}

fn window_row(r: &Row) -> rusqlite::Result<WindowRow> {
    Ok(WindowRow {
        id: r.get::<_, i64>(0)? as u64,
        selector: r.get(1)?,
        days: r.get(2)?,
        start: r.get(3)?,
        end: r.get(4)?,
        action: r.get(5)?,
        cap_threads: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
        note: r.get(7)?,
    })
}

fn max_items() -> usize {
    crate::tune::n("max_items") as usize
}

fn max_item_bytes() -> usize {
    crate::tune::n("max_item_bytes") as usize
}

impl Store {
    /// days like "mon-fri" / "sat,sun"; start/end "HH:MM" (end may be < start: overnight); action "block" or "cap".
    pub fn add_window(&self, w: &WindowRow) -> Result<WindowRow, String> {
        let days = check_days(&w.days)?;
        check_hhmm(&w.start)?;
        check_hhmm(&w.end)?;
        let cap = match (w.action.as_str(), w.cap_threads) {
            ("block", _) => None,
            ("cap", Some(n)) if n >= 1 => Some(n as i64),
            ("cap", _) => return Err("cap window needs cap_threads >= 1".into()),
            (a, _) => return Err(format!("bad action '{a}' (block or cap)")),
        };
        let c = self.db();
        c.execute(
            "INSERT INTO windows (selector,days,start,\"end\",action,cap_threads,note) VALUES (?,?,?,?,?,?,?)",
            params![w.selector, days, w.start, w.end, w.action, cap, w.note],
        )
        .map_err(e)?;
        Ok(WindowRow { id: c.last_insert_rowid() as u64, days, cap_threads: cap.map(|v| v as u32), ..w.clone() })
    }
    pub fn delete_window(&self, id: u64) -> Result<(), String> {
        match self.db().execute("DELETE FROM windows WHERE id=?", [id as i64]).map_err(e)? {
            0 => Err(format!("no window {id}")),
            _ => Ok(()),
        }
    }
    pub fn windows(&self) -> Vec<WindowRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,selector,days,start,\"end\",action,cap_threads,note FROM windows ORDER BY id") else { return vec![] };
        st.query_map([], window_row).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    /// end_ms > start_ms.
    pub fn add_reservation(&self, r: &ReservationRow) -> Result<ReservationRow, String> {
        if r.end_ms <= r.start_ms {
            return Err("reservation end must be after start".into());
        }
        let c = self.db();
        c.execute("INSERT INTO reservations (selector,start_ms,end_ms,note) VALUES (?,?,?,?)", params![r.selector, r.start_ms, r.end_ms, r.note]).map_err(e)?;
        Ok(ReservationRow { id: c.last_insert_rowid() as u64, ..r.clone() })
    }
    pub fn delete_reservation(&self, id: u64) -> Result<(), String> {
        match self.db().execute("DELETE FROM reservations WHERE id=?", [id as i64]).map_err(e)? {
            0 => Err(format!("no reservation {id}")),
            _ => Ok(()),
        }
    }
    /// Ordered by start_ms. `from_ms` = only those ending after it (None = all).
    pub fn reservations(&self, from_ms: Option<i64>) -> Vec<ReservationRow> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT id,selector,start_ms,end_ms,note FROM reservations WHERE end_ms > ? ORDER BY start_ms,id") else { return vec![] };
        st.query_map([from_ms.unwrap_or(i64::MIN)], |r| {
            Ok(ReservationRow { id: r.get::<_, i64>(0)? as u64, selector: r.get(1)?, start_ms: r.get(2)?, end_ms: r.get(3)?, note: r.get(4)? })
        })
        .map(|m| m.flatten().collect())
        .unwrap_or_default()
    }
    /// List job items (JOB-01), index 0.. in order. Replaces any stored items of the job.
    pub fn put_items(&self, job: u64, items: &[String]) -> Result<(), String> {
        if items.len() > max_items() {
            return Err(format!("at most {} items", max_items()));
        }
        if let Some(i) = items.iter().position(|s| s.len() > max_item_bytes()) {
            return Err(format!("item {i} is over {} bytes", max_item_bytes()));
        }
        let mut c = self.db();
        let tx = c.transaction().map_err(e)?;
        tx.execute("DELETE FROM items WHERE job_id=?", [job as i64]).map_err(e)?;
        {
            let mut st = tx.prepare("INSERT INTO items (job_id,idx,item) VALUES (?,?,?)").map_err(e)?;
            for (i, it) in items.iter().enumerate() {
                st.execute(params![job as i64, i as i64, it]).map_err(e)?;
            }
        }
        tx.commit().map_err(e)
    }
    /// Items [a, b) of a job, in order.
    pub fn items(&self, job: u64, a: u64, b: u64) -> Vec<String> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT item FROM items WHERE job_id=? AND idx>=? AND idx<? ORDER BY idx") else { return vec![] };
        let (a, b) = (a.min(i64::MAX as u64) as i64, b.min(i64::MAX as u64) as i64);
        st.query_map(params![job as i64, a, b], |r| r.get(0)).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    // ---- wave 13: blobs (docs/BULK.md)
    /// Store the blob list and mode of a job (columns apart from the main insert so the old statements stay as they are).
    pub fn set_job_blobs(&self, id: u64, spec: &JobSpec) {
        if spec.blobs.is_empty() && spec.blob_mode.is_empty() {
            return;
        }
        let b = serde_json::to_string(&spec.blobs).unwrap_or_else(|_| "[]".into());
        let _ = self.db().execute("UPDATE jobs SET blobs=?, blob_mode=? WHERE id=?", params![b, spec.blob_mode, id as i64]);
    }
    pub fn put_blob(&self, sha: &str, size: u64, now_ms: i64) {
        let _ = self.db().execute(
            "INSERT INTO blobs (sha,size,created_ms,last_used_ms) VALUES (?1,?2,?3,?3) ON CONFLICT(sha) DO UPDATE SET last_used_ms=?3",
            params![sha, size as i64, now_ms],
        );
    }
    pub fn blob_size(&self, sha: &str) -> Option<u64> {
        self.db().query_row("SELECT size FROM blobs WHERE sha=?", [sha], |r| r.get::<_, i64>(0)).optional().ok().flatten().map(|n| n as u64)
    }
    pub fn touch_blob(&self, sha: &str, now_ms: i64) {
        let _ = self.db().execute("UPDATE blobs SET last_used_ms=? WHERE sha=?", params![now_ms, sha]);
    }
    /// (sha, size, refs, created_ms, last_used_ms), newest first. refs = unfinished jobs that name the blob.
    pub fn blob_rows(&self) -> Vec<(String, u64, u64, i64, i64)> {
        let c = self.db();
        let Ok(mut st) = c.prepare(
            "SELECT sha,size,(SELECT COUNT(*) FROM jobs WHERE state IN ('queued','running') AND blobs LIKE '%\"' || blobs.sha || '\"%'),created_ms,last_used_ms FROM blobs ORDER BY created_ms DESC",
        ) else { return vec![] };
        st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64, r.get(3)?, r.get(4)?))).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    pub fn delete_blob(&self, sha: &str) {
        let _ = self.db().execute("DELETE FROM blobs WHERE sha=?", [sha]);
    }
    pub fn set_node_net(&self, node_id: &str, mbps: f64, rtt_ms: f64, now_ms: i64) {
        let _ = self.db().execute("UPDATE nodes SET net_mbps=?, rtt_ms=?, probed_ms=? WHERE node_id=?", params![mbps, rtt_ms, now_ms, node_id]);
    }
    pub fn set_node_calib(&self, node_id: &str, mhs: f64) -> Result<(), String> {
        match self.db().execute("UPDATE nodes SET calib_mhs=? WHERE node_id=?", params![mhs, node_id]).map_err(e)? {
            0 => Err(format!("no node {node_id}")),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests5 {
    use super::*;

    fn win(days: &str, a: &str, b: &str, act: &str, cap: Option<u32>) -> WindowRow {
        WindowRow { id: 0, selector: "all".into(), days: days.into(), start: a.into(), end: b.into(), action: act.into(), cap_threads: cap, note: "n".into() }
    }

    #[test]
    fn job_policy_reducer_items_round_trip() {
        let s = Store::memory();
        let sp = JobSpec { name: "l".into(), kind: crate::proto::KIND_LIST, end: 3, chunk: 1, policy: "adaptive".into(), reducer: "sum".into(),
            items: vec!["a".into(), "b b".into(), "".into()], ..Default::default() };
        let j = s.create_job(&sp, 1).unwrap();
        assert_eq!((j.policy.as_str(), j.reducer.as_str(), j.eta_ms), ("adaptive", "sum", None));
        let back = s.spec(j.id).unwrap();
        assert_eq!((back.policy.as_str(), back.reducer.as_str()), ("adaptive", "sum"));
        assert_eq!(s.items(j.id, 0, 10), vec!["a", "b b", ""]);
        assert_eq!(s.items(j.id, 1, 2), vec!["b b"]);
        assert!(s.items(j.id, 5, 9).is_empty() && s.items(j.id, 2, 1).is_empty());
        s.put_items(j.id, &["z".into()]).unwrap();
        assert_eq!(s.items(j.id, 0, 10), vec!["z"]);
        // too-long item rejected, nothing created
        let bad = JobSpec { items: vec!["x".repeat(4097)], ..sp.clone() };
        assert!(s.create_job(&bad, 1).is_err());
        assert_eq!(s.jobs(10, None).len(), 1);
        assert!(s.put_items(j.id, &["x".repeat(4096)]).is_ok());
        assert_eq!(s.items(j.id, 0, 1)[0].len(), 4096);
    }

    #[test]
    fn windows_crud_and_validation() {
        let s = Store::memory();
        let w = s.add_window(&win("Mon-Fri, sat", "08:00", "17:30", "block", Some(9))).unwrap();
        assert_eq!((w.id, w.days.as_str()), (1, "mon-fri, sat".trim()));
        let c = s.add_window(&win("sat,sun", "22:00", "06:00", "cap", Some(2))).unwrap();
        assert_eq!(s.windows(), vec![WindowRow { cap_threads: None, ..w.clone() }, c.clone()]);
        for (d, a, b, act, cap) in [
            ("", "08:00", "09:00", "block", None),
            ("funday", "08:00", "09:00", "block", None),
            ("mon-", "08:00", "09:00", "block", None),
            ("mon", "24:00", "09:00", "block", None),
            ("mon", "8:00", "09:00", "block", None),
            ("mon", "08:00", "09:60", "block", None),
            ("mon", "08:00", "09:00", "pause", None),
            ("mon", "08:00", "09:00", "cap", None),
            ("mon", "08:00", "09:00", "cap", Some(0)),
        ] {
            assert!(s.add_window(&win(d, a, b, act, cap)).is_err(), "{d} {a} {b} {act}");
        }
        s.delete_window(w.id).unwrap();
        assert!(s.delete_window(w.id).is_err());
        assert_eq!(s.windows(), vec![c]);
    }

    #[test]
    fn reservations_crud_and_filter() {
        let s = Store::memory();
        let r = |a, b| ReservationRow { id: 0, selector: "lab:1".into(), start_ms: a, end_ms: b, note: "x".into() };
        assert!(s.add_reservation(&r(5, 5)).is_err() && s.add_reservation(&r(6, 5)).is_err());
        let late = s.add_reservation(&r(100, 200)).unwrap();
        let early = s.add_reservation(&r(10, 50)).unwrap();
        assert_eq!(s.reservations(None), vec![early.clone(), late.clone()]);
        assert_eq!(s.reservations(Some(50)), vec![late.clone()]); // ends after 50, strictly
        assert_eq!(s.reservations(Some(49)).len(), 2);
        assert!(s.reservations(Some(200)).is_empty());
        s.delete_reservation(early.id).unwrap();
        assert!(s.delete_reservation(early.id).is_err());
        assert_eq!(s.reservations(None), vec![late]);
    }

    #[test]
    fn items_cascade_and_limits() {
        let s = Store::memory();
        let j = s.create_job(&JobSpec { name: "n".into(), end: 1, chunk: 1, ..Default::default() }, 1).unwrap();
        s.put_items(j.id, &["a".into()]).unwrap();
        s.db().execute("DELETE FROM jobs WHERE id=?", [j.id as i64]).unwrap();
        assert!(s.items(j.id, 0, 5).is_empty());
        assert!(s.put_items(1, &vec![String::new(); max_items() + 1]).is_err());
    }

    #[test]
    fn calib_round_trip() {
        let s = Store::memory();
        s.upsert_node("n1", "h", "a", "{}", 1).unwrap();
        assert_eq!(s.nodes()[0].calib_mhs, None);
        s.set_node_calib("n1", 12.5).unwrap();
        s.upsert_node("n1", "h", "a", "{}", 2).unwrap(); // upsert keeps it
        assert_eq!(s.nodes()[0].calib_mhs, Some(12.5));
        assert!(s.set_node_calib("nope", 1.0).is_err());
    }

    #[test]
    fn migration_4_from_v3() {
        let dir = std::env::temp_dir().join(format!("ck-store-m4-{}-{}", std::process::id(), crate::jobs::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.db");
        {
            let c = Connection::open(&path).unwrap();
            for m in &MIGRATIONS[..3] {
                c.execute_batch(m).unwrap();
            }
            c.execute_batch("PRAGMA user_version = 3").unwrap();
            c.execute("INSERT INTO jobs (name,kind,from_n,end_n,chunk,arg,only_json,submitter,state,created_ms) VALUES ('old',0,0,10,5,x'','[]','u','queued',1)", []).unwrap();
            c.execute("INSERT INTO nodes (node_id,host,addr,inventory,first_seen_ms,last_seen_ms) VALUES ('n','h','a','{}',1,1)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let j = s.job(1).unwrap();
        assert_eq!((j.policy.as_str(), j.reducer.as_str()), ("", ""));
        assert_eq!(s.spec(1).unwrap().policy, "");
        assert_eq!(s.nodes()[0].calib_mhs, None);
        s.set_node_calib("n", 3.0).unwrap();
        assert!(s.windows().is_empty() && s.reservations(None).is_empty() && s.items(1, 0, 9).is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------- M4 (HA: meta values, snapshots, replay).
impl Store {
    /// Small key/value settings: "epoch", "membership" (JSON list of node ids), "last_seq".
    pub fn meta(&self, key: &str) -> Option<String> {
        self.db().query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional().ok().flatten()
    }
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.db().execute("INSERT INTO meta (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2", params![key, value]).map(|_| ()).map_err(e)
    }
    /// Consistent copy of the whole database into `path` (VACUUM INTO), for a standby's first sync.
    pub fn snapshot_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|x| format!("create {}: {x}", d.display()))?;
        }
        for p in [path.to_path_buf(), PathBuf::from(format!("{}-wal", path.display())), PathBuf::from(format!("{}-shm", path.display()))] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(x) if x.kind() == std::io::ErrorKind::NotFound => {}
                Err(x) => return Err(format!("remove {}: {x}", p.display())),
            }
        }
        let p = path.to_str().ok_or("snapshot path is not utf-8")?;
        self.db().execute("VACUUM INTO ?", [p]).map(|_| ()).map_err(e)
    }
    /// Replay on a standby: create the job with exactly this id (and its items); Ok(false) if it exists already.
    /// SQLite AUTOINCREMENT raises sqlite_sequence to the highest explicit id, so a promoted standby never reuses ids.
    pub fn insert_job_with_id(&self, id: u64, spec: &JobSpec, now_ms: i64) -> Result<bool, String> {
        let only = serde_json::to_string(&spec.only).map_err(|x| x.to_string())?;
        let n = self
            .db()
            .execute(
                "INSERT OR IGNORE INTO jobs (id,name,kind,from_n,end_n,chunk,arg,only_json,submitter,selector,priority,policy,reducer,max_pcs,placement,mem_mb,state,created_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'queued',?)",
                params![id as i64, spec.name, spec.kind as i64, spec.from as i64, spec.end as i64, spec.chunk as i64, spec.arg, only, spec.submitter, spec.selector, spec.priority as i64, spec.policy, spec.reducer, spec.max_pcs as i64, spec.placement, spec.mem_mb as i64, now_ms],
            )
            .map_err(e)?;
        if n == 0 {
            return Ok(false);
        }
        self.set_job_blobs(id, spec);
        if !spec.items.is_empty() {
            if let Err(x) = self.put_items(id, &spec.items) {
                let _ = self.db().execute("DELETE FROM jobs WHERE id=?", [id as i64]);
                return Err(x);
            }
        }
        Ok(true)
    }
    /// Replay on a standby: replace users, tokens (hashes) and node keys with the primary's.
    pub fn replace_auth(&self, users: &[UserRow], tokens: &[(String, String)], node_keys: &[(String, Vec<u8>)]) -> Result<(), String> {
        let mut c = self.db();
        let tx = c.transaction().map_err(e)?;
        tx.execute("DELETE FROM tokens", []).map_err(e)?;
        for u in users {
            tx.execute(
                "INSERT INTO users (name,role,pw_hash,created_ms) VALUES (?1,?2,?3,?4) ON CONFLICT(name) DO UPDATE SET role=?2, pw_hash=?3, created_ms=?4",
                params![u.name, u.role, u.pw_hash, u.created_ms],
            )
            .map_err(e)?;
        }
        for (sha, user) in tokens {
            tx.execute("INSERT INTO tokens (sha256,user,created_ms) VALUES (?,?,0)", params![sha, user]).map_err(e)?;
        }
        for (id, key) in node_keys {
            tx.execute("UPDATE nodes SET key=? WHERE node_id=?", params![key, id]).map_err(e)?; // unknown node: 0 rows, skipped
        }
        tx.commit().map_err(e)
    }
    /// All token hashes with their user (for replication): (sha256 hex, user).
    pub fn tokens(&self) -> Vec<(String, String)> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT sha256,user FROM tokens ORDER BY sha256") else { return vec![] };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|m| m.flatten().collect()).unwrap_or_default()
    }
    /// All node keys (for replication): (node_id, key).
    pub fn node_keys(&self) -> Vec<(String, Vec<u8>)> {
        let c = self.db();
        let Ok(mut st) = c.prepare("SELECT node_id,key FROM nodes WHERE key IS NOT NULL ORDER BY node_id") else { return vec![] };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|m| m.flatten().collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests_m4 {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-store-{tag}-{}-{}", std::process::id(), crate::jobs::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn spec(n: &str) -> JobSpec {
        JobSpec { name: n.into(), end: 10, chunk: 5, items: vec!["a".into(), "b".into()], ..Default::default() }
    }

    #[test]
    fn meta_round_trip() {
        let s = Store::memory();
        assert_eq!(s.meta("epoch"), None);
        s.set_meta("epoch", "3").unwrap();
        s.set_meta("epoch", "4").unwrap();
        assert_eq!(s.meta("epoch").as_deref(), Some("4"));
    }

    #[test]
    fn insert_with_id_and_continuity() {
        let s = Store::memory();
        assert!(s.insert_job_with_id(7, &spec("a"), 5).unwrap());
        assert!(!s.insert_job_with_id(7, &spec("dup"), 6).unwrap());
        assert_eq!((s.job(7).unwrap().name.as_str(), s.job(7).unwrap().created_ms), ("a", 5));
        assert_eq!(s.items(7, 0, 9), vec!["a", "b"]);
        assert_eq!(s.create_job(&spec("next"), 9).unwrap().id, 8);
        assert!(s.insert_job_with_id(3, &spec("low"), 1).unwrap()); // lower id later: sequence stays above
        assert_eq!(s.create_job(&spec("n2"), 9).unwrap().id, 9);
        let bad = JobSpec { items: vec!["x".repeat(5000)], ..spec("bad") };
        assert!(s.insert_job_with_id(50, &bad, 1).is_err() && s.job(50).is_none());
    }

    #[test]
    fn auth_replace_and_read() {
        let s = Store::memory();
        s.create_user("old", "h0", "admin", 1).unwrap();
        s.add_token("t_old", "old", 1).unwrap();
        s.upsert_node("n1", "h", "a", "{}", 1).unwrap();
        let u = |n: &str, h: &str| UserRow { name: n.into(), role: "viewer".into(), pw_hash: h.into(), created_ms: 9 };
        s.replace_auth(&[u("old", "h1"), u("new", "h2")], &[("t1".into(), "new".into())], &[("n1".into(), vec![1, 2]), ("ghost".into(), vec![3])]).unwrap();
        assert_eq!(s.tokens(), vec![("t1".to_string(), "new".to_string())]);
        assert_eq!(s.user("old").unwrap().pw_hash, "h1");
        assert!(s.user("new").is_some());
        assert_eq!(s.node_keys(), vec![("n1".to_string(), vec![1u8, 2])]);
        // a failing replace (token for unknown user) rolls back
        assert!(s.replace_auth(&[], &[("t2".into(), "nobody".into())], &[]).is_err());
        assert_eq!(s.tokens().len(), 1);
    }

    #[test]
    fn snapshot_round_trip() {
        let s = Store::memory();
        let j = s.create_job(&spec("j"), 1).unwrap();
        s.add_part(j.id, 0, 5, b"body").unwrap();
        s.upsert_node("n1", "h", "a", "{}", 1).unwrap();
        s.set_node_key("n1", &[9, 9]).unwrap();
        s.create_user("u", "h", "admin", 1).unwrap();
        s.add_token("t", "u", 1).unwrap();
        s.set_meta("epoch", "2").unwrap();
        let dir = tmp("snap");
        let p = dir.join("snap.db");
        s.snapshot_to(&p).unwrap();
        s.snapshot_to(&p).unwrap(); // overwrites an existing file
        let c = Store::open(&p).unwrap();
        assert_eq!(c.jobs(10, None), s.jobs(10, None));
        assert_eq!(c.parts(j.id), s.parts(j.id));
        assert_eq!(c.nodes(), s.nodes());
        assert_eq!(c.users(), s.users());
        assert_eq!((c.tokens(), c.node_keys(), c.items(j.id, 0, 9), c.meta("epoch")), (s.tokens(), s.node_keys(), s.items(j.id, 0, 9), Some("2".into())));
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_5_from_v4() {
        let dir = tmp("m5");
        let path = dir.join("head.db");
        {
            let c = Connection::open(&path).unwrap();
            for m in &MIGRATIONS[..4] {
                c.execute_batch(m).unwrap();
            }
            c.execute_batch("PRAGMA user_version = 4").unwrap();
            c.execute("INSERT INTO nodes (node_id,host,addr,inventory,first_seen_ms,last_seen_ms) VALUES ('n','h','a','{}',1,1)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.meta("k"), None);
        s.set_meta("k", "v").unwrap();
        assert_eq!((s.meta("k").as_deref(), s.nodes().len()), (Some("v"), 1));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn job_max_pcs_placement_mem_survive_reopen_and_replay() {
        let dir = tmp("m6");
        let path = dir.join("head.db");
        let sp = JobSpec { name: "p".into(), end: 4, chunk: 2, max_pcs: 7, placement: "spread".into(), mem_mb: 512, ..Default::default() };
        {
            let s = Store::open(&path).unwrap();
            let j = s.create_job(&sp, 1).unwrap();
            assert!(s.insert_job_with_id(50, &sp, 1).unwrap());
            drop(s);
            let s = Store::open(&path).unwrap();
            for id in [j.id, 50] {
                let b = s.spec(id).unwrap();
                assert_eq!((b.max_pcs, b.placement.as_str(), b.mem_mb), (7, "spread", 512));
                assert_eq!(s.job_limits(id), (7, "spread".to_string(), 512));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod done_in_tests {
    use super::*;
    #[test]
    fn done_in_matches_done_ranges_window() {
        let s = Store::memory();
        for (a, b) in [(0, 10), (10, 20), (30, 40), (50, 60)] {
            s.add_part(1, a, b, b"x").unwrap();
        }
        assert_eq!(s.done_in(1, 5, 15), vec![(0, 20)]);
        assert_eq!(s.done_in(1, 20, 30), vec![]);
        assert_eq!(s.done_in(1, 25, 55), vec![(30, 40), (50, 60)]);
        assert_eq!(s.done_in(2, 0, 100), vec![]);
    }
}

// ---- wave 12: alerts, experiments, samples, per-job time breakdown
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AlertRow {
    pub id: u64,
    pub kind: String,
    pub severity: String,
    pub node_id: Option<String>,
    pub detail: String,
    pub since_ms: i64,
    pub last_ms: i64,
    pub active: bool,
    pub acked_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ExperimentRow {
    pub id: u64,
    pub name: String,
    pub submitter: String,
    pub created_ms: i64,
    pub configs: u64,
    pub repeats: u64,
    pub r#virtual: bool,
    pub commit: String,
    pub pcs_max: u64,
}

/// One per-minute sample of the fleet.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub ts_ms: i64,
    pub online: u32,
    pub busy: u32,
    pub rate: f64,
    pub energy_j: f64,
}

const ALERT_SEL: &str = "SELECT id,kind,severity,node_id,detail,since_ms,last_ms,active,acked_by FROM alerts";
const EXP_SEL: &str = "SELECT id,name,submitter,created_ms,configs,repeats,virtual,commit_hash,pcs_max FROM experiments";

fn alert_row(r: &Row) -> rusqlite::Result<AlertRow> {
    Ok(AlertRow { id: r.get::<_, i64>(0)? as u64, kind: r.get(1)?, severity: r.get(2)?, node_id: r.get(3)?, detail: r.get(4)?, since_ms: r.get(5)?, last_ms: r.get(6)?, active: r.get::<_, i64>(7)? != 0, acked_by: r.get(8)? })
}

fn exp_row(r: &Row) -> rusqlite::Result<ExperimentRow> {
    Ok(ExperimentRow { id: r.get::<_, i64>(0)? as u64, name: r.get(1)?, submitter: r.get(2)?, created_ms: r.get(3)?, configs: r.get::<_, i64>(4)? as u64, repeats: r.get::<_, i64>(5)? as u64,
        r#virtual: r.get::<_, i64>(6)? != 0, commit: r.get(7)?, pcs_max: r.get::<_, i64>(8)? as u64 })
}

impl Store {
    /// New active alert; fails if one with the same kind and node is active already (unique index).
    pub fn alert_open(&self, kind: &str, severity: &str, node: Option<&str>, detail: &str, now_ms: i64) -> Result<AlertRow, String> {
        let c = self.db();
        c.execute("INSERT INTO alerts (kind,severity,node_id,detail,since_ms,last_ms) VALUES (?,?,?,?,?,?)", params![kind, severity, node, detail, now_ms, now_ms]).map_err(e)?;
        Ok(AlertRow { id: c.last_insert_rowid() as u64, kind: kind.into(), severity: severity.into(), node_id: node.map(String::from), detail: detail.into(), since_ms: now_ms, last_ms: now_ms, active: true, acked_by: None })
    }
    pub fn alert_touch(&self, id: u64, detail: &str, now_ms: i64) -> Option<AlertRow> {
        self.db().execute("UPDATE alerts SET detail=?, last_ms=? WHERE id=?", params![detail, now_ms, id as i64]).ok()?;
        self.alert(id)
    }
    pub fn alert_close(&self, id: u64, now_ms: i64) -> Option<AlertRow> {
        self.db().execute("UPDATE alerts SET active=0, last_ms=? WHERE id=?", params![now_ms, id as i64]).ok()?;
        self.alert(id)
    }
    /// After a head start nothing is being watched yet: alerts left active by a previous run are closed.
    pub fn alerts_close_stale(&self, now_ms: i64) {
        let _ = self.db().execute("UPDATE alerts SET active=0, last_ms=? WHERE active=1", [now_ms]);
    }
    pub fn alert_ack(&self, id: u64, user: &str) -> Option<AlertRow> {
        self.db().execute("UPDATE alerts SET acked_by=? WHERE id=?", params![user, id as i64]).ok()?;
        self.alert(id)
    }
    pub fn alert(&self, id: u64) -> Option<AlertRow> {
        self.db().query_row(&format!("{ALERT_SEL} WHERE id=?"), [id as i64], alert_row).optional().ok().flatten()
    }
    /// Newest first.
    pub fn alerts(&self, active_only: bool, limit: usize) -> Vec<AlertRow> {
        let c = self.db();
        let sql = format!("{ALERT_SEL} {} ORDER BY id DESC LIMIT ?", if active_only { "WHERE active=1" } else { "" });
        let Ok(mut s) = c.prepare(&sql) else { return vec![] };
        s.query_map([limit as i64], alert_row).map(|r| r.flatten().collect()).unwrap_or_default()
    }
    pub fn alerts_opened_between(&self, a: i64, b: i64) -> u64 {
        self.db().query_row("SELECT COUNT(*) FROM alerts WHERE since_ms>=? AND since_ms<?", [a, b], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    pub fn add_experiment(&self, row: &ExperimentRow, raw_csv: &str, runs_jsonl: &str, sim: Option<&str>) -> Result<ExperimentRow, String> {
        let c = self.db();
        c.execute("INSERT INTO experiments (name,submitter,created_ms,configs,repeats,virtual,commit_hash,pcs_max,raw_csv,runs_jsonl,sim) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![row.name, row.submitter, row.created_ms, row.configs as i64, row.repeats as i64, row.r#virtual as i64, row.commit, row.pcs_max as i64, raw_csv, runs_jsonl, sim]).map_err(e)?;
        Ok(ExperimentRow { id: c.last_insert_rowid() as u64, ..row.clone() })
    }
    pub fn experiments(&self) -> Vec<ExperimentRow> {
        let c = self.db();
        let Ok(mut s) = c.prepare(&format!("{EXP_SEL} ORDER BY id DESC")) else { return vec![] };
        s.query_map([], exp_row).map(|r| r.flatten().collect()).unwrap_or_default()
    }
    /// The row and its files (raw.csv, runs.jsonl, sim).
    pub fn experiment(&self, id: u64) -> Option<(ExperimentRow, String, String, Option<String>)> {
        let c = self.db();
        let row = c.query_row(&format!("{EXP_SEL} WHERE id=?"), [id as i64], exp_row).optional().ok()??;
        let (a, b, s) = c.query_row("SELECT raw_csv,runs_jsonl,sim FROM experiments WHERE id=?", [id as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()?;
        Some((row, a, b, s))
    }

    /// Per-minute fleet samples; rows older than 48 h are dropped on every insert.
    pub fn add_sample(&self, s: &Sample) -> Result<(), String> {
        let c = self.db();
        c.execute("INSERT OR REPLACE INTO samples (ts_ms,online,busy,rate,energy_j) VALUES (?,?,?,?,?)", params![s.ts_ms, s.online, s.busy, s.rate, s.energy_j]).map_err(e)?;
        c.execute("DELETE FROM samples WHERE ts_ms < ?", [s.ts_ms - 48 * 3_600_000]).map_err(e)?;
        Ok(())
    }
    pub fn samples_between(&self, a: i64, b: i64) -> Vec<Sample> {
        let c = self.db();
        let Ok(mut s) = c.prepare("SELECT ts_ms,online,busy,rate,energy_j FROM samples WHERE ts_ms>=? AND ts_ms<? ORDER BY ts_ms") else { return vec![] };
        s.query_map([a, b], |r| Ok(Sample { ts_ms: r.get(0)?, online: r.get(1)?, busy: r.get(2)?, rate: r.get(3)?, energy_j: r.get(4)? })).map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn put_job_stats(&self, job: u64, body: &str) {
        let _ = self.db().execute("INSERT OR REPLACE INTO job_stats (job_id, body) VALUES (?,?)", params![job as i64, body]);
    }
    pub fn job_stats(&self, job: u64) -> Option<String> {
        self.db().query_row("SELECT body FROM job_stats WHERE job_id=?", [job as i64], |r| r.get(0)).optional().ok().flatten()
    }
    /// Jobs that ended in [a, b): (id, name, kind code, from, end, state).
    pub fn jobs_finished_between(&self, a: i64, b: i64) -> Vec<(u64, String, u8, u64, u64, String)> {
        let c = self.db();
        let Ok(mut s) = c.prepare("SELECT id,name,kind,from_n,end_n,state FROM jobs WHERE finished_ms>=? AND finished_ms<?") else { return vec![] };
        s.query_map([a, b], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get::<_, i64>(2)? as u8, r.get::<_, i64>(3)? as u64, r.get::<_, i64>(4)? as u64, r.get(5)?))).map(|r| r.flatten().collect()).unwrap_or_default()
    }
    /// (kind, detail) of the events in [a, b), oldest first.
    pub fn events_between(&self, a: i64, b: i64) -> Vec<(String, String)> {
        let c = self.db();
        let Ok(mut s) = c.prepare("SELECT kind,detail FROM events WHERE ts_ms>=? AND ts_ms<? ORDER BY id") else { return vec![] };
        s.query_map([a, b], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.flatten().collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod wave12_tests {
    use super::*;

    #[test]
    fn migration_7_from_v6_keeps_data_and_adds_tables() {
        let dir = std::env::temp_dir().join(format!("ck-m7-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.db");
        {
            let c = Connection::open(&path).unwrap();
            for m in &MIGRATIONS[..6] {
                c.execute_batch(m).unwrap();
            }
            c.execute_batch("PRAGMA user_version = 6").unwrap();
            c.execute("INSERT INTO nodes (node_id,host,addr,inventory,first_seen_ms,last_seen_ms) VALUES ('n','h','a','{}',1,1)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.nodes().len(), 1);
        let a = s.alert_open("no_standby", "warn", None, "x", 5).unwrap();
        assert!(s.alert_open("no_standby", "warn", None, "dup", 6).is_err(), "no second active alert for the same kind and node");
        assert!(s.alert_open("slow_pc", "warn", Some("n"), "x", 6).is_ok());
        assert_eq!(s.alert_close(a.id, 9).map(|a| a.active), Some(false));
        assert!(s.alert_open("no_standby", "warn", None, "again", 10).is_ok(), "a closed alert frees the slot");
        assert_eq!((s.alerts(true, 10).len(), s.alerts(false, 10).len()), (2, 3));
        assert_eq!(s.alert_ack(a.id, "ann").unwrap().acked_by.as_deref(), Some("ann"));
        let ex = ExperimentRow { id: 0, name: "x".into(), submitter: "ann".into(), created_ms: 1, configs: 2, repeats: 3, r#virtual: true, commit: "abc".into(), pcs_max: 8 };
        let ex = s.add_experiment(&ex, "a,b\n", "{}\n", None).unwrap();
        assert_eq!(s.experiment(ex.id).map(|x| (x.0, x.1)), Some((ex.clone(), "a,b\n".to_string())));
        s.add_sample(&Sample { ts_ms: 100 * 3_600_000, online: 3, busy: 2, rate: 5.0, energy_j: 1.0 }).unwrap();
        s.add_sample(&Sample { ts_ms: 200 * 3_600_000, online: 3, busy: 2, rate: 5.0, energy_j: 1.0 }).unwrap();
        assert_eq!(s.samples_between(0, i64::MAX).len(), 1, "samples older than 48 h are dropped");
        let v: i64 = s.db().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
