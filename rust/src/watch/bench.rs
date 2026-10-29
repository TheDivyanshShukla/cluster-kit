//! Cluster benchmark: the real pipeline over chosen recordings, with the workers that are connected right now.
//!
//! A run owns throw-away sources and jobs (ids `bench-<run>-<n>...`) and its own SQLite file, so nothing it indexes
//! shows up in normal lists, searches or alerts, and deleting the run deletes its data. Every number comes from
//! what the workers counted; a run with one worker is reported as a run with one worker.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::watch::api::{Api, Reply};
use crate::watch::config::{self, Camera};
use crate::watch::domain::*;
use crate::watch::scheduler::{plan_segments, TaskSpec};
use crate::watch::{log, now_secs, repo_root};

#[derive(Clone)]
struct TaskInfo {
    task_id: String,
    key: String,
    start_s: f64,
    len_s: f64,
}

struct Run {
    id: String,
    name: String,
    state: &'static str,
    error: Option<String>,
    started: f64,
    finished: Option<f64>,
    inputs: Vec<Value>,
    repeat: u32,
    segment_secs: u32,
    jobs: Vec<String>,
    tasks: Vec<TaskInfo>,
    online_at_start: Vec<Value>,
    other_tasks_at_start: usize,
    process: Value,
    series: Vec<Value>,
    result: Option<Value>,
}

/// Runs in progress and finished this session. `Api::bench_cams` and `Api::bench_stores` are separate leaf locks so the
/// lease and index paths never wait on this one.
#[derive(Default)]
pub struct BenchState {
    runs: HashMap<String, Run>,
}

/// The run's own store for any id minted by `start` (`bench-<run>-...`).
pub fn store_of(api: &Api, id: &str) -> Option<Arc<dyn Store>> {
    api.bench_stores.lock().unwrap().get(run_of(id)?).cloned()
}

/// `bench-<run>-<n>...` -> `<run>`. Run ids contain no dash.
pub fn run_of(id: &str) -> Option<&str> {
    id.strip_prefix("bench-")?.split('-').next().filter(|r| !r.is_empty())
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new("invalid_request", m)
}

fn dir(api: &Api) -> PathBuf {
    api.data_dir.join("bench")
}

fn rel_or_abs(p: &std::path::Path) -> String {
    p.strip_prefix(repo_root()).unwrap_or(p).to_string_lossy().into_owned()
}

#[derive(serde::Deserialize)]
struct StartIn {
    #[serde(default)]
    name: String,
    media: Vec<String>,
    #[serde(default)]
    repeat: Option<u32>,
    #[serde(default)]
    segment_secs: Option<u32>,
}

/// Source id or file path -> file under the allowed footage roots.
fn resolve(item: &str) -> Result<(PathBuf, String), ApiError> {
    let cams = config::load_watch().cameras;
    let path = match cams.iter().find(|c| c.id == item && matches!(c.kind.as_str(), "file" | "archive")) {
        Some(c) => PathBuf::from(config::resolve_url(&c.url)),
        None => PathBuf::from(config::resolve_url(item)),
    };
    if !path.is_file() {
        return Err(ApiError::new("not_found", format!("recording not found: {item}")));
    }
    if !crate::watch::auth::allowed_media(&repo_root(), &path) {
        return Err(ApiError::new("forbidden", format!("{item} is outside the footage folders (vision/footage or CW_MEDIA_ROOTS)")));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((path, name))
}

pub fn start(api: &Arc<Api>, body: &[u8]) -> Result<Reply, ApiError> {
    let r: StartIn = serde_json::from_slice(body).map_err(|e| ApiError::new("parse_error", e.to_string()))?;
    if r.media.is_empty() || r.media.len() > 50 {
        return Err(bad("choose 1 to 50 recordings"));
    }
    let repeat = r.repeat.unwrap_or(1).clamp(1, 20);
    {
        let b = api.bench.lock().unwrap();
        if let Some(a) = b.runs.values().find(|x| x.state == "running") {
            return Err(ApiError::new("conflict", format!("benchmark {} is still running; wait for it or delete it", a.id)));
        }
    }
    let process = config::load_watch().process;
    let seg_secs = r.segment_secs.map(|s| if s == 0 { 0 } else { s.clamp(5, 3600) }).unwrap_or(process.segment_secs);
    let now = now_secs();
    let run_id = format!("b{:x}", (now * 1000.0) as u64);
    let name = if r.name.trim().is_empty() { format!("Benchmark {}", run_id) } else { r.name.trim().chars().take(80).collect() };
    std::fs::create_dir_all(dir(api)).map_err(|e| ApiError::new("unavailable", e.to_string()))?;
    let store = crate::watch::storage::open(&dir(api).join(format!("{run_id}.db")))?;

    let mut inputs = Vec::new();
    let mut tasks = Vec::new();
    let mut jobs = Vec::new();
    let mut cams = Vec::new();
    let mut submits = Vec::new();
    let mut n = 0usize;
    let mut copies: HashMap<PathBuf, u32> = HashMap::new();
    for _ in 0..repeat {
        for item in &r.media {
            let (path, fname) = resolve(item)?;
            let sid = format!("bench-{run_id}-{n}");
            let mut media = crate::watch::media_catalog::probe(&sid, &path)?;
            let dur = media.duration_us.filter(|d| *d > 0).ok_or_else(|| bad(format!("{fname}: cannot read its duration")))?;
            media.id = format!("{sid}-m");
            // The same file twice in one run must not collide on observation ids; a first copy keeps the plain fingerprint,
            // so a one-worker run and a four-worker run of the same file index identical observation ids.
            let copy = copies.entry(path.clone()).or_insert(0);
            if *copy > 0 {
                media.fingerprint = format!("{}#copy{}", media.fingerprint, *copy);
            }
            *copy += 1;
            let job_id = format!("{sid}-j");
            let src = Source { id: sid.clone(), name: fname.clone(), kind: SourceKind::Archive, location: rel_or_abs(&path), credential_ref: None, eligible_workers: vec![], enabled: true, capture_timezone: None };
            store.upsert_source(&src)?;
            store.upsert_media(&media)?;
            let job = IndexJob {
                id: job_id.clone(), manifest_version: "bench/1".into(), media_ids: vec![media.id.clone()], model_versions: vec![],
                sampling: serde_json::to_value(&process).unwrap_or_default(), state: JobState::Queued, checkpoint: Default::default(),
                coverage: Coverage::default(), error: None, created_utc_us: (now * 1e6) as i64, finished_utc_us: None,
            };
            store.upsert_job(&job)?;
            let seg_us = seg_secs as i64 * 1_000_000;
            let mut specs = if seg_secs > 0 && dur as f64 > seg_us as f64 * 1.5 {
                plan_segments(&job_id, &sid, &media.id, dur, seg_us, 2_000_000)
            } else {
                vec![TaskSpec { task_id: format!("{job_id}-t0"), source_id: sid.clone(), media_id: media.id.clone(), kind: SourceKind::Archive, segment: [None, None], overlap_us: 0, device: None, eligible_workers: vec![], settings: json!({}) }]
            };
            let total = specs.len();
            for (i, s) in specs.iter_mut().enumerate() {
                s.task_id = format!("{job_id}-t{i}");
                let key = if total > 1 { format!("{sid}~{i}") } else { sid.clone() };
                s.settings = json!({ "camera_id": key, "seg": i, "segments": total, "fingerprint": media.fingerprint, "bench": run_id });
                let start_s = s.segment[0].unwrap_or(0) as f64 / 1e6;
                let end_s = s.segment[1].map(|e| e as f64 / 1e6).unwrap_or(dur as f64 / 1e6);
                tasks.push(TaskInfo { task_id: s.task_id.clone(), key, start_s, len_s: (end_s - start_s).max(0.001) });
            }
            inputs.push(json!({ "source_id": sid, "name": fname, "path": rel_or_abs(&path), "duration_s": dur as f64 / 1e6, "segments": total, "size": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) }));
            cams.push(Camera { id: sid.clone(), name: fname, url: rel_or_abs(&path), start: 0.0, kind: "archive".into(), zones: vec![], analyze: true });
            jobs.push(job_id.clone());
            submits.push((job, specs));
            n += 1;
        }
    }
    let online: Vec<Value> = {
        let now = now_secs();
        let f = api.fleet.lock().unwrap();
        let mut v: Vec<Value> = f.nodes.values().filter(|x| now - x.seen < crate::watch::fleet::LOST_AFTER).map(|x| json!({ "id": x.id, "host": x.host, "ip": x.ip, "os": x.os, "arch": x.arch, "cores": x.cores })).collect();
        v.sort_by_key(|x| x["id"].as_str().unwrap_or("").to_string());
        v
    };
    if online.is_empty() {
        return Err(ApiError::new("unavailable", "no worker is connected; start a worker (the head starts one itself unless --no-local-worker) and try again"));
    }
    let other = api.sched.lock().unwrap().tasks().iter().filter(|t| t.state == "leased").count();
    {
        {
            let mut c = api.bench_cams.lock().unwrap();
            for cam in cams {
                c.insert(cam.id.clone(), cam);
            }
        }
        api.bench_stores.lock().unwrap().insert(run_id.clone(), store);
        let mut b = api.bench.lock().unwrap();
        let run = Run {
            id: run_id.clone(), name: name.clone(), state: "running", error: None, started: now, finished: None, inputs, repeat, segment_secs: seg_secs, jobs, tasks,
            online_at_start: online, other_tasks_at_start: other, process: serde_json::to_value(&process).unwrap_or_default(), series: vec![], result: None,
        };
        b.runs.insert(run_id.clone(), run);
    }
    {
        let mut s = api.sched.lock().unwrap();
        for (job, specs) in submits {
            s.submit(job, specs)?;
        }
    }
    crate::watch::api::audit(api, None, "benchmark.start", &[&run_id], "ok", &format!("{} recording(s), segments of {} s", n, seg_secs));
    log(&format!("benchmark {run_id} started: {n} recording(s), segments of {seg_secs} s"));
    let a2 = api.clone();
    let rid = run_id.clone();
    std::thread::spawn(move || sampler(a2, rid));
    let b = api.bench.lock().unwrap();
    Ok(Reply::json(&view(api, &b.runs[&run_id], false)))
}

/// Media seconds a task has covered, from what its worker last reported.
fn task_done_s(f: &crate::watch::fleet::Fleet, t: &TaskInfo, scheduler_done: bool) -> (f64, Option<String>) {
    if scheduler_done {
        let w = f.nodes.values().find(|n| n.cams.contains_key(&t.key)).map(|n| n.id.clone());
        return (t.len_s, w);
    }
    let mut best = (0.0f64, None);
    for n in f.nodes.values() {
        if let Some(c) = n.cams.get(&t.key) {
            let d = (c.at - t.start_s).clamp(0.0, t.len_s);
            if d >= best.0 {
                best = (d, Some(n.id.clone()));
            }
        }
    }
    best
}

fn sampler(api: Arc<Api>, run_id: String) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = now_secs();
        let (tasks, jobs, started) = {
            let b = api.bench.lock().unwrap();
            match b.runs.get(&run_id) {
                Some(r) if r.state == "running" => (r.tasks.clone(), r.jobs.clone(), r.started),
                _ => return,
            }
        };
        let (states, task_state): (Vec<Option<JobState>>, HashMap<String, (&'static str, Option<ApiError>)>) = {
            let s = api.sched.lock().unwrap();
            (jobs.iter().map(|j| s.job_state(j)).collect(), s.tasks().into_iter().map(|t| (t.task_id, (t.state, t.error))).collect())
        };
        let mut per: BTreeMap<String, f64> = BTreeMap::new();
        let mut total = 0.0;
        {
            let f = api.fleet.lock().unwrap();
            for t in &tasks {
                let done = task_state.get(&t.task_id).is_some_and(|s| s.0 == "done");
                let (d, w) = task_done_s(&f, t, done);
                total += d;
                if let Some(w) = w {
                    *per.entry(w).or_default() += d;
                }
            }
        }
        let finished = states.iter().all(|s| matches!(s, Some(JobState::Completed) | Some(JobState::Failed)));
        let failed = task_state.iter().find_map(|(_, (s, e))| (*s == "failed").then(|| e.clone())).flatten();
        let mut b = api.bench.lock().unwrap();
        let Some(run) = b.runs.get_mut(&run_id) else { return };
        if run.series.len() < 7200 {
            run.series.push(json!({ "t": round(now - started), "done_s": round(total), "workers": per }));
        }
        if finished {
            run.finished = Some(now);
            run.state = if failed.is_some() { "failed" } else { "done" };
            run.error = failed.map(|e| e.message);
            let result = finalize(&api, run);
            run.result = Some(result.clone());
            let _ = std::fs::write(dir(&api).join(format!("{run_id}.json")), serde_json::to_vec_pretty(&result).unwrap_or_default());
            log(&format!("benchmark {run_id} {}: {:.1} s", run.state, now - started));
            return;
        }
    }
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Final numbers: counters from every worker that touched one of the run's cameras.
fn finalize(api: &Api, run: &Run) -> Value {
    let wall = run.finished.unwrap_or_else(now_secs) - run.started;
    let prefix = format!("bench-{}-", run.id);
    let media_s: f64 = run.tasks.iter().map(|t| t.len_s).sum();
    let f = api.fleet.lock().unwrap();
    let (mut dec, mut inf, mut skip, mut fail, mut passes, mut pix, mut pixfull, mut runs) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut workers = Vec::new();
    let mut ids: Vec<&String> = f.nodes.keys().collect();
    ids.sort();
    for id in ids {
        let n = &f.nodes[id];
        let mine: Vec<(&String, &crate::watch::fleet::CamStat)> = n.cams.iter().filter(|(k, c)| k.starts_with(&prefix) && c.source_id.starts_with(&prefix)).collect();
        if mine.is_empty() {
            continue;
        }
        let g = |f: &dyn Fn(&crate::watch::fleet::CamStat) -> u64| mine.iter().map(|(_, c)| f(c)).sum::<u64>();
        let (d, i, s, fl, p, px, pf, r) = (g(&|c| c.decoded), g(&|c| c.inferred), g(&|c| c.skipped), g(&|c| c.failed), g(&|c| c.passes), g(&|c| c.pixels_inferred), g(&|c| c.pixels_full), g(&|c| c.model_runs));
        let covered: f64 = run.tasks.iter().filter(|t| n.cams.contains_key(&t.key)).map(|t| (n.cams[&t.key].at - t.start_s).clamp(0.0, t.len_s)).sum();
        let first = mine.iter().map(|(_, c)| c.started).fold(f64::MAX, f64::min);
        let last = mine.iter().map(|(_, c)| c.seen).fold(0.0, f64::max);
        let active = (last - first).max(1.0);
        dec += d; inf += i; skip += s; fail += fl; passes += p; pix += px; pixfull += pf; runs += r;
        workers.push(json!({
            "id": n.id, "host": n.host, "ip": n.ip, "os": n.os, "arch": n.arch, "cores": n.cores, "tasks": mine.len(),
            "decoded": d, "inferred": i, "skipped": s, "failed": fl, "detector_passes": p, "pixels_inferred": px, "model_runs": r,
            "media_s": round(covered), "share": round(covered / media_s.max(0.001)), "active_s": round(active), "inferred_fps": round(i as f64 / active), "decoded_fps": round(d as f64 / active),
        }));
    }
    // Which PC ran which part, with what it counted. A part that moved (a worker died or a PC joined late) shows both PCs.
    let parts: Vec<Value> = run.tasks.iter().map(|t| {
        let by: Vec<Value> = f.nodes.values().filter_map(|n| n.cams.get(&t.key).map(|c| json!({ "worker": n.id, "decoded": c.decoded, "inferred": c.inferred, "skipped": c.skipped, "failed": c.failed, "at_s": round(c.at), "state": c.state }))).collect();
        json!({ "key": t.key, "start_s": round(t.start_s), "len_s": round(t.len_s), "workers": by })
    }).collect();
    // Two workers on one machine share its CPU. Same IP or same host name = same machine.
    let machines: std::collections::HashSet<String> = workers.iter().map(|w| { let ip = w["ip"].as_str().unwrap_or(""); if ip.is_empty() { w["host"].as_str().unwrap_or("").to_string() } else { ip.to_string() } }).collect();
    let sig = format!("{}|{}x|seg{}|{}", run.inputs.iter().map(|i| format!("{}:{}", i["path"].as_str().unwrap_or(""), i["size"])).collect::<Vec<_>>().join(","), run.repeat, run.segment_secs, run.process);
    json!({
        "id": run.id, "name": run.name, "state": run.state, "error": run.error, "started": run.started, "finished": run.finished,
        "wall_s": round(wall), "media_s": round(media_s), "speed_x": round(media_s / wall.max(0.001)),
        "frames": { "decoded": dec, "inferred": inf, "skipped": skip, "failed": fail }, "detector_passes": passes, "model_runs": runs,
        "pixels_inferred": pix, "pixels_full": pixfull,
        "workers": workers, "worker_count": workers.len(), "machines": machines.len(), "same_machine": workers.len() > machines.len(),
        "parts": parts, "online_at_start": run.online_at_start, "other_tasks_at_start": run.other_tasks_at_start,
        "inputs": run.inputs, "repeat": run.repeat, "segment_secs": run.segment_secs, "tasks": run.tasks.len(),
        "process": run.process, "signature": short_hash(&sig), "series": run.series,
    })
}

fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes()).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// A run as the API shows it: final result when finished, live progress while running.
fn view(api: &Api, run: &Run, full: bool) -> Value {
    if let Some(r) = &run.result {
        let mut r = r.clone();
        if !full {
            r.as_object_mut().map(|o| o.remove("series"));
        }
        return r;
    }
    let now = now_secs();
    let task_state: HashMap<String, &'static str> = api.sched.lock().unwrap().tasks().into_iter().map(|t| (t.task_id, t.state)).collect();
    let f = api.fleet.lock().unwrap();
    let media_s: f64 = run.tasks.iter().map(|t| t.len_s).sum();
    let mut per: BTreeMap<String, f64> = BTreeMap::new();
    let mut total = 0.0;
    let mut done_tasks = 0;
    for t in &run.tasks {
        let done = task_state.get(&t.task_id) == Some(&"done");
        done_tasks += done as usize;
        let (d, w) = task_done_s(&f, t, done);
        total += d;
        if let Some(w) = w {
            *per.entry(w).or_default() += d;
        }
    }
    let elapsed = now - run.started;
    let p = total / media_s.max(0.001);
    let workers: Vec<Value> = per.iter().map(|(id, d)| {
        let n = f.nodes.get(id);
        json!({ "id": id, "host": n.map(|n| n.host.clone()), "media_s": round(*d), "share": round(*d / media_s.max(0.001)), "state": n.map(|n| n.state(now)) })
    }).collect();
    let mut out = json!({
        "id": run.id, "name": run.name, "state": run.state, "error": run.error, "started": run.started, "elapsed_s": round(elapsed),
        "progress": round(p), "eta_s": (p > 0.02).then(|| round(elapsed * (1.0 - p) / p)),
        "media_s": round(media_s), "tasks": run.tasks.len(), "tasks_done": done_tasks, "workers": workers, "online_at_start": run.online_at_start,
        "inputs": run.inputs, "repeat": run.repeat, "segment_secs": run.segment_secs,
    });
    if full {
        out["series"] = json!(run.series);
    }
    out
}

fn load_saved(api: &Api) -> Vec<Value> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir(api)).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == "json") {
            if let Some(v) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                out.push(v);
            }
        }
    }
    out
}

/// Finished runs, newest first, each compared with the best run of the same recordings and settings that used
/// one worker. Only runs that really had one worker can be a baseline.
fn listing(api: &Api, with_series: bool) -> Vec<Value> {
    let mut runs: Vec<Value> = load_saved(api);
    {
        let b = api.bench.lock().unwrap();
        let live: Vec<Value> = b.runs.values().filter(|r| r.result.is_none()).map(|r| view(api, r, false)).collect();
        runs.retain(|v| !live.iter().any(|l| l["id"] == v["id"]));
        runs.extend(live);
    }
    runs.sort_by(|a, b| b["started"].as_f64().partial_cmp(&a["started"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    let base: HashMap<String, f64> = {
        let mut m: HashMap<String, f64> = HashMap::new();
        for r in runs.iter().filter(|r| r["state"] == "done" && r["worker_count"] == 1) {
            let (sig, w) = (r["signature"].as_str().unwrap_or("").to_string(), r["wall_s"].as_f64().unwrap_or(0.0));
            if w > 0.0 {
                let e = m.entry(sig).or_insert(w);
                *e = e.min(w);
            }
        }
        m
    };
    for r in runs.iter_mut() {
        if r["state"] != "done" {
            continue;
        }
        let sig = r["signature"].as_str().unwrap_or("").to_string();
        let (wall, n) = (r["wall_s"].as_f64().unwrap_or(0.0), r["worker_count"].as_f64().unwrap_or(1.0).max(1.0));
        if let (Some(b), true) = (base.get(&sig), wall > 0.0) {
            r["baseline_wall_s"] = json!(b);
            r["speedup"] = json!(round(b / wall));
            r["efficiency"] = json!(round(b / wall / n));
        }
        if !with_series {
            r.as_object_mut().map(|o| o.remove("series"));
        }
    }
    runs
}

/// Recordings a benchmark can use: configured file sources, then files under vision/footage.
fn candidates() -> Value {
    let root = repo_root();
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for c in config::load_watch().cameras.iter().filter(|c| matches!(c.kind.as_str(), "file" | "archive")) {
        let p = PathBuf::from(config::resolve_url(&c.url));
        if p.is_file() && seen.insert(p.clone()) {
            out.push(json!({ "id": c.id, "name": c.name, "size": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), "kind": "source" }));
        }
    }
    let mut stack = vec![root.join("vision/footage")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() && !p.file_name().is_some_and(|n| n == "raw") {
                stack.push(p);
            } else if crate::watch::ingest::classify(&p) == Some(crate::watch::ingest::MediaClass::Video) && seen.insert(p.clone()) && out.len() < 300 {
                out.push(json!({ "id": rel_or_abs(&p), "name": p.file_name().map(|n| n.to_string_lossy().into_owned()), "size": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), "kind": "file" }));
            }
        }
    }
    json!({ "media": out })
}

fn delete(api: &Api, id: &str) -> Result<Reply, ApiError> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(bad("bad benchmark id"));
    }
    let mut b = api.bench.lock().unwrap();
    let had_run = b.runs.remove(id);
    if let Some(r) = &had_run {
        let mut s = api.sched.lock().unwrap();
        for j in &r.jobs {
            s.remove_job(j);
        }
        api.bench_cams.lock().unwrap().retain(|k, _| !k.starts_with(&format!("bench-{id}-")));
    }
    api.bench_stores.lock().unwrap().remove(id);
    drop(b);
    let d = dir(api);
    let gone = ["json", "db", "db-wal", "db-shm"].iter().filter(|e| std::fs::remove_file(d.join(format!("{id}.{e}"))).is_ok()).count();
    if had_run.is_none() && gone == 0 {
        return Err(ApiError::new("not_found", "no such benchmark"));
    }
    crate::watch::api::audit(api, None, "benchmark.delete", &[id], "ok", "run and its data removed");
    Ok(Reply::json(&json!({ "deleted": id })))
}

/// `/api/v1/benchmarks...`
pub fn handle(api: &Arc<Api>, method: &str, url: &str, body: &[u8]) -> Option<Reply> {
    let path = url.split('?').next().unwrap_or(url);
    let rest = path.strip_prefix("/api/v1/benchmarks")?.trim_matches('/');
    let r: Result<Reply, ApiError> = match (method, rest) {
        ("POST", "") => start(api, body),
        ("GET", "") => {
            let runs = listing(api, false);
            Ok(Reply::json(&json!({ "runs": runs, "running": runs.iter().find(|r| r["state"] == "running") })))
        }
        ("GET", "media") => Ok(Reply::json(&candidates())),
        ("GET", id) => listing(api, true).into_iter().find(|r| r["id"] == id).map(|r| Reply::json(&r)).ok_or_else(|| ApiError::new("not_found", "no such benchmark")),
        ("DELETE", id) if !id.is_empty() => delete(api, id),
        _ => return None,
    };
    Some(r.unwrap_or_else(Reply::err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_map_back_to_their_run() {
        assert_eq!(run_of("bench-b19a3-0"), Some("b19a3"));
        assert_eq!(run_of("bench-b19a3-0~3"), Some("b19a3"));
        assert_eq!(run_of("bench-b19a3-2-j"), Some("b19a3"));
        assert_eq!(run_of("street"), None);
        assert_eq!(run_of("bench-"), None);
    }

    #[test]
    fn speedup_is_only_against_a_real_one_worker_run() {
        let api = crate::watch::api::test_api();
        std::fs::create_dir_all(dir(&api)).unwrap();
        let mk = |id: &str, n: u64, wall: f64, started: f64| json!({"id": id, "state": "done", "worker_count": n, "wall_s": wall, "started": started, "signature": "s1", "series": []});
        for v in [mk("a", 1, 100.0, 1.0), mk("b", 2, 55.0, 2.0), mk("c", 4, 30.0, 3.0)] {
            std::fs::write(dir(&api).join(format!("{}.json", v["id"].as_str().unwrap())), v.to_string()).unwrap();
        }
        let runs = listing(&api, false);
        let by = |id: &str| runs.iter().find(|r| r["id"] == id).unwrap().clone();
        assert_eq!(by("a")["speedup"], 1.0);
        assert_eq!(by("b")["speedup"], 1.82);
        assert_eq!(by("b")["efficiency"], 0.91);
        assert_eq!(by("c")["efficiency"], 0.83);
        // Without a 1-worker run there is nothing to compare against, and nothing is invented.
        std::fs::remove_file(dir(&api).join("a.json")).unwrap();
        assert!(listing(&api, false).iter().all(|r| r.get("speedup").is_none()));
    }

    #[test]
    fn a_missing_recording_or_one_outside_the_footage_folders_is_refused() {
        assert_eq!(resolve("no/such/file.mp4").unwrap_err().code, "not_found");
        assert_eq!(resolve("../Cargo.toml").unwrap_err().code, "not_found");
    }
}
