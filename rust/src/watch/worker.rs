//! One process per PC. The reader gates every frame. The detector thread takes the
//! latest sharp frame. Rules run only on frames the detector actually saw.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::watch::config::{self, Camera, Module, Process, Rule};
use crate::watch::decode::{self, DecodeEnd, Feed};
use crate::watch::domain::{ApiError, IndexBatch, LeaseRef, TaskLease};
use crate::watch::ingest::{self, CoverageTracker, DetIn, FrameIn};
use crate::watch::scheduler::WorkerCaps;
use crate::watch::fight::{self, union};
use crate::watch::gate::{self, Gray, Motion, MotionModel, MotionParams};
use pipe::{Cost, Last, Pipe, Plan};
use crate::watch::rules::{self, Compiled, Streak};
use crate::watch::yolo::Engine;
use crate::watch::{log, now_secs, primary_ip, repo_root, Opts};

mod pipe;

struct Slot {
    rgb: Vec<u8>,
    w: u32,
    h: u32,
    gray: Gray,
    prev: Vec<u8>,
    at: f64,
    ts: f64,
    seq: u64,
    archive: bool,
    queued: Instant,
    /// False inside a segment's overlap: decoded and tracked for warm-up, never indexed.
    own: bool,
    /// Motion on this frame against the slow background (motion mode); `cut` set when the scene changed.
    motion: Motion,
}

/// Cumulative per-camera operation counts, so the UI can show what the motion pipeline saved.
/// `pixels_*` are source pixels shown to the detector; `model_runs` is the number of detector executions
/// (each crop is one run), the honest cost figure.
#[derive(Clone, Default)]
struct Ops {
    /// Sampled frames with no motion and nothing tracked: detector skipped, not indexed, counted analyzed.
    static_frames: u64,
    /// Sampled frames between detector passes: tracker prediction drawn, not indexed, counted analyzed.
    tracked_frames: u64,
    /// Scheduled/forced full-picture passes (start, scene cut, `keyframe_secs`).
    keyframes: u64,
    /// Motion-mode frames that ran one full picture because the crops were too many or too large.
    full_fallbacks: u64,
    /// Frames the detector ran on crops, and the crops cut in total.
    region_frames: u64,
    regions: u64,
    /// Frames the detector ran on at all (all of the above that ran it, and every frame in `full` mode).
    detector_passes: u64,
    model_runs: u64,
    pixels_inferred: u64,
    /// What a classic whole-picture pass on every sampled frame would have shown the detector.
    pixels_full: u64,
}

/// Live queue depth. Archives use depth 1: the reader blocks until the frame is acknowledged.
const LIVE_QUEUE: usize = 2;

struct CamLive {
    name: String,
    url: String,
    gated: u32,
    blur: u32,
    blank: u32,
    missed: u32,
    same: u32,
    paced: u32,
    passes: u32,
    detect_us: u64,
    debt: f64,
    pace: f64,
    persons: u32,
    at: f64,
    dur: f64,
    error: String,
    detector_error: String,
    queue: VecDeque<Slot>,
    seq: u64,
    boxes: serde_json::Value,
    completed: bool,
    indexed_frames: u64,
    processing: bool,
    /// Cumulative (never reset): decoded / inferred / skipped / failed frames and analyzed media intervals.
    cov: CoverageTracker,
    /// Live frames discarded because the bounded queue was full (oldest first).
    dropped: u64,
    /// Machine-readable reason behind `error` ("missing_file", "unsupported_media", "no_frames", ...).
    error_code: String,
    /// Fatal: the reader and inference stop touching this camera and the error stays visible.
    halt: bool,
    /// Media time of the last frame the head acknowledged (archives).
    checkpoint_us: i64,
    ops: Ops,
    /// The last detector pass was acknowledged by the head: static/tracked frames after it may extend coverage.
    last_acked: bool,
    ops_logged: bool,
}

impl CamLive {
    fn new(name: String, url: String) -> Self {
        CamLive {
            name, url, gated: 0, blur: 0, blank: 0, missed: 0, same: 0, paced: 0, passes: 0, detect_us: 0, debt: 0.0, pace: 25.0, persons: 0,
            at: 0.0, dur: 0.0, error: String::new(), detector_error: String::new(), queue: VecDeque::new(), seq: 0,
            boxes: serde_json::json!([]), completed: false, indexed_frames: 0, processing: false, cov: CoverageTracker::default(),
            dropped: 0, error_code: String::new(), halt: false, checkpoint_us: 0,
            ops: Ops::default(), last_acked: false, ops_logged: false,
        }
    }
}

struct Shared {
    proc: Mutex<Process>,
    modules: Mutex<Vec<Module>>,
    rules: Mutex<Vec<Rule>>,
    cams: Mutex<HashMap<String, CamLive>>,
    stop: AtomicBool,
    turn: AtomicUsize,
    /// camera id -> lease, as delivered by the head in `/api/work` (`leases`). Empty until the head issues leases.
    leases: Mutex<HashMap<String, TaskLease>>,
    /// Until this unix-ms time, skip `/api/v1/index` (the head answered 404) and use the legacy route.
    legacy_until_ms: AtomicI64,
    measured_fps: Mutex<f64>,
}

pub fn run(opts: Opts) -> Result<(), String> {
    let ip = primary_ip();
    // Two workers on one PC (tests, demos) need distinct identities: CW_WORKER_ID adds a suffix.
    let id = match std::env::var("CW_WORKER_ID") {
        Ok(x) if !x.is_empty() => format!("{}|{ip}|{x}", crate::watch::hostname()),
        _ => format!("{}|{ip}", crate::watch::hostname()),
    };
    let base = loop {
        let host = opts.head.clone().unwrap_or_else(|| crate::watch::net::discover(&opts.name));
        let url = format!("http://{host}:{}", opts.port);
        if ureq::get(&format!("{url}/api/health")).call().is_ok() {
            log(&format!("worker {id} joined {url}"));
            break url;
        }
        log("head not up yet");
        thread::sleep(Duration::from_secs(2));
    };
    crate::watch::shard::node::start(&id, &base); // CW_SHARDS=1: local shard store + shard server
    let shared = Arc::new(Shared {
        proc: Mutex::new(config::preset("balanced")),
        modules: Mutex::new(Vec::new()),
        rules: Mutex::new(Vec::new()),
        cams: Mutex::new(HashMap::new()),
        stop: AtomicBool::new(false),
        turn: AtomicUsize::new(0),
        leases: Mutex::new(HashMap::new()),
        legacy_until_ms: AtomicI64::new(0),
        measured_fps: Mutex::new(0.0),
    });
    let threads = crate::watch::detector_threads();
    let infer_shared = shared.clone();
    let infer_base = base.clone();
    thread::spawn(move || infer_loop(infer_shared, threads, infer_base));
    let stat_shared = shared.clone();
    let stat_base = base.clone();
    let stat_id = id.clone();
    let local = opts.local;
    thread::spawn(move || stats_loop(stat_shared, stat_base, stat_id, ip, local));
    let hb_shared = shared.clone();
    let hb_base = base.clone();
    let hb_id = id.clone();
    thread::spawn(move || heartbeat_loop(hb_shared, hb_base, hb_id, threads));

    let mut running: HashMap<String, (String, Arc<AtomicBool>)> = HashMap::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let work = auth(ureq::get(&format!("{base}/api/work")))
            .query("id", &id)
            .query("host", &crate::watch::hostname())
            .query("cores", &threads.to_string())
            .query("local", if opts.local { "1" } else { "0" })
            .call();
        let Ok(resp) = work else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
        if let Some(p) = v.get("process") {
            if let Ok(p) = serde_json::from_value::<Process>(p.clone()) {
                *shared.proc.lock().unwrap() = config::clamp_process(p);
            }
        }
        if let Ok(m) = serde_json::from_value::<Vec<Module>>(v.get("modules").cloned().unwrap_or(serde_json::json!([]))) {
            *shared.modules.lock().unwrap() = m;
        }
        if let Ok(r) = serde_json::from_value::<Vec<Rule>>(v.get("rules").cloned().unwrap_or(serde_json::json!([]))) {
            crate::watch::shard::node::set_rules(&r);
            *shared.rules.lock().unwrap() = r;
        }
        if let Some(l) = v.get("leases").and_then(|l| serde_json::from_value::<HashMap<String, TaskLease>>(l.clone()).ok()) {
            *shared.leases.lock().unwrap() = l;
        }
        let cams: Vec<Camera> = serde_json::from_value(v.get("cameras").cloned().unwrap_or(serde_json::json!([]))).unwrap_or_default();
        let want: HashMap<String, Camera> = cams.into_iter().map(|c| (c.id.clone(), c)).collect();
        for id in running.keys().cloned().collect::<Vec<_>>() {
            if !want.contains_key(&id) {
                if let Some((_, stop)) = running.remove(&id) {
                    stop.store(true, Ordering::Relaxed);
                }
                shared.cams.lock().unwrap().remove(&id);
            }
        }
        for (id, cam) in want {
            let lease_sig = shared.leases.lock().unwrap().get(&id).map(|l| format!("{}#{}", l.task_id, l.generation)).unwrap_or_default();
            let signature = format!("{}|{}|{}|{lease_sig}", cam.url, cam.start, cam.kind);
            if let Some((old, stop)) = running.get(&id) {
                if *old == signature { continue; }
                stop.store(true, Ordering::Relaxed);
            }
            let stop = Arc::new(AtomicBool::new(false));
            shared.cams.lock().unwrap().insert(id.clone(), CamLive::new(cam.name.clone(), cam.url.clone()));
            let shared2 = shared.clone();
            let stop2 = stop.clone();
            let cid = id.clone();
            thread::spawn(move || camera_loop(cam, cid, shared2, stop2));
            running.insert(id, (signature, stop));
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn source_width(url: &str) -> Option<u32> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width", "-of", "csv=p=0", url])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok().filter(|w| *w > 0)
}

/// Record why a camera stopped or stalled. `halt` makes the error final: no further retries of the same input.
fn set_error(shared: &Shared, id: &str, code: &str, msg: String, halt: bool) {
    if let Some(c) = shared.cams.lock().unwrap().get_mut(id) {
        c.error = msg;
        c.error_code = code.into();
        if halt {
            c.halt = true;
            c.queue.clear();
        }
    }
}

/// Sleep up to `d`, waking early when stopped. True = still running.
fn nap(stop: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(20).min(d));
    }
    !stop.load(Ordering::Relaxed)
}

/// Archive end: wait until the last queued frame is processed and acknowledged, then mark the camera complete.
fn drain_and_complete(shared: &Shared, id: &str, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut cams = shared.cams.lock().unwrap();
            let Some(c) = cams.get_mut(id) else { return };
            if c.halt {
                return;
            }
            if c.queue.is_empty() && !c.processing {
                c.completed = true;
                return;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn camera_loop(cam: Camera, id: String, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let url = config::resolve_url(&cam.url);
    let archive = matches!(cam.kind.as_str(), "archive" | "image");
    if let Err(e) = decode::preflight(&url, &cam.kind) {
        set_error(&shared, &id, e.code(), e.message(), true);
        return;
    }
    // Say up front that nothing can be indexed; cleared by the first successful inference.
    if let Err(e) = modules_ready(&repo_root(), &shared.modules.lock().unwrap()) {
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
            c.detector_error = e.message;
            c.error_code = e.code;
        }
    }
    let probed = if matches!(cam.kind.as_str(), "file" | "archive") { decode::probe(&url) } else { decode::Probe { fps: 25.0, duration: 0.0 } };
    // A lease with a segment decodes from (start - overlap) and stops at its end; the overlap is tracked, not indexed.
    let lease = shared.leases.lock().unwrap().get(&id).cloned();
    let seg_end_us = lease.as_ref().and_then(|l| l.segment[1]);
    let ss = match lease.as_ref().and_then(|l| l.segment[0]) {
        Some(start) => (start - lease.as_ref().map_or(0, |l| l.overlap_us)).max(0) as f64 / 1e6,
        None if probed.duration > 0.0 => cam.start * probed.duration,
        None => 0.0,
    };
    // Decoding wider than the source only upscales: cap at the source width when it is known (files).
    let src_w = if matches!(cam.kind.as_str(), "file" | "archive") { source_width(&url) } else { None };
    let eff_width = |p: &Process| src_w.map_or(p.decode_width(), |s| p.decode_width().min(s.max(16)));
    let started = Instant::now();
    let mut frames = 0u64;
    let mut prev: Option<Gray> = None;
    let mut last_sent: Option<Gray> = None;
    let mut eligible = 0u32;
    let mut motion_model = MotionModel::default();
    while !stop.load(Ordering::Relaxed) {
        let width = eff_width(&shared.proc.lock().unwrap());
        let mut feed = match Feed::open(&url, &cam.kind, ss, width) {
            Ok(f) => f,
            Err(e) => {
                set_error(&shared, &id, "ffmpeg_missing", format!("ffmpeg did not start ({e}). Install ffmpeg and leave it on PATH."), false);
                if !nap(&stop, Duration::from_secs(2)) {
                    return;
                }
                continue;
            }
        };
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
            c.error.clear();
            c.error_code.clear();
            c.pace = probed.fps;
        }
        loop {
            if stop.load(Ordering::Relaxed) {
                feed.cancel();
                return;
            }
            let (w, h, rgb) = match feed.next_frame() {
                Ok(v) => v,
                Err(_) => {
                    let end = feed.end(frames);
                    if archive {
                        match end {
                            DecodeEnd::Eof => drain_and_complete(&shared, &id, &stop),
                            other => set_error(&shared, &id, other.code(), other.message(), true),
                        }
                        return;
                    }
                    // Live: say why, back off, reconnect. The pause stops a dead stream from spinning FFmpeg.
                    set_error(&shared, &id, end.code(), format!("stream ended ({}); reconnecting", end.message()), false);
                    if !nap(&stop, Duration::from_secs(2)) {
                        return;
                    }
                    break;
                }
            };
            frames += 1;
            let proc = shared.proc.lock().unwrap().clone();
            if eff_width(&proc) != width {
                break;
            }
            let at = media_at(ss, frames, probed.fps, probed.duration);
            let at_us = (at * 1e6).round() as i64;
            if seg_end_us.is_some_and(|e| at_us >= e) {
                // Segment finished: stop decoding and let the queue drain.
                feed.cancel();
                drain_and_complete(&shared, &id, &stop);
                return;
            }
            // Archive ingestion must not replace an unprocessed frame with a newer one: block until the
            // previous frame is taken (and, in the infer loop, acknowledged). Live sources use a bounded queue.
            if archive {
                loop {
                    if stop.load(Ordering::Relaxed) { return; }
                    let (pending, halted) = shared.cams.lock().unwrap().get(&id).map_or((false, true), |c| (!c.queue.is_empty(), c.halt));
                    if halted { return; }
                    if !pending { break; }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            // Every decoded frame feeds the slow background, sampled or not, so drift is tracked at full rate.
            let motion = (proc.detect_mode == "motion")
                .then(|| motion_model.update(&gate::shrink_gray_box(&rgb, w, h, 256), &MotionParams::new(proc.motion_sensitivity, proc.motion_min_area)));
            let gray = gate::shrink_gray(&rgb, w, h, 160);
            let (mut g, _) = gate::judge(&gray, proc.blur);
            if g == gate::Gate::Blank && !proc.blank_on {
                g = gate::Gate::Sharp;
            }
            if g == gate::Gate::Blur && !proc.blur_on {
                g = gate::Gate::Sharp;
            }
            let send = match g {
                gate::Gate::Sharp => {
                    let diff = last_sent.as_ref().map(|p| gate::changed(p, &gray)).unwrap_or(255.0);
                    if proc.same_on && motion.is_none() && diff < proc.same {
                        None
                    } else {
                        eligible = eligible.wrapping_add(1);
                        Some(proc.every <= 1 || (eligible - 1) % proc.every == 0)
                    }
                }
                _ => None,
            };
            let debt = (started.elapsed().as_secs_f64() * probed.fps - frames as f64).max(0.0);
            {
                let mut cams = shared.cams.lock().unwrap();
                let Some(c) = cams.get_mut(&id) else { return };
                if c.halt { return; }
                c.gated += 1;
                c.cov.decoded();
                c.debt = debt;
                c.error.clear();
                c.error_code.clear();
                match g {
                    gate::Gate::Blank => { c.blank += 1; c.cov.skipped(); }
                    gate::Gate::Blur => { c.blur += 1; c.cov.skipped(); }
                    gate::Gate::Sharp => match send {
                        None => { c.same += 1; c.cov.skipped(); }
                        Some(false) => { c.paced += 1; c.cov.skipped(); }
                        Some(true) => {
                            let previous = prev.clone().map(|p| p.px).unwrap_or_else(|| gray.px.clone());
                            let own = lease.as_ref().map_or(true, |l| ingest::owns(l, at_us));
                            let slot = Slot { rgb, w, h, gray: gray.clone(), prev: previous, at, ts: now_secs(), seq: frames, archive, queued: Instant::now(), own, motion: motion.unwrap_or_default() };
                            // Bounded live queue: when full, the OLDEST frame is dropped and counted.
                            while !archive && c.queue.len() >= LIVE_QUEUE {
                                c.queue.pop_front();
                                c.missed += 1;
                                c.dropped += 1;
                                c.cov.skipped();
                            }
                            c.queue.push_back(slot);
                            last_sent = Some(gray.clone());
                        }
                    },
                }
                c.seq = frames;
                c.at = at;
                if probed.duration > 1.0 {
                    c.dur = probed.duration;
                }
            }
            prev = Some(gray);
        }
    }
}

struct Loaded {
    module: Module,
    engine: Option<Engine>,
    last_err: String,
    retry_at: Instant,
    /// Validated ModelVersion id (models::validate); the model path when validation fails.
    version: String,
}

fn infer_loop(shared: Arc<Shared>, threads: usize, base: String) {
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut pipes: HashMap<String, Pipe> = HashMap::new();
    let mut streaks: HashMap<String, Streak> = HashMap::new();
    // Live posts are best-effort and short; archive posts wait for a real acknowledgment.
    let live_agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(400)).build();
    let arch_agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        sync_engines(&shared, &mut loaded, threads);
        let job = {
            let mut cams = shared.cams.lock().unwrap();
            let mut ids: Vec<String> = cams.keys().cloned().collect();
            ids.sort();
            let mut found = None;
            if !ids.is_empty() {
                let start = shared.turn.fetch_add(1, Ordering::Relaxed);
                for step in 0..ids.len() {
                    let id = &ids[(start + step) % ids.len()];
                    let Some(cam) = cams.get_mut(id) else { continue };
                    if cam.halt { continue; }
                    if let Some(slot) = cam.queue.pop_front() {
                        cam.processing = true;
                        found = Some((id.clone(), slot, cam.name.clone()));
                        break;
                    }
                }
            }
            found
        };
        let Some((id, slot, name)) = job else {
            thread::sleep(Duration::from_millis(2));
            continue;
        };
        let t0 = Instant::now();
        let proc_now = shared.proc.lock().unwrap().clone();
        let plan = pipes.entry(id.clone()).or_insert_with(|| Pipe::new(&id)).plan(&proc_now, slot.at, &slot.motion, slot.w, slot.h);
        if !plan.runs_detector() {
            // Static or tracker-predicted frame: no detector, nothing indexed, counted as known.
            let pipe = pipes.get_mut(&id).unwrap();
            pipe.commit(&plan, slot.at);
            let view = if plan == Plan::Tracked { pipe.predicted_view(proc_now.detect_interval) } else { Vec::new() };
            let at_us = (slot.at * 1e6).round() as i64;
            if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
                c.processing = false;
                let span = (1_000_000.0 / c.pace.max(1.0)) as i64 * proc_now.every.max(1) as i64;
                if plan == Plan::Tracked { c.ops.tracked_frames += 1 } else { c.ops.static_frames += 1 }
                c.ops.pixels_full += slot.w as u64 * slot.h as u64;
                if slot.own && c.last_acked {
                    c.cov.carried(at_us, span);
                    c.checkpoint_us = at_us;
                } else {
                    c.cov.skipped();
                }
                c.persons = view.iter().filter(|b| b["class"] == "person").count() as u32;
                c.boxes = serde_json::Value::Array(view);
            }
            continue;
        }
        let rules = shared.rules.lock().unwrap().clone();
        let mut counts: HashMap<String, u32> = HashMap::new();
        let mut people = Vec::new();
        let mut detectable = Vec::new();
        let mut drawn = Vec::new();
        let mut successful_detectors = 0;
        let mut ran: Vec<String> = Vec::new();
        let mut cost = Cost::default();
        let sw = slot.w as f32;
        let sh = slot.h as f32;
        let interest = interest_of(&rules, &loaded);
        for item in loaded.iter_mut() {
            if !item.module.enabled || item.module.kind == "fight" {
                continue;
            }
            let Some(engine) = item.engine.as_mut() else { continue };
            let mode = shared.proc.lock().unwrap().mode.clone();
            let conf = conf_of(item.module.conf, &mode);
            match pipe::run_plan(&plan, &slot.rgb, slot.w, slot.h, |rgb, w, h| engine.run(rgb, w, h, conf, &item.module.labels, &interest)) {
                Ok((dets, spent)) => {
                    successful_detectors += 1;
                    cost.runs += spent.runs;
                    cost.pixels += spent.pixels;
                    ran.push(item.version.clone());
                    item.last_err.clear();
                    for label in &item.module.labels {
                        if !detectable.contains(label) {
                            detectable.push(label.clone());
                        }
                    }
                    for d in dets {
                        *counts.entry(d.class.clone()).or_insert(0) += 1;
                        if d.class == "person" {
                            people.push(d.xyxy);
                        }
                        if !detectable.contains(&d.class) {
                            detectable.push(d.class.clone());
                        }
                        if sw > 1.0 && sh > 1.0 {
                            let color = gate::color_name(&slot.rgb, slot.w, slot.h, d.xyxy);
                            drawn.push(serde_json::json!({
                                "class": d.class,
                                "confidence": d.conf,
                                "model": item.module.model,
                                "model_version": item.version,
                                "color": color,
                                "x": (d.xyxy[0] / sw).clamp(0.0, 1.0),
                                "y": (d.xyxy[1] / sh).clamp(0.0, 1.0),
                                "w": ((d.xyxy[2] - d.xyxy[0]) / sw).clamp(0.0, 1.0),
                                "h": ((d.xyxy[3] - d.xyxy[1]) / sh).clamp(0.0, 1.0),
                            }));
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if item.last_err != msg {
                        item.last_err = msg.clone();
                        log(&format!("{} detector: {msg}", item.module.name));
                    }
                }
            }
        }
        if successful_detectors == 0 {
            if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
                // Never an empty success: say which model is missing. The frame stays queued for archives.
                c.detector_error = match modules_ready(&repo_root(), &loaded.iter().map(|l| l.module.clone()).collect::<Vec<_>>()) {
                    Err(e) => e.message,
                    Ok(_) => "No object detector completed inference. Check worker logs.".into(),
                };
                c.error_code = "model_unavailable".into();
                c.processing = false;
                if slot.archive { c.queue.push_front(slot); }
            }
            thread::sleep(Duration::from_millis(250));
            continue;
        }
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) { c.detector_error.clear(); }
        let fight_on = loaded.iter().any(|m| m.module.enabled && m.module.kind == "fight");
        if fight_on {
            detectable.push("fight".into());
            ran.push("heuristic:motion".into());
            let people_boxes = people.clone();
            let gray = &slot.gray;
            let prev = &slot.prev;
            if fight::fight_frame(&people_boxes, |i, j| {
                let u = union(people_boxes[i], people_boxes[j]);
                gate::motion_box(prev, &gray.px, gray.w, gray.h, u, sw, sh)
            }) {
                counts.insert("fight".into(), 1);
                drawn.push(serde_json::json!({"class": "fight", "confidence": 0.0, "model": "heuristic:motion", "color": "", "x": 0, "y": 0, "w": 0, "h": 0}));
            }
        }
        let geometry: Vec<(String, [f32; 4])> = drawn.iter().map(|b| {
            let f = |k: &str| b[k].as_f64().unwrap_or(0.0) as f32;
            (b["class"].as_str().unwrap_or("").to_string(), [f("x"), f("y"), f("x")+f("w"), f("y")+f("h")])
        }).collect();
        // Scoped tracker numbers from 1, so a replay of the same frames yields the same track IDs.
        let carried = match &plan {
            Plan::Regions(r) if slot.own => pipes.get(&id).map(|p| p.uncovered(r, slot.w, slot.h)).unwrap_or_default(),
            _ => Vec::new(),
        };
        let ids = pipes.get_mut(&id).unwrap().tracker.update(&geometry);
        let persons_tracked: Vec<_> = geometry.iter().zip(&ids).filter(|((c, _), _)| c == "person").map(|((_, xy), tid)| (*tid, *xy)).collect();
        for (i, b) in drawn.iter_mut().enumerate() {
            b["track_id"] = serde_json::json!(ids[i]);
            if geometry[i].0 == "cell phone" {
                b["near_person"] = serde_json::json!(crate::watch::track::near_person(geometry[i].1, &persons_tracked));
                if !b["near_person"].is_null() { b["relation"] = serde_json::json!("near"); }
            }
        }
        // Region pass: objects tracked outside every crop are unknown here, not gone. Carry them with their provenance.
        // They are not fed back to the tracker, so they expire with their track instead of living on.
        for (n, l) in &carried {
            *counts.entry(l.class.clone()).or_insert(0) += 1;
            drawn.push(serde_json::json!({
                "class": l.class, "confidence": l.conf, "model": l.model, "model_version": l.model_version, "color": l.color,
                "x": l.bbox[0], "y": l.bbox[1], "w": l.bbox[2] - l.bbox[0], "h": l.bbox[3] - l.bbox[1],
                "track_id": n, "provenance": format!("tracked:{}", l.det_id),
            }));
        }
        let proc_every = shared.proc.lock().unwrap().every.max(1) as i64;
        let at_us = (slot.at * 1e6).round() as i64;
        let (pace, halted, cam_url) = shared.cams.lock().unwrap().get(&id).map_or((25.0, true, String::new()), |c| (c.pace.max(1.0), c.halt, c.url.clone()));
        if halted {
            if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) { c.processing = false; }
            continue;
        }
        let lease = shared.leases.lock().unwrap().get(&id).cloned();
        let posted = if !slot.own {
            // Segment overlap: tracker warmed above, but the previous segment owns this frame.
            Posted::Skipped
        } else {
            let thumbnail_hex: String = decode::thumbnail(&slot.rgb, slot.w, slot.h).iter().map(|b| format!("{b:02x}")).collect();
            let dets: Vec<DetIn> = drawn.iter().map(|b| {
                let f = |k: &str| b[k].as_f64().unwrap_or(0.0) as f32;
                DetIn {
                    class: b["class"].as_str().unwrap_or("").into(),
                    confidence: f("confidence"),
                    model_version: b["model_version"].as_str().or(b["model"].as_str()).unwrap_or("").into(),
                    bbox: [f("x"), f("y"), f("x") + f("w"), f("y") + f("h")],
                    track: b["track_id"].as_u64(),
                    color: b["color"].as_str().map(str::to_string),
                    provenance: b["provenance"].as_str().map(str::to_string),
                }
            }).collect();
            // phone -> person "near" pairs, matched through the tracker's person id.
            let near: Vec<(usize, usize)> = drawn.iter().enumerate().filter_map(|(i, b)| {
                let tid = b["near_person"].as_u64()?;
                let j = drawn.iter().position(|p| p["class"] == "person" && p["track_id"].as_u64() == Some(tid))?;
                Some((i, j))
            }).collect();
            ran.sort();
            ran.dedup();
            let job_id = lease.as_ref().map(|l| l.job_id.clone()).unwrap_or_else(|| "legacy".into());
            let resolved = config::resolve_url(&cam_url);
            let batch = ingest::batch_from_frame(&FrameIn {
                source_id: &id,
                media_id: lease.as_ref().map(|l| l.media_id.as_str()),
                fingerprint: None,
                resolved_url: &resolved,
                job_id: &job_id,
                media_time_us: at_us,
                frame_index: Some(slot.seq),
                model_versions: &ran,
                detections: &dets,
                near: &near,
                thumbnail_hex: Some(thumbnail_hex.clone()),
                indexed_at_utc_us: (now_secs() * 1e6) as i64,
                lease: lease.as_ref().map(|l| LeaseRef { task_id: l.task_id.clone(), generation: l.generation }),
            });
            // Next pass can carry these forward by detection id.
            if let Some(pipe) = pipes.get_mut(&id) {
                for (d, b) in batch.detections.iter().zip(&drawn) {
                    if d.provenance != "model" { continue; }
                    let Some(tid) = b["track_id"].as_u64() else { continue };
                    let f = |k: &str| b[k].as_f64().unwrap_or(0.0) as f32;
                    pipe.last.insert(tid, Last {
                        class: d.class.clone(), bbox: [f("x"), f("y"), f("x") + f("w"), f("y") + f("h")], conf: d.confidence,
                        color: b["color"].as_str().unwrap_or("").into(), det_id: d.detection_id.clone(),
                        model: b["model"].as_str().unwrap_or("").into(), model_version: d.model_version.clone(),
                    });
                }
            }
            crate::watch::shard::node::note_media(&batch.observation.media_id, &id, &resolved);
            let legacy = serde_json::json!({"camera_id": id, "camera": name, "at": slot.at, "ts": slot.ts, "seq": slot.seq, "detections": drawn, "thumbnail_hex": thumbnail_hex});
            post_index(&shared, if slot.archive { &arch_agent } else { &live_agent }, &base, &batch, &legacy, slot.archive)
        };
        match &posted {
            Posted::Stopped => return,
            Posted::Stale(e) => {
                shared.leases.lock().unwrap().remove(&id);
                set_error(&shared, &id, "stale_lease", format!("lease revoked: {}", e.message), true);
            }
            Posted::Permanent(e) if slot.archive => set_error(&shared, &id, &e.code, format!("head rejected the batch permanently: {}", e.message), true),
            Posted::Unreachable(m) if slot.archive => set_error(&shared, &id, "index_unreachable", m.clone(), true),
            Posted::Permanent(e) => log(&format!("{name}: head rejected a live batch: {}", e.message)),
            _ => {}
        }
        if let Some(pipe) = pipes.get_mut(&id) {
            pipe.commit(&plan, slot.at);
            pipe.shown = drawn.clone();
            pipe.prune();
        }
        let persons = counts.get("person").copied().unwrap_or(0);
        let us = t0.elapsed().as_micros() as u64;
        let looks: Vec<String> = people.iter().take(2).map(|b| format!("{} person", gate::color_name(&slot.rgb, slot.w, slot.h, *b))).collect();
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
            c.processing = false;
            match &posted {
                Posted::Acked => {
                    c.last_acked = true;
                    c.indexed_frames += 1;
                    c.checkpoint_us = at_us;
                    c.cov.inferred(at_us, (1_000_000.0 / pace) as i64 * proc_every);
                }
                Posted::Skipped => c.cov.skipped(),
                _ => { c.last_acked = false; c.cov.failed() }
            }
            c.ops.detector_passes += 1;
            c.ops.model_runs += cost.runs as u64;
            c.ops.pixels_inferred += cost.pixels;
            c.ops.pixels_full += slot.w as u64 * slot.h as u64 * successful_detectors as u64;
            match &plan {
                Plan::Full { keyframe: true } => c.ops.keyframes += 1,
                Plan::Full { keyframe: false } if proc_now.detect_mode == "motion" => c.ops.full_fallbacks += 1,
                Plan::Regions(r) => { c.ops.region_frames += 1; c.ops.regions += r.len() as u64; }
                _ => {}
            }
            c.passes += 1;
            c.detect_us += us;
            c.persons = persons;
            c.boxes = serde_json::Value::Array(drawn);
        }
        let mut events = Vec::new();
        for rule in &rules {
            if !rule.enabled {
                continue;
            }
            if !rule.camera.is_empty() && rule.camera != name && rule.camera != id {
                continue;
            }
            let Ok(compiled) = rules::compile(&rule.line) else { continue };
            let on = compiled.eval(&counts);
            let freeze = compiled.needs_look && compiled.classes.iter().any(|c| !detectable.iter().any(|d| d == c));
            let key = format!("{}|{id}", rule.id);
            let streak = streaks.entry(key.clone()).or_default();
            if let Some(status) = rules::step(streak, on, freeze, rule.hold) {
                let mut why = describe(rule, &compiled, &counts, status);
                if !looks.is_empty() {
                    why = format!("{why} {}", looks.join(", "));
                }
                events.push(serde_json::json!({
                    "key": key,
                    "rule": rule.name,
                    "camera": name,
                    "camera_id": id,
                    "status": status,
                    "note": why,
                    "ts": now_secs(),
                }));
            }
        }
        if !events.is_empty() {
            let _ = auth(live_agent.post(&format!("{base}/api/events"))).set("Content-Type", "application/json").send_json(serde_json::json!({"events": events}));
        }
    }
}

fn describe(rule: &Rule, compiled: &Compiled, counts: &HashMap<String, u32>, status: &str) -> String {
    let bits: Vec<String> = compiled.classes.iter().map(|c| format!("{} {}", counts.get(c).copied().unwrap_or(0), c)).collect();
    let tail = if status == "agreed" { "Needs a person to confirm." } else { "Checking." };
    format!("{}: {}. {tail}", rule.name, bits.join(", "))
}

fn interest_of(rules: &[Rule], loaded: &[Loaded]) -> Vec<String> {
    let mut out = Vec::new();
    for m in loaded {
        for l in &m.module.labels {
            if !out.contains(l) {
                out.push(l.clone());
            }
        }
    }
    for r in rules {
        if let Ok(c) = rules::compile(&r.line) {
            for class in c.classes {
                if !out.contains(&class) {
                    out.push(class);
                }
            }
        }
    }
    out
}

fn sync_engines(shared: &Shared, loaded: &mut Vec<Loaded>, threads: usize) {
    let modules = shared.modules.lock().unwrap().clone();
    let root = repo_root();
    loaded.retain(|l| modules.iter().any(|m| m.id == l.module.id && m.enabled == l.module.enabled && m.model == l.module.model)
        && !(l.module.enabled && l.module.kind != "fight" && l.engine.is_none() && Instant::now() >= l.retry_at));
    for m in modules {
        if loaded.iter().any(|l| l.module.id == m.id) {
            if let Some(slot) = loaded.iter_mut().find(|l| l.module.id == m.id) {
                slot.module = m;
            }
            continue;
        }
        let mut engine = None;
        if m.enabled && m.kind != "fight" && !m.model.is_empty() {
            let p = root.join(&m.model);
            if p.is_file() {
                match Engine::load(&p, threads) {
                    Ok(e) => engine = Some(e),
                    Err(e) => log(&format!("could not load {}: {e}", m.model)),
                }
            } else {
                log(&format!("{} is on, but {} is not on this PC", m.name, m.model));
            }
        }
        let version = crate::watch::models::validate(&m).map(|v| v.id).unwrap_or_else(|_| m.model.clone());
        loaded.push(Loaded { module: m, engine, version, last_err: String::new(), retry_at: Instant::now() + Duration::from_secs(5) });
    }
}

/// Outcome of delivering one batch.
enum Posted {
    Acked,
    /// Not sent on purpose (segment overlap).
    Skipped,
    /// Live: dropped after one try. Archive: retries exhausted (see `Unreachable`).
    Lost,
    Unreachable(String),
    /// 4xx other than 404/408/409/429: the head will never accept this batch. Not retried.
    Permanent(ApiError),
    /// 409 stale_lease: another worker owns this task now.
    Stale(ApiError),
    Stopped,
}

const ARCHIVE_TRIES: u32 = 12;

fn now_ms() -> i64 {
    (now_secs() * 1000.0) as i64
}

fn api_error(resp: ureq::Response) -> ApiError {
    let code = resp.status();
    let body = resp.into_string().unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| serde_json::from_value::<ApiError>(v.get("error").cloned().unwrap_or_default()).ok())
        .unwrap_or_else(|| ApiError::new(if code == 409 { "conflict" } else { "invalid_request" }, format!("HTTP {code}: {}", body.chars().take(200).collect::<String>())))
}

/// One attempt: v1 first, legacy when the head has no v1 route (404, remembered for 30 s).
fn try_once(shared: &Shared, agent: &ureq::Agent, base: &str, batch: &IndexBatch, legacy: &serde_json::Value) -> Result<(), Posted> {
    let classify = |e: ureq::Error| match e {
        ureq::Error::Status(code, resp) => {
            let err = api_error(resp);
            if err.code == "stale_lease" {
                Posted::Stale(err)
            } else if (400..500).contains(&code) && !matches!(code, 408 | 429) {
                Posted::Permanent(err)
            } else {
                Posted::Unreachable(format!("head answered HTTP {code}: {}", err.message))
            }
        }
        ureq::Error::Transport(t) => Posted::Unreachable(format!("head unreachable: {t}")),
    };
    if let Some(p) = crate::watch::shard::node::commit_and_ack(batch, agent, base) {
        return match p {
            crate::watch::shard::node::Post::Ok => Ok(()),
            crate::watch::shard::node::Post::Stale(e) => Err(Posted::Stale(e)),
            crate::watch::shard::node::Post::Permanent(e) => Err(Posted::Permanent(e)),
            crate::watch::shard::node::Post::Unreachable(m) => Err(Posted::Unreachable(m)),
        };
    }
    if now_ms() >= shared.legacy_until_ms.load(Ordering::Relaxed) {
        match auth(agent.post(&format!("{base}/api/v1/index"))).send_json(batch) {
            Ok(_) => return Ok(()),
            Err(ureq::Error::Status(404, _)) => shared.legacy_until_ms.store(now_ms() + 30_000, Ordering::Relaxed),
            Err(e) => return Err(classify(e)),
        }
    }
    auth(agent.post(&format!("{base}/api/index"))).send_json(legacy.clone()).map(|_| ()).map_err(classify)
}

/// Archives: block until acknowledged, retrying transient failures with backoff up to `ARCHIVE_TRIES`; permanent
/// 4xx and stale leases stop at once. Live: one try, the frame is dropped on failure.
fn post_index(shared: &Shared, agent: &ureq::Agent, base: &str, batch: &IndexBatch, legacy: &serde_json::Value, archive: bool) -> Posted {
    let mut tries = 0u32;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Posted::Stopped;
        }
        match try_once(shared, agent, base, batch, legacy) {
            Ok(()) => return Posted::Acked,
            Err(Posted::Unreachable(m)) => {
                tries += 1;
                if !archive {
                    return Posted::Lost;
                }
                if tries >= ARCHIVE_TRIES {
                    return Posted::Unreachable(format!("{m} (gave up after {tries} attempts; the archive is paused at the last acknowledged frame)"));
                }
                if !nap(&shared.stop, Duration::from_millis((200u64 << tries.min(5)).min(5000))) {
                    return Posted::Stopped;
                }
            }
            Err(other) => return other,
        }
    }
}

/// Every enabled detector needs its ONNX file on this PC. Missing files are an error, never an empty success.
pub fn modules_ready(root: &std::path::Path, modules: &[Module]) -> Result<Vec<String>, ApiError> {
    let detectors: Vec<&Module> = modules.iter().filter(|m| m.enabled && m.kind != "fight").collect();
    if detectors.is_empty() {
        return Err(ApiError::new("model_unavailable", "No detector module is enabled. Enable a module and install its ONNX model."));
    }
    let missing: Vec<&str> = detectors.iter().filter(|m| m.model.is_empty() || !root.join(&m.model).is_file()).map(|m| m.model.as_str()).collect();
    if !missing.is_empty() {
        return Err(ApiError::new("model_unavailable", format!("Model file not found on this PC: {}. Install it (cw fetch) or disable the module.", missing.join(", "))));
    }
    Ok(detectors.iter().map(|m| m.model.clone()).collect())
}

fn caps_of(shared: &Shared, id: &str, cores: usize) -> WorkerCaps {
    let root = repo_root();
    WorkerCaps {
        id: id.into(),
        host: crate::watch::hostname(),
        cores: cores as u32,
        decoders: if std::process::Command::new("ffmpeg").arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success()) { vec!["ffmpeg".into()] } else { vec![] },
        models: shared.modules.lock().unwrap().iter().filter(|m| m.enabled && !m.model.is_empty() && root.join(&m.model).is_file()).map(|m| m.model.clone()).collect(),
        // Attached cameras by index and name (a webcam source's location is its index, e.g. "0").
        // CW_DEVICES overrides discovery: CW_DEVICES="0,FaceTime HD Camera"
        devices: std::env::var("CW_DEVICES").map(|d| d.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_else(|_| decode::capture_devices()),
        measured_fps: *shared.measured_fps.lock().unwrap(),
    }
}

/// Tell the head this worker is alive and what it can do; drop any lease the head no longer recognises.
/// Dormant against a head without `/api/v1/heartbeat` (404): checked again every 30 s.
fn heartbeat_loop(shared: Arc<Shared>, base: String, id: String, cores: usize) {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
    let mut quiet_until = 0.0;
    let mut cached: Option<(f64, WorkerCaps)> = None;
    loop {
        if !nap(&shared.stop, Duration::from_secs(2)) {
            return;
        }
        if now_secs() < quiet_until {
            continue;
        }
        // Probing ffmpeg every beat is wasteful; refresh caps every 30 s, fps every beat.
        let mut caps = match &cached {
            Some((t, c)) if now_secs() - t < 30.0 => c.clone(),
            _ => {
                let c = caps_of(&shared, &id, cores);
                cached = Some((now_secs(), c.clone()));
                c
            }
        };
        caps.measured_fps = *shared.measured_fps.lock().unwrap();
        match auth(agent.post(&format!("{base}/api/v1/heartbeat"))).send_json(serde_json::json!({"worker": id, "caps": caps, "shard": crate::watch::shard::node::announce()})) {
            Ok(r) => {
                let v: serde_json::Value = r.into_json().unwrap_or_default();
                if let Some(held) = v.get("leases").and_then(|l| serde_json::from_value::<Vec<LeaseRef>>(l.clone()).ok()) {
                    let mut leases = shared.leases.lock().unwrap();
                    let lost: Vec<String> = leases.iter().filter(|(_, l)| !held.iter().any(|h| h.task_id == l.task_id && h.generation == l.generation)).map(|(c, _)| c.clone()).collect();
                    for cam in lost {
                        leases.remove(&cam);
                        drop(leases);
                        set_error(&shared, &cam, "lease_lost", "The head no longer lists this lease (expired or reassigned); stopping this task.".into(), true);
                        leases = shared.leases.lock().unwrap();
                    }
                }
            }
            Err(ureq::Error::Status(404, _)) => quiet_until = now_secs() + 30.0,
            Err(_) => {}
        }
    }
}

fn media_at(start: f64, frames: u64, fps: f64, dur: f64) -> f64 {
    let fps = if fps > 0.1 { fps } else { 25.0 };
    let mut at = start + frames.saturating_sub(1) as f64 / fps;
    if dur > 1.0 {
        at %= dur;
    }
    at
}

fn conf_of(base: f32, mode: &str) -> f32 {
    let nudge = match mode {
        "speed" => 0.08,
        "accuracy" => -0.05,
        _ => 0.0,
    };
    (base + nudge).clamp(0.05, 0.9)
}

fn stats_loop(shared: Arc<Shared>, base: String, id: String, ip: String, local: bool) {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(500)).build();
    let mut sys = sysinfo::System::new();
    loop {
        thread::sleep(Duration::from_secs(1));
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let mut cams_json = serde_json::Map::new();
        let mut passes_total = 0u32;
        {
            let mut cams = shared.cams.lock().unwrap();
            for (cid, cam) in cams.iter_mut() {
                cams_json.insert(
                    cid.clone(),
                    serde_json::json!({
                        "name": cam.name,
                        "gated": cam.gated,
                        "blur": cam.blur,
                        "blank": cam.blank,
                        "missed": cam.missed,
                        "same": cam.same,
                        "paced": cam.paced,
                        "passes": cam.passes,
                        "detect_us": cam.detect_us,
                        "debt": cam.debt,
                        "pace": cam.pace,
                        "persons": cam.persons,
                        "error": if cam.detector_error.is_empty() { &cam.error } else { &cam.detector_error },
                        "seq": cam.seq,
                        "at": cam.at,
                        "dur": cam.dur,
                        "boxes": cam.boxes,
                        "completed": cam.completed,
                        "indexed_frames": cam.indexed_frames,
                        // Added alongside the old fields; older heads ignore them.
                        "coverage": cam.cov.cov,
                        "dropped": cam.dropped,
                        "queue_age_ms": cam.queue.front().map(|s| s.queued.elapsed().as_millis() as u64).unwrap_or(0),
                        "queue_len": cam.queue.len(),
                        "state": if cam.halt { "error" } else if cam.completed { "completed" } else { "running" },
                        "error_code": cam.error_code,
                        "checkpoint_us": cam.checkpoint_us,
                        // Cumulative (never reset). `static` + `tracked` frames ran no detector; pixels_inferred / pixels_full
                        // is the share of the picture the detector saw; model_runs counts executions (one per crop).
                        "ops": {
                            "static": cam.ops.static_frames,
                            "tracked": cam.ops.tracked_frames,
                            "keyframes": cam.ops.keyframes,
                            "full_fallbacks": cam.ops.full_fallbacks,
                            "region_frames": cam.ops.region_frames,
                            "regions": cam.ops.regions,
                            "detector_passes": cam.ops.detector_passes,
                            "model_runs": cam.ops.model_runs,
                            "pixels_inferred": cam.ops.pixels_inferred,
                            "pixels_full": cam.ops.pixels_full,
                        },
                    }),
                );
                if cam.completed && !cam.ops_logged {
                    cam.ops_logged = true;
                    let o = &cam.ops;
                    log(&format!(
                        "{}: done. detector on {} frames ({} model runs, {} keyframes, {} crop frames, {} full fallbacks); skipped {} static, {} tracked; {:.0}% of full-picture pixels; coverage {}/{} inferred/decoded",
                        cam.name, o.detector_passes, o.model_runs, o.keyframes, o.region_frames, o.full_fallbacks, o.static_frames, o.tracked_frames,
                        100.0 * o.pixels_inferred as f64 / o.pixels_full.max(1) as f64, cam.cov.cov.inferred, cam.cov.cov.decoded
                    ));
                }
                passes_total += cam.passes;
                cam.gated = 0;
                cam.blur = 0;
                cam.blank = 0;
                cam.missed = 0;
                cam.same = 0;
                cam.paced = 0;
                cam.passes = 0;
                cam.detect_us = 0;
            }
        }
        {
            // Inferred frames per second, smoothed. The scheduler weights workers by this, not by core count.
            let mut m = shared.measured_fps.lock().unwrap();
            *m = if *m == 0.0 { passes_total as f64 } else { *m * 0.7 + passes_total as f64 * 0.3 };
        }
        let body = serde_json::json!({
            "measured_fps": *shared.measured_fps.lock().unwrap(),
            "id": id,
            "host": crate::watch::hostname(),
            "ip": ip,
            "cores": crate::watch::detector_threads(),
            "local": local,
            "cpu": sys.global_cpu_usage(),
            "ram_used": sys.used_memory(),
            "ram_total": sys.total_memory(),
            "dt": 1.0,
            "cameras": cams_json,
        });
        let _ = auth(agent.post(&format!("{base}/api/stats"))).set("Content-Type", "application/json").send_json(body);
    }
}

#[cfg(test)]
mod tests {
    use super::media_at;

    #[test]
    fn media_time_wraps_inside_the_file() {
        let at = media_at(1.0, 30, 10.0, 5.0);
        assert!((at - 3.9).abs() < 0.001, "{at}");
        let wrapped = media_at(0.0, 51, 25.0, 2.0);
        assert!(wrapped.abs() < 0.001, "{wrapped}");
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    fn module(model: &str, enabled: bool, kind: &str) -> Module {
        Module { id: "m".into(), name: "m".into(), kind: kind.into(), model: model.into(), labels: vec![], conf: 0.4, confirm: 1, gflops: 0.0, enabled }
    }

    #[test]
    fn missing_model_is_an_error_not_an_empty_success() {
        let root = std::env::temp_dir();
        let e = modules_ready(&root, &[module("vision/models/nope-does-not-exist.onnx", true, "object")]).unwrap_err();
        assert_eq!(e.code, "model_unavailable");
        assert!(e.message.contains("nope-does-not-exist.onnx"));
        assert_eq!(modules_ready(&root, &[module("x.onnx", false, "object")]).unwrap_err().code, "model_unavailable");
        assert_eq!(modules_ready(&root, &[module("", true, "fight")]).unwrap_err().code, "model_unavailable", "fight alone is not a detector");
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(modules_ready(&real, &[module("vision/models/yolov5n.onnx", true, "object")]).unwrap(), vec!["vision/models/yolov5n.onnx".to_string()]);
    }
}

/// Worker credential for an authenticated head (`CW_WORKER_TOKEN`). Absent = open-mode head.
fn auth(r: ureq::Request) -> ureq::Request {
    match std::env::var("CW_WORKER_TOKEN") {
        Ok(t) if !t.is_empty() => r.set("Authorization", &format!("Bearer {t}")),
        _ => r,
    }
}
