//! The head serves the panel, keeps the rule list, and assigns cameras.
//! Numbers shown on the panel are smoothed in `approx`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::watch::approx::{self, PcSample};
use crate::watch::config::{self, Camera, Module, Rule};
use crate::watch::split::{self, Node};
use crate::watch::{log, now_secs, primary_ip, Opts};

struct Worker {
    host: String,
    ip: String,
    cores: usize,
    local: bool,
    seen: f64,
    cpu: Option<f64>,
    ram_used: Option<f64>,
    ram_total: Option<f64>,
    cameras: HashMap<String, CamRate>,
}

struct CamRate {
    name: String,
    gated: Option<f64>,
    blur: Option<f64>,
    blank: Option<f64>,
    missed: Option<f64>,
    same: Option<f64>,
    paced: Option<f64>,
    passes: Option<f64>,
    ms: Option<f64>,
    debt: Option<f64>,
    persons: Option<f64>,
    at: f64,
    dur: f64,
    error: String,
    seq: u64,
    boxes: serde_json::Value,
}

struct Preview {
    jpeg: Vec<u8>,
    seq: u64,
    fps: f64,
    mark: f64,
    count: u32,
}

struct Event {
    id: u64,
    ts: f64,
    key: String,
    rule: String,
    camera: String,
    status: String,
    note: String,
}

struct Seen {
    id: u64,
    ts: f64,
    camera: String,
    camera_id: String,
    class: String,
    color: String,
    jpeg: Vec<u8>,
    at: f64,
}

struct Hub {
    solo: Mutex<bool>,
    workers: Mutex<HashMap<String, Worker>>,
    previews: Mutex<HashMap<String, Preview>>,
    events: Mutex<Vec<Event>>,
    next_event: Mutex<u64>,
    rejected: Mutex<HashMap<String, f64>>,
    history: Mutex<Vec<serde_json::Value>>,
    seen: Mutex<Vec<Seen>>,
    next_seen: Mutex<u64>,
    live: Condvar,
    pipes: Mutex<Vec<Arc<Mutex<Option<Child>>>>>,
}

pub fn run(opts: Opts) -> Result<(), String> {
    let hub = Arc::new(Hub {
        solo: Mutex::new(false),
        workers: Mutex::new(HashMap::new()),
        previews: Mutex::new(HashMap::new()),
        events: Mutex::new(Vec::new()),
        next_event: Mutex::new(1),
        rejected: Mutex::new(HashMap::new()),
        history: Mutex::new(Vec::new()),
        seen: Mutex::new(Vec::new()),
        next_seen: Mutex::new(1),
        live: Condvar::new(),
        pipes: Mutex::new(Vec::new()),
    });
    {
        let (list, next) = load_log();
        *hub.seen.lock().unwrap() = list;
        *hub.next_seen.lock().unwrap() = next;
    }
    thread::spawn({
        let hub = hub.clone();
        move || watch_pictures(hub)
    });
    thread::spawn({
        let name = opts.name.clone();
        let port = opts.port;
        move || crate::watch::net::beacon(name, port)
    });
    if !opts.no_local {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(exe);
        cmd.args(["worker", "--head", "127.0.0.1", "--local", "--name", &opts.name, "--port", &opts.port.to_string()]);
        match cmd.spawn() {
            Ok(child) => {
                let child = Mutex::new(Some(child));
                let pipes = hub.clone();
                let _ = ctrlc::set_handler(move || {
                    if let Ok(mut g) = child.lock() {
                        if let Some(c) = g.as_mut() {
                            let _ = c.kill();
                        }
                    }
                    if let Ok(list) = pipes.pipes.lock() {
                        for pipe in list.iter() {
                            if let Ok(mut g) = pipe.lock() {
                                if let Some(c) = g.as_mut() {
                                    let _ = c.kill();
                                }
                            }
                        }
                    }
                    std::process::exit(0);
                });
            }
            Err(e) => log(&format!("local worker did not start: {e}")),
        }
    }
    let ip = primary_ip();
    log(&format!("watch  http://{ip}:{}", opts.port));
    log("other PCs: kit rs watch worker");
    let server = Server::http(format!("0.0.0.0:{}", opts.port)).map_err(|e| e.to_string())?;
    for mut req in server.incoming_requests() {
        let hub = hub.clone();
        thread::spawn(move || {
            let method = req.method().clone();
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or(&url);
            if method == Method::Get {
                if let Some(id) = path.strip_prefix("/api/live/") {
                    let _ = req.respond(live_response(hub, id.to_string()));
                    return;
                }
                if let Some(id) = path.strip_prefix("/api/media/") {
                    let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
                    let _ = req.respond(media_response(id, range.as_deref()));
                    return;
                }
            }
            let mut body = Vec::new();
            let _ = req.as_reader().take(2_000_000).read_to_end(&mut body);
            let response = handle(&hub, &method, &url, &body, &req);
            let _ = req.respond(response);
        });
    }
    Ok(())
}

fn watch_pictures(hub: Arc<Hub>) {
    let mut running: HashMap<String, (String, Arc<AtomicBool>, Arc<Mutex<Option<Child>>>)> = HashMap::new();
    loop {
        let watch = config::load_watch();
        let rate = watch.process.rate;
        let want: HashMap<String, (String, Camera)> = watch
            .cameras
            .iter()
            .map(|c| (c.id.clone(), (format!("{}|{:.3}|{rate}", c.url, c.start), c.clone())))
            .collect();
        let stale: Vec<String> = running
            .iter()
            .filter(|(id, (key, _, _))| want.get(*id).map(|(k, _)| k != key).unwrap_or(true))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some((_, stop, pipe)) = running.remove(&id) {
                stop.store(true, Ordering::Relaxed);
                if let Ok(mut g) = pipe.lock() {
                    if let Some(c) = g.as_mut() {
                        let _ = c.kill();
                    }
                }
            }
        }
        for (id, (key, cam)) in want {
            if running.contains_key(&id) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let pipe = Arc::new(Mutex::new(None));
            running.insert(id.clone(), (key, stop.clone(), pipe.clone()));
            hub.pipes.lock().unwrap().push(pipe.clone());
            let hub2 = hub.clone();
            thread::spawn(move || pump_camera(cam, rate, id, hub2, stop, pipe));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn pump_camera(cam: Camera, rate: u32, id: String, hub: Arc<Hub>, stop: Arc<AtomicBool>, pipe: Arc<Mutex<Option<Child>>>) {
    let url = config::resolve_url(&cam.url);
    let probed = if cam.kind == "file" { crate::watch::decode::probe(&url) } else { crate::watch::decode::Probe { fps: 25.0, duration: 0.0 } };
    let ss = if probed.duration > 0.0 { cam.start * probed.duration } else { 0.0 };
    while !stop.load(Ordering::Relaxed) {
        let mut motion = match crate::watch::decode::Motion::open(&url, &cam.kind, ss, rate) {
            Ok(m) => m,
            Err(e) => {
                log(&format!("{id} picture did not start ({e})"));
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        {
            let handle = motion.child();
            let mut inner = handle.lock().unwrap();
            let mut slot = pipe.lock().unwrap();
            *slot = inner.take();
        }
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match motion.next_jpeg() {
                Ok(jpeg) => publish_frame(&hub, &id, jpeg),
                Err(_) => break,
            }
        }
        if let Some(mut c) = pipe.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
    }
}

fn publish_frame(hub: &Hub, id: &str, jpeg: Vec<u8>) {
    if jpeg.len() < 4 {
        return;
    }
    let now = now_secs();
    {
        let mut g = hub.previews.lock().unwrap();
        let p = g.entry(id.to_string()).or_insert(Preview { jpeg: Vec::new(), seq: 0, fps: 0.0, mark: now, count: 0 });
        p.jpeg = jpeg;
        p.seq = p.seq.wrapping_add(1);
        p.count += 1;
        let dt = now - p.mark;
        if dt >= 0.5 {
            p.fps = p.count as f64 / dt;
            p.count = 0;
            p.mark = now;
        }
    }
    hub.live.notify_all();
}

struct LiveBody {
    hub: Arc<Hub>,
    id: String,
    seen: u64,
    last: Vec<u8>,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for LiveBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.pending.len() {
            let jpeg = self.wait_jpeg();
            if jpeg.is_empty() {
                return Ok(0);
            }
            let mut part = Vec::with_capacity(jpeg.len() + 80);
            part.extend_from_slice(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ");
            part.extend_from_slice(jpeg.len().to_string().as_bytes());
            part.extend_from_slice(b"\r\n\r\n");
            part.extend_from_slice(&jpeg);
            part.extend_from_slice(b"\r\n");
            self.pending = part;
            self.pos = 0;
        }
        let n = (self.pending.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl LiveBody {
    fn wait_jpeg(&mut self) -> Vec<u8> {
        let mut guard = self.hub.previews.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(p) = guard.get(&self.id) {
                if p.seq != self.seen && !p.jpeg.is_empty() {
                    self.seen = p.seq;
                    self.last = p.jpeg.clone();
                    return self.last.clone();
                }
            }
            if std::time::Instant::now() >= deadline {
                return self.last.clone();
            }
            let (g, _) = self.hub.live.wait_timeout(guard, Duration::from_millis(200)).unwrap();
            guard = g;
        }
    }
}

fn live_response(hub: Arc<Hub>, id: String) -> Response<LiveBody> {
    let body = LiveBody { hub, id, seen: 0, last: Vec::new(), pending: Vec::new(), pos: 0 };
    let ctype = Header::from_bytes(b"Content-Type", b"multipart/x-mixed-replace; boundary=frame").unwrap();
    let cache = Header::from_bytes(b"Cache-Control", b"no-store").unwrap();
    Response::new(StatusCode(200), vec![ctype, cache], body, None, None)
}

fn handle(hub: &Hub, method: &Method, url: &str, body: &[u8], _req: &tiny_http::Request) -> Response<std::io::Cursor<Vec<u8>>> {
    let path = url.split('?').next().unwrap_or(url);
    if method == &Method::Get {
        if let Some(page) = ui_page(path) {
            return page;
        }
    }
    if path == "/api/health" {
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/state" && method == &Method::Get {
        return json(snapshot(hub));
    }
    if path == "/api/work" && method == &Method::Get {
        return json(assignment(hub, url));
    }
    if path == "/api/stats" && method == &Method::Post {
        ingest_stats(hub, body);
        return json(serde_json::json!({"ok": true}));
    }
    if let Some(id) = path.strip_prefix("/api/preview/") {
        if method == &Method::Post {
            publish_frame(hub, id, body.to_vec());
            return json(serde_json::json!({"ok": true}));
        }
        if method == &Method::Get {
            if let Some(p) = hub.previews.lock().unwrap().get(id) {
                return bytes(p.jpeg.clone(), "image/jpeg");
            }
            return empty(StatusCode(404));
        }
    }
    if let Some(id) = path.strip_prefix("/api/hit/") {
        if method == &Method::Get {
            let id: u64 = id.parse().unwrap_or(0);
            if let Some(hit) = hub.seen.lock().unwrap().iter().find(|s| s.id == id) {
                if !hit.jpeg.is_empty() {
                    return bytes(hit.jpeg.clone(), "image/jpeg");
                }
            }
            return empty(StatusCode(404));
        }
    }
    if path == "/api/events" && method == &Method::Post {
        ingest_events(hub, body);
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/confirm" && method == &Method::Post {
        confirm(hub, body);
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/rules" && method == &Method::Post {
        return match serde_json::from_slice::<Vec<Rule>>(body) {
            Ok(rules) => {
                for r in &rules {
                    if let Err(e) = rules::compile(&r.line) {
                        return json_err(&format!("{}: {e}", r.name));
                    }
                }
                match config::save_json(&config::rules_path(), &rules) {
                    Ok(()) => json(serde_json::json!({"ok": true})),
                    Err(e) => json_err(&e),
                }
            }
            Err(e) => json_err(&e.to_string()),
        };
    }
    if path == "/api/modules" && method == &Method::Post {
        return match serde_json::from_slice::<Vec<Module>>(body) {
            Ok(m) => match config::save_json(&config::modules_path(), &m) {
                Ok(()) => json(serde_json::json!({"ok": true})),
                Err(e) => json_err(&e),
            },
            Err(e) => json_err(&e.to_string()),
        };
    }
    if path == "/api/settings" && method == &Method::Post {
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
        if let Some(solo) = v.get("solo").and_then(|x| x.as_bool()) {
            *hub.solo.lock().unwrap() = solo;
        }
        let watch = config::load_watch();
        let mut proc = watch.process.clone();
        if v.get("apply_preset").and_then(|x| x.as_bool()).unwrap_or(false) {
            if let Some(mode) = v.get("mode").and_then(|x| x.as_str()) {
                proc = config::preset(mode);
            }
        } else {
            if let Some(mode) = v.get("mode").and_then(|x| x.as_str()) {
                proc.mode = mode.to_string();
            }
            if let Some(b) = v.get("blur_on").and_then(|x| x.as_bool()) {
                proc.blur_on = b;
            }
            if let Some(n) = v.get("blur").and_then(|x| x.as_f64()) {
                proc.blur = n as f32;
            }
            if let Some(b) = v.get("blank_on").and_then(|x| x.as_bool()) {
                proc.blank_on = b;
            }
            if let Some(b) = v.get("same_on").and_then(|x| x.as_bool()) {
                proc.same_on = b;
            }
            if let Some(n) = v.get("same").and_then(|x| x.as_f64()) {
                proc.same = n as f32;
            }
            if let Some(n) = v.get("every").and_then(|x| x.as_u64()) {
                proc.every = n as u32;
            }
            if let Some(n) = v.get("width").and_then(|x| x.as_u64()) {
                proc.width = n as u32;
            }
            if let Some(n) = v.get("rate").and_then(|x| x.as_u64()) {
                proc.rate = n as u32;
            }
        }
        let _ = config::save_process(&proc, watch.copies);
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/find" && method == &Method::Post {
        return json(run_find(hub, body));
    }
    if path == "/api/searches" && method == &Method::Post {
        return match serde_json::from_slice::<Vec<config::SavedSearch>>(body) {
            Ok(list) => match config::save_json(&config::searches_path(), &list) {
                Ok(()) => json(serde_json::json!({"ok": true})),
                Err(e) => json_err(&e),
            },
            Err(e) => json_err(&e.to_string()),
        };
    }
    empty(StatusCode(404))
}

fn assignment(hub: &Hub, url: &str) -> serde_json::Value {
    let id = query(url, "id").unwrap_or_default();
    let host = query(url, "host").unwrap_or_else(|| "pc".into());
    let cores = query(url, "cores").and_then(|s| s.parse().ok()).unwrap_or(1usize);
    let local = query(url, "local").as_deref() == Some("1");
    if !id.is_empty() {
        let mut workers = hub.workers.lock().unwrap();
        let w = workers.entry(id.clone()).or_insert_with(|| Worker {
            host: host.clone(),
            ip: String::new(),
            cores,
            local,
            seen: now_secs(),
            cpu: None,
            ram_used: None,
            ram_total: None,
            cameras: HashMap::new(),
        });
        w.host = host;
        w.cores = cores.max(1);
        w.local = local;
        w.seen = now_secs();
    }
    let watch = config::load_watch();
    let rules = config::load_rules();
    let modules = config::load_modules();
    let solo = *hub.solo.lock().unwrap();
    let now = now_secs();
    let nodes: Vec<Node> = hub
        .workers
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, w)| now - w.seen < 6.0)
        .map(|(id, w)| Node { id: id.clone(), cores: w.cores, local: w.local })
        .collect();
    let ids: Vec<String> = watch.cameras.iter().map(|c| c.id.clone()).collect();
    let map = split::assign(&nodes, &ids, solo);
    let mine: Vec<&Camera> = watch.cameras.iter().filter(|c| map.get(&c.id).map(|s| s == &id).unwrap_or(false)).collect();
    serde_json::json!({
        "blur": watch.blur,
        "process": watch.process,
        "cameras": mine,
        "modules": modules,
        "rules": rules,
    })
}

fn ingest_stats(hub: &Hub, body: &[u8]) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
    let Some(id) = v.get("id").and_then(|x| x.as_str()) else { return };
    let mut workers = hub.workers.lock().unwrap();
    let w = workers.entry(id.to_string()).or_insert_with(|| Worker {
        host: "pc".into(),
        ip: String::new(),
        cores: 1,
        local: false,
        seen: 0.0,
        cpu: None,
        ram_used: None,
        ram_total: None,
        cameras: HashMap::new(),
    });
    if let Some(h) = v.get("host").and_then(|x| x.as_str()) {
        w.host = h.to_string();
    }
    if let Some(ip) = v.get("ip").and_then(|x| x.as_str()) {
        w.ip = ip.to_string();
    }
    if let Some(c) = v.get("cores").and_then(|x| x.as_u64()) {
        w.cores = c.max(1) as usize;
    }
    w.local = v.get("local").and_then(|x| x.as_bool()).unwrap_or(w.local);
    w.seen = now_secs();
    if let Some(cpu) = v.get("cpu").and_then(|x| x.as_f64()) {
        w.cpu = Some(approx::ema(w.cpu, cpu));
    }
    if let Some(n) = v.get("ram_used").and_then(|x| x.as_f64()) {
        w.ram_used = Some(n);
    }
    if let Some(n) = v.get("ram_total").and_then(|x| x.as_f64()) {
        w.ram_total = Some(n);
    }
    let dt = v.get("dt").and_then(|x| x.as_f64()).unwrap_or(1.0).max(0.2);
    let mut notes = Vec::new();
    if let Some(obj) = v.get("cameras").and_then(|x| x.as_object()) {
        for (cid, cam) in obj {
            let rate = |key: &str| cam.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0) / dt;
            let entry = w.cameras.entry(cid.clone()).or_insert_with(|| CamRate {
                name: cid.clone(),
                gated: None,
                blur: None,
                blank: None,
                missed: None,
                same: None,
                paced: None,
                passes: None,
                ms: None,
                debt: None,
                persons: None,
                at: 0.0,
                dur: 0.0,
                error: String::new(),
                seq: 0,
                boxes: serde_json::json!([]),
            });
            if let Some(n) = cam.get("name").and_then(|x| x.as_str()) {
                entry.name = n.to_string();
            }
            entry.gated = Some(approx::ema(entry.gated, rate("gated")));
            entry.blur = Some(approx::ema(entry.blur, rate("blur")));
            entry.blank = Some(approx::ema(entry.blank, rate("blank")));
            entry.missed = Some(approx::ema(entry.missed, rate("missed")));
            entry.same = Some(approx::ema(entry.same, rate("same")));
            entry.paced = Some(approx::ema(entry.paced, rate("paced")));
            entry.passes = Some(approx::ema(entry.passes, rate("passes")));
            let passes = cam.get("passes").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let us = cam.get("detect_us").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let ms = if passes > 0.0 { us / passes / 1000.0 } else { 0.0 };
            entry.ms = Some(approx::ema(entry.ms, ms));
            entry.debt = Some(approx::ema(entry.debt, cam.get("debt").and_then(|x| x.as_f64()).unwrap_or(0.0)));
            entry.persons = Some(approx::ema(entry.persons, cam.get("persons").and_then(|x| x.as_f64()).unwrap_or(0.0)));
            entry.error = cam.get("error").and_then(|x| x.as_str()).unwrap_or("").to_string();
            entry.seq = cam.get("seq").and_then(|x| x.as_u64()).unwrap_or(entry.seq);
            entry.at = cam.get("at").and_then(|x| x.as_f64()).unwrap_or(entry.at);
            entry.dur = cam.get("dur").and_then(|x| x.as_f64()).unwrap_or(entry.dur);
            entry.boxes = cam.get("boxes").cloned().unwrap_or(serde_json::json!([]));
            let at = entry.at;
            if let Some(arr) = entry.boxes.as_array() {
                for b in arr {
                    let class = b.get("class").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if class.is_empty() {
                        continue;
                    }
                    let color = b.get("color").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    notes.push((cid.clone(), entry.name.clone(), class, color, at));
                }
            }
        }
    }
    drop(workers);
    note_seen(hub, notes);
}

fn note_seen(hub: &Hub, notes: Vec<(String, String, String, String, f64)>) {
    if notes.is_empty() {
        return;
    }
    let now = now_secs();
    let mut lines = Vec::new();
    {
        let mut seen = hub.seen.lock().unwrap();
        let mut next = hub.next_seen.lock().unwrap();
        let previews = hub.previews.lock().unwrap();
        for (cid, camera, class, color, at) in notes {
            if seen.iter().rev().take(40).any(|s| s.camera_id == cid && s.class == class && s.color == color && now - s.ts < 4.0) {
                continue;
            }
            let jpeg = previews.get(&cid).map(|p| p.jpeg.clone()).unwrap_or_default();
            let id = *next;
            *next += 1;
            seen.push(Seen { id, ts: now, camera: camera.clone(), camera_id: cid.clone(), class: class.clone(), color: color.clone(), jpeg, at });
            lines.push(serde_json::json!({"id": id, "ts": now, "at": at, "camera": camera, "camera_id": cid, "class": class, "color": color}));
        }
        if seen.len() > 40 {
            let keep = seen.len() - 40;
            for s in &mut seen[..keep] {
                s.jpeg.clear();
            }
        }
        if seen.len() > 2000 {
            let drop_n = seen.len() - 2000;
            seen.drain(0..drop_n);
        }
    }
    for line in lines {
        append_log(&line);
    }
}

fn run_find(hub: &Hub, body: &[u8]) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
    let ask = crate::watch::query::Ask {
        mode: v.get("mode").and_then(|x| x.as_str()).unwrap_or("words").to_string(),
        words: v.get("words").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        class: v.get("class").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        color: v.get("color").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        camera: v.get("camera").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        from: v.get("from").and_then(|x| x.as_f64()).unwrap_or(0.0),
        to: v.get("to").and_then(|x| x.as_f64()).unwrap_or(0.0),
        lua: v.get("lua").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    };
    let seen = hub.seen.lock().unwrap();
    let hits: Vec<crate::watch::query::HitIn> = seen
        .iter()
        .map(|s| crate::watch::query::HitIn {
            id: s.id,
            ts: s.ts,
            at: s.at,
            camera: s.camera.clone(),
            camera_id: s.camera_id.clone(),
            class: s.class.clone(),
            color: s.color.clone(),
        })
        .collect();
    match crate::watch::query::ask(&hits, &ask) {
        Ok(ids) => {
            let mut out = Vec::new();
            for id in ids.into_iter().rev().take(400).collect::<Vec<_>>().into_iter().rev() {
                if let Some(s) = seen.iter().find(|s| s.id == id) {
                    out.push(serde_json::json!({
                        "id": s.id, "ts": s.ts, "at": s.at,
                        "camera": s.camera, "camera_id": s.camera_id,
                        "class": s.class, "color": s.color,
                    }));
                }
            }
            serde_json::json!({"hits": out, "error": ""})
        }
        Err(e) => serde_json::json!({"hits": [], "error": e}),
    }
}

fn log_path() -> std::path::PathBuf {
    crate::watch::repo_root().join("vision/data/seen.jsonl")
}

fn load_log() -> (Vec<Seen>, u64) {
    let text = std::fs::read_to_string(log_path()).unwrap_or_default();
    let mut list = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        list.push(Seen {
            id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            ts: v.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0),
            at: v.get("at").and_then(|x| x.as_f64()).unwrap_or(0.0),
            camera: v.get("camera").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            camera_id: v.get("camera_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            class: v.get("class").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            color: v.get("color").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            jpeg: Vec::new(),
        });
    }
    if list.len() > 2000 {
        let drop_n = list.len() - 2000;
        list.drain(0..drop_n);
    }
    let next = list.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    (list, next)
}

fn append_log(line: &serde_json::Value) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    use std::io::Write;
    let _ = writeln!(file, "{line}");
}

fn media_path(id: &str) -> Option<std::path::PathBuf> {
    let id = id.split('?').next().unwrap_or(id);
    let watch = config::load_watch();
    let cam = watch.cameras.iter().find(|c| c.id == id)?;
    if cam.kind != "file" {
        return None;
    }
    let path = std::path::PathBuf::from(config::resolve_url(&cam.url));
    if path.is_file() { Some(path) } else { None }
}

fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let s = header.trim().strip_prefix("bytes=")?;
    let (a, b) = s.split_once('-')?;
    let start = if a.is_empty() { 0 } else { a.parse().ok()? };
    let end = if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) };
    if start >= len || start > end {
        return None;
    }
    Some((start, end.min(start + 8 * 1024 * 1024 - 1)))
}

fn media_response(id: &str, range: Option<&str>) -> Response<std::io::Cursor<Vec<u8>>> {
    let Some(path) = media_path(id) else { return empty(StatusCode(404)) };
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => return empty(StatusCode(404)),
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (start, end, status) = if let Some(h) = range {
        match parse_range(h, len) {
            Some((s, e)) => (s, e, StatusCode(206)),
            None => return empty(StatusCode(416)),
        }
    } else if len > 16 * 1024 * 1024 {
        (0, 16 * 1024 * 1024 - 1, StatusCode(206))
    } else {
        (0, len.saturating_sub(1), StatusCode(200))
    };
    if len == 0 {
        return empty(StatusCode(404));
    }
    let n = (end - start + 1) as usize;
    let mut buf = vec![0u8; n];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
        return empty(StatusCode(404));
    }
    let ctype = Header::from_bytes(b"Content-Type", b"video/mp4").unwrap();
    let ranges = Header::from_bytes(b"Accept-Ranges", b"bytes").unwrap();
    let mut headers = vec![ctype, ranges];
    if status.0 == 206 {
        headers.push(Header::from_bytes(b"Content-Range", format!("bytes {start}-{end}/{len}").as_bytes()).unwrap());
    }
    // A known length under the threshold is sent with Content-Length. Video
    // seeking needs that. The default threshold would chunk anything over 32 KB.
    Response::new(status, headers, std::io::Cursor::new(buf), Some(n), None).with_chunked_threshold(usize::MAX)
}

fn ingest_events(hub: &Hub, body: &[u8]) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
    let Some(arr) = v.get("events").and_then(|x| x.as_array()) else { return };
    let mut events = hub.events.lock().unwrap();
    let mut next = hub.next_event.lock().unwrap();
    let rejected = hub.rejected.lock().unwrap();
    let now = now_secs();
    for e in arr {
        let key = e.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if rejected.get(&key).copied().unwrap_or(0.0) > now {
            continue;
        }
        let status = e.get("status").and_then(|x| x.as_str()).unwrap_or("checking").to_string();
        let note = e.get("note").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let rule = e.get("rule").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let camera = e.get("camera").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if let Some(existing) = events.iter_mut().rev().find(|ev| ev.key == key && now - ev.ts < 600.0 && ev.status != "rejected") {
            existing.status = status;
            existing.note = note;
            existing.ts = now;
            continue;
        }
        let id = *next;
        *next += 1;
        events.push(Event { id, ts: now, key, rule, camera, status, note });
    }
    if events.len() > 400 {
        let drop_n = events.len() - 400;
        events.drain(0..drop_n);
    }
}

fn confirm(hub: &Hub, body: &[u8]) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
    let Some(id) = v.get("id").and_then(|x| x.as_u64()) else { return };
    let verdict = v.get("verdict").and_then(|x| x.as_str()).unwrap_or("confirm");
    let mut events = hub.events.lock().unwrap();
    if let Some(ev) = events.iter_mut().find(|e| e.id == id) {
        if verdict == "reject" {
            ev.status = "rejected".into();
            hub.rejected.lock().unwrap().insert(ev.key.clone(), now_secs() + 600.0);
        } else {
            ev.status = "confirmed".into();
        }
    }
}

fn snapshot(hub: &Hub) -> serde_json::Value {
    let feeds: HashMap<String, f64> = hub.previews.lock().unwrap().iter().map(|(k, p)| (k.clone(), p.fps)).collect();
    let watch = config::load_watch();
    let rules = config::load_rules();
    let modules = config::load_modules();
    let now = now_secs();
    let workers = hub.workers.lock().unwrap();
    let alive: Vec<&Worker> = workers.values().filter(|w| now - w.seen < 6.0).collect();
    let mut pcs = Vec::new();
    let mut warnings = Vec::new();
    let mut cam_views = Vec::new();
    let mut ops_total = 0.0;
    let mut lack_total = 0.0;
    let gflops: HashMap<&str, f32> = modules.iter().map(|m| (m.id.as_str(), m.gflops)).collect();
    let _ = gflops;
    // Operation cost: one pass is the sum of enabled detector modules' rough size.
    // Fight is free. The figure stays approximate on purpose.
    let pass_cost: f64 = modules.iter().filter(|m| m.enabled && m.kind != "fight").map(|m| m.gflops as f64).sum::<f64>() * 1.0e9;
    for w in &alive {
        let mut gated = 0.0;
        let mut blur = 0.0;
        let mut blank = 0.0;
        let mut missed = 0.0;
        let mut same = 0.0;
        let mut paced = 0.0;
        let mut passes = 0.0;
        let mut behind = 0.0;
        let mut ms_acc = 0.0;
        let mut ms_n = 0.0;
        let mut on_time = 0;
        let mut pc_warnings = 0;
        for (cid, cam) in &w.cameras {
            let g = cam.gated.unwrap_or(0.0);
            let b = cam.blur.unwrap_or(0.0);
            let k = cam.blank.unwrap_or(0.0);
            let m = cam.missed.unwrap_or(0.0);
            let still = cam.same.unwrap_or(0.0);
            let pace = cam.paced.unwrap_or(0.0);
            let lacking = m;
            let seen = cam.gated.is_some();
            let st = approx::status(&cam.error, seen, g, b, k, lacking);
            if st == "on time" {
                on_time += 1;
            }
            gated += g;
            blur += b;
            blank += k;
            missed += m;
            same += still;
            paced += pace;
            passes += cam.passes.unwrap_or(0.0);
            behind += cam.debt.unwrap_or(0.0);
            if cam.passes.unwrap_or(0.0) > 0.2 {
                ms_acc += cam.ms.unwrap_or(0.0);
                ms_n += 1.0;
            }
            let mut these = approx::cam_warnings(&cam.name, g, b, k, lacking, &cam.error);
            pc_warnings += these.len();
            warnings.append(&mut these);
            let show_boxes = st != "behind";
            cam_views.push(serde_json::json!({
                "id": cid,
                "name": cam.name,
                "status": st,
                "seq": cam.seq,
                "people": approx::people(cam.persons.unwrap_or(0.0)),
                "lacking": approx::fmt_num(lacking),
                "feed": match feeds.get(cid).copied().unwrap_or(0.0) {
                    fps if fps >= 1.0 => format!("{} fps", fps.round() as i32),
                    _ => String::new(),
                },
                "skip": if still + pace >= 1.0 { format!("{} skipped", approx::fmt_num(still + pace)) } else { String::new() },
                "host": w.host,
                "at": cam.at,
                "dur": cam.dur,
                "boxes": if show_boxes { cam.boxes.clone() } else { serde_json::json!([]) },
            }));
        }
        let ops = passes * if pass_cost > 0.0 { pass_cost } else { 0.0 };
        // passes is per second smoothed, pass_cost is ops per pass. Good.
        ops_total += ops;
        lack_total += missed;
        pcs.push(PcSample {
            name: w.host.clone(),
            cores: w.cores,
            cpu: w.cpu.unwrap_or(0.0),
            ram_used: w.ram_used.unwrap_or(0.0),
            ram_total: w.ram_total.unwrap_or(0.0),
            cameras: w.cameras.len(),
            on_time,
            gated,
            blur,
            blank,
            passes,
            ops,
            lacking: missed,
            missed,
            same,
            paced,
            behind,
            ms_pass: if ms_n > 0.0 { ms_acc / ms_n } else { 0.0 },
            warnings: pc_warnings,
            share: 0.0,
        });
    }
    for m in &modules {
        if m.enabled && m.kind != "fight" && m.model.is_empty() {
            warnings.push(format!("{} is on, and no model file is set.", m.name));
        }
    }
    {
        let mut hist = hub.history.lock().unwrap();
        let stale = hist.last().and_then(|p| p.get("t")).and_then(|t| t.as_f64()).map(|t| now - t > 0.8).unwrap_or(true);
        if stale {
            let by: Vec<serde_json::Value> = pcs.iter().map(|p| serde_json::json!({"name": p.name, "ops": p.ops, "lacking": p.lacking})).collect();
            hist.push(serde_json::json!({"t": now, "ops": ops_total, "lacking": lack_total, "by": by}));
            if hist.len() > 60 {
                let drop_n = hist.len() - 60;
                hist.drain(0..drop_n);
            }
        }
    }
    if ops_total > 0.0 {
        for pc in &mut pcs {
            pc.share = pc.ops / ops_total * 100.0;
        }
    }
    let (headers, rows) = approx::matrix(&pcs);
    let sentence = approx::sentence(alive.len(), ops_total, lack_total);
    let events: Vec<serde_json::Value> = hub
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .filter(|e| e.status != "rejected")
        .take(40)
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "ts": e.ts,
                "rule": e.rule,
                "camera": e.camera,
                "status": e.status,
                "note": e.note,
            })
        })
        .collect();
    serde_json::json!({
        "sentence": sentence,
        "footnote": approx::FOOTNOTE,
        "solo": *hub.solo.lock().unwrap(),
        "blur": watch.blur,
        "process": watch.process,
        "process_line": config::process_line(&watch.process),
        "figures": {
            "ops": approx::format_ops(ops_total),
            "lacking": approx::fmt_num(lack_total),
            "pcs": alive.len().to_string(),
            "warnings": warnings.len(),
        },
        "warnings": warnings,
        "headers": headers,
        "rows": rows.into_iter().map(|(label, cells)| serde_json::json!({"label": label, "cells": cells})).collect::<Vec<_>>(),
        "cameras": cam_views,
        "events": events,
        "seen": hub.seen.lock().unwrap().iter().rev().take(80).map(|s| serde_json::json!({
            "id": s.id,
            "ts": s.ts,
            "at": s.at,
            "camera": s.camera,
            "camera_id": s.camera_id,
            "class": s.class,
            "color": s.color,
        })).collect::<Vec<_>>(),
        "searches": config::load_searches(),
        "rules": rules,
        "modules": modules,
        "ffmpeg": Command::new("ffmpeg").arg("-version").output().is_ok(),
        "history": hub.history.lock().unwrap().clone(),
    })
}

fn query(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return Some(urlencoding_lite(v));
        }
    }
    None
}

fn urlencoding_lite(s: &str) -> String {
    let s = s.replace('+', " ");
    percent_decode(&s).unwrap_or(s)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn json(v: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    bytes(v.to_string().into_bytes(), "application/json")
}

fn json_err(msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    bytes_status(serde_json::json!({"ok": false, "error": msg}).to_string().into_bytes(), "application/json", StatusCode(400))
}

fn ui_page(path: &str) -> Option<Response<std::io::Cursor<Vec<u8>>>> {
    let (name, ctype) = match path {
        "/" | "/wall" => ("wall.html", "text/html; charset=utf-8"),
        "/find" => ("find.html", "text/html; charset=utf-8"),
        "/analytics" => ("analytics.html", "text/html; charset=utf-8"),
        "/setup" => ("setup.html", "text/html; charset=utf-8"),
        "/clip" => ("clip.html", "text/html; charset=utf-8"),
        "/ui/app.css" => ("app.css", "text/css; charset=utf-8"),
        "/ui/app.js" => ("app.js", "text/javascript; charset=utf-8"),
        "/ui/wall.js" => ("wall.js", "text/javascript; charset=utf-8"),
        "/ui/find.js" => ("find.js", "text/javascript; charset=utf-8"),
        "/ui/clip.js" => ("clip.js", "text/javascript; charset=utf-8"),
        "/ui/analytics.js" => ("analytics.js", "text/javascript; charset=utf-8"),
        "/ui/setup.js" => ("setup.js", "text/javascript; charset=utf-8"),
        _ => return None,
    };
    // The files in rust/src/watch/ui are the pages. Read them so an edit shows up on refresh.
    let disk = crate::watch::repo_root().join("rust/src/watch/ui").join(name);
    if let Ok(data) = std::fs::read(&disk) {
        return Some(bytes(data, ctype));
    }
    let embedded: &[u8] = match name {
        "wall.html" => include_str!("ui/wall.html").as_bytes(),
        "find.html" => include_str!("ui/find.html").as_bytes(),
        "analytics.html" => include_str!("ui/analytics.html").as_bytes(),
        "setup.html" => include_str!("ui/setup.html").as_bytes(),
        "clip.html" => include_str!("ui/clip.html").as_bytes(),
        "app.css" => include_str!("ui/app.css").as_bytes(),
        "app.js" => include_str!("ui/app.js").as_bytes(),
        "wall.js" => include_str!("ui/wall.js").as_bytes(),
        "find.js" => include_str!("ui/find.js").as_bytes(),
        "clip.js" => include_str!("ui/clip.js").as_bytes(),
        "analytics.js" => include_str!("ui/analytics.js").as_bytes(),
        "setup.js" => include_str!("ui/setup.js").as_bytes(),
        _ => return None,
    };
    Some(bytes(embedded.to_vec(), ctype))
}

fn bytes(data: Vec<u8>, ctype: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    bytes_status(data, ctype, StatusCode(200))
}

fn bytes_status(data: Vec<u8>, ctype: &str, code: StatusCode) -> Response<std::io::Cursor<Vec<u8>>> {
    let header = Header::from_bytes(b"Content-Type", ctype.as_bytes()).unwrap();
    let cache = Header::from_bytes(b"Cache-Control", b"no-store").unwrap();
    Response::from_data(data).with_status_code(code).with_header(header).with_header(cache)
}

fn empty(code: StatusCode) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(Vec::new()).with_status_code(code)
}

use crate::watch::rules;
