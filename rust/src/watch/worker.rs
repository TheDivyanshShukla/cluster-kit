//! One process per PC. The reader gates every frame. The detector thread takes the
//! latest sharp frame. Rules run only on frames the detector actually saw.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::watch::config::{self, Camera, Module, Process, Rule};
use crate::watch::decode::{self, Feed};
use crate::watch::fight::{self, union};
use crate::watch::gate::{self, Gray};
use crate::watch::rules::{self, Compiled, Streak};
use crate::watch::yolo::Engine;
use crate::watch::{log, now_secs, primary_ip, repo_root, Opts};

struct Slot {
    rgb: Vec<u8>,
    w: u32,
    h: u32,
    gray: Gray,
    prev: Vec<u8>,
}

struct CamLive {
    name: String,
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
    slot: Option<Slot>,
    seq: u64,
    boxes: serde_json::Value,
}

struct Shared {
    proc: Mutex<Process>,
    modules: Mutex<Vec<Module>>,
    rules: Mutex<Vec<Rule>>,
    cams: Mutex<HashMap<String, CamLive>>,
    stop: AtomicBool,
    turn: AtomicUsize,
}

pub fn run(opts: Opts) -> Result<(), String> {
    let ip = primary_ip();
    let id = format!("{}|{ip}", crate::watch::hostname());
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
    let shared = Arc::new(Shared {
        proc: Mutex::new(config::preset("balanced")),
        modules: Mutex::new(Vec::new()),
        rules: Mutex::new(Vec::new()),
        cams: Mutex::new(HashMap::new()),
        stop: AtomicBool::new(false),
        turn: AtomicUsize::new(0),
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

    let mut running: HashMap<String, (String, Arc<AtomicBool>)> = HashMap::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let work = ureq::get(&format!("{base}/api/work"))
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
            *shared.rules.lock().unwrap() = r;
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
            if running.contains_key(&id) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            shared.cams.lock().unwrap().insert(
                id.clone(),
                CamLive {
                    name: cam.name.clone(),
                    gated: 0,
                    blur: 0,
                    blank: 0,
                    missed: 0,
                    same: 0,
                    paced: 0,
                    passes: 0,
                    detect_us: 0,
                    debt: 0.0,
                    pace: 25.0,
                    persons: 0,
                    at: 0.0,
                    dur: 0.0,
                    error: String::new(),
                    slot: None,
                    seq: 0,
                    boxes: serde_json::json!([]),
                },
            );
            let shared2 = shared.clone();
            let stop2 = stop.clone();
            let cid = id.clone();
            thread::spawn(move || camera_loop(cam, cid, shared2, stop2));
            running.insert(id, (String::new(), stop));
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn camera_loop(cam: Camera, id: String, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let url = config::resolve_url(&cam.url);
    let probed = if cam.kind == "file" { decode::probe(&url) } else { decode::Probe { fps: 25.0, duration: 0.0 } };
    let ss = if probed.duration > 0.0 { cam.start * probed.duration } else { 0.0 };
    let started = Instant::now();
    let mut frames = 0u64;
    let mut prev: Option<Gray> = None;
    let mut last_sent: Option<Gray> = None;
    let mut eligible = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let width = shared.proc.lock().unwrap().width;
        let mut feed = match Feed::open(&url, &cam.kind, ss, width) {
            Ok(f) => f,
            Err(e) => {
                if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
                    c.error = format!("ffmpeg did not start ({e}). Install ffmpeg and leave it on PATH.");
                }
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
            c.error.clear();
            c.pace = probed.fps;
        }
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (w, h, rgb) = match feed.next_frame() {
                Ok(v) => v,
                Err(_) => break,
            };
            frames += 1;
            let proc = shared.proc.lock().unwrap().clone();
            if proc.width != width {
                break;
            }
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
                    if proc.same_on && diff < proc.same {
                        None
                    } else {
                        eligible = eligible.wrapping_add(1);
                        Some(proc.every <= 1 || (eligible - 1) % proc.every == 0)
                    }
                }
                _ => None,
            };
            let at = media_at(ss, frames, probed.fps, probed.duration);
            let debt = (started.elapsed().as_secs_f64() * probed.fps - frames as f64).max(0.0);
            {
                let mut cams = shared.cams.lock().unwrap();
                let Some(c) = cams.get_mut(&id) else { return };
                c.gated += 1;
                c.debt = debt;
                c.error.clear();
                match g {
                    gate::Gate::Blank => c.blank += 1,
                    gate::Gate::Blur => c.blur += 1,
                    gate::Gate::Sharp => match send {
                        None => c.same += 1,
                        Some(false) => c.paced += 1,
                        Some(true) => {
                            let previous = prev.clone().map(|p| p.px).unwrap_or_else(|| gray.px.clone());
                            let slot = Slot { rgb: rgb.clone(), w, h, gray: gray.clone(), prev: previous };
                            if c.slot.is_some() {
                                c.missed += 1;
                            }
                            c.slot = Some(slot);
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
}

fn infer_loop(shared: Arc<Shared>, threads: usize, base: String) {
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut streaks: HashMap<String, Streak> = HashMap::new();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(400)).build();
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
                    if cam.slot.is_some() {
                        found = Some((id.clone(), cam.slot.take().unwrap(), cam.name.clone()));
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
        let rules = shared.rules.lock().unwrap().clone();
        let mut counts: HashMap<String, u32> = HashMap::new();
        let mut people = Vec::new();
        let mut detectable = Vec::new();
        let mut drawn = Vec::new();
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
            match engine.run(&slot.rgb, slot.w, slot.h, conf, &item.module.labels, &interest) {
                Ok(dets) => {
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
        let fight_on = loaded.iter().any(|m| m.module.enabled && m.module.kind == "fight");
        if fight_on {
            detectable.push("fight".into());
            let people_boxes = people.clone();
            let gray = &slot.gray;
            let prev = &slot.prev;
            if fight::fight_frame(&people_boxes, |i, j| {
                let u = union(people_boxes[i], people_boxes[j]);
                gate::motion_box(prev, &gray.px, gray.w, gray.h, u, sw, sh)
            }) {
                counts.insert("fight".into(), 1);
                drawn.push(serde_json::json!({"class": "fight", "color": "", "x": 0, "y": 0, "w": 0, "h": 0}));
            }
        }
        let persons = counts.get("person").copied().unwrap_or(0);
        let us = t0.elapsed().as_micros() as u64;
        let looks: Vec<String> = people.iter().take(2).map(|b| format!("{} person", gate::color_name(&slot.rgb, slot.w, slot.h, *b))).collect();
        if let Some(c) = shared.cams.lock().unwrap().get_mut(&id) {
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
            let _ = agent.post(&format!("{base}/api/events")).set("Content-Type", "application/json").send_json(serde_json::json!({"events": events}));
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
    loaded.retain(|l| modules.iter().any(|m| m.id == l.module.id && m.enabled == l.module.enabled && m.model == l.module.model));
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
        loaded.push(Loaded { module: m, engine, last_err: String::new() });
    }
}

fn media_at(start: f64, frames: u64, fps: f64, dur: f64) -> f64 {
    let fps = if fps > 0.1 { fps } else { 25.0 };
    let mut at = start + frames as f64 / fps;
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
                        "error": cam.error,
                        "seq": cam.seq,
                        "at": cam.at,
                        "dur": cam.dur,
                        "boxes": cam.boxes,
                    }),
                );
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
        let body = serde_json::json!({
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
        let _ = agent.post(&format!("{base}/api/stats")).set("Content-Type", "application/json").send_json(body);
    }
}

#[cfg(test)]
mod tests {
    use super::media_at;

    #[test]
    fn media_time_wraps_inside_the_file() {
        let at = media_at(1.0, 30, 10.0, 5.0);
        assert!((at - 4.0).abs() < 0.001, "{at}");
        let wrapped = media_at(0.0, 50, 25.0, 2.0);
        assert!(wrapped.abs() < 0.001, "{wrapped}");
    }
}
