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
use crate::watch::split::Node;
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
    completed: bool,
    indexed_frames: u64,
    seq: u64,
    boxes: serde_json::Value,
    ops: serde_json::Value,
}

struct Preview {
    jpeg: Vec<u8>,
    seq: u64,
    fps: f64,
    mark: f64,
    count: u32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Event {
    id: u64,
    ts: f64,
    key: String,
    rule: String,
    camera: String,
    status: String,
    note: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Seen {
    id: u64,
    ts: f64,
    camera: String,
    camera_id: String,
    class: String,
    color: String,
    #[serde(skip)]
    jpeg: Vec<u8>,
    at: f64,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    model: String,
    #[serde(default)]
    track_id: u64,
    #[serde(default)]
    near_person: bool,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    thumbnail: String,
}

struct Hub {
    journal: Mutex<()>,
    index_batches: Mutex<HashMap<String, u64>>,
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
    api: Arc<crate::watch::api::Api>,
}

pub fn run(opts: Opts) -> Result<(), String> {
    let _ = started_secs();
    let root = crate::watch::repo_root();
    std::fs::create_dir_all(data_path("")).map_err(|e| e.to_string())?;
    let store = crate::watch::storage::open(&data_path("watch.db")).map_err(|e| format!("metadata store: {}", e.message))?;
    let store = crate::watch::shard::wrap_store(store, &data_path("")); // CW_SHARDS=1: read through the federated store
    let auth = crate::watch::auth::Auth::load(&root);
    log(&auth.startup_notice());
    let api = Arc::new(crate::watch::api::Api::new(store, data_path(""), auth));
    for (id, r) in crate::watch::api::register_models(&api) {
        if let Err(e) = r { log(&format!("module {id}: {}", e.message)); }
    }
    crate::watch::api::sync_sources(&api, &config::load_watch().cameras);
    // Old-format journals (seen.jsonl, events.jsonl) are imported only on request: CW_IMPORT_LEGACY=1.
    // They hold sample records from before the database existed and show up as bogus "legacy:" cameras.
    if std::env::var("CW_IMPORT_LEGACY").is_ok_and(|v| v == "1") {
        thread::spawn({
            let api = api.clone();
            move || match crate::watch::storage::legacy::import(&*api.store, &data_path("")) {
                Ok(r) => log(&format!("legacy index: {} imported, {} already present, {} malformed", r.imported, r.duplicates, r.malformed)),
                Err(e) => log(&format!("legacy import failed: {}", e.message)),
            }
        });
    }
    thread::spawn({
        let api = api.clone();
        move || loop { thread::sleep(Duration::from_secs(30)); api.save_alert_state(); }
    });
    let hub = Arc::new(Hub {
        api: api.clone(),
        journal: Mutex::new(()),
        index_batches: Mutex::new(HashMap::new()),
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
    let events: Vec<Event> = read_journal("events.jsonl").into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
    let mut latest = std::collections::BTreeMap::new();
    for event in events { latest.insert(event.id, event); }
    *hub.next_event.lock().unwrap() = latest.keys().max().copied().unwrap_or(0) + 1;
    *hub.events.lock().unwrap() = latest.into_values().collect();
    audit(&hub, "server.start", "Vision query service started");
    thread::spawn({
        let hub = hub.clone();
        move || watch_pictures(hub)
    });
    thread::spawn({
        let name = opts.name.clone();
        let port = opts.port;
        move || crate::watch::net::beacon(name, port)
    });
    // The Cluster page listens for this instead of polling.
    thread::spawn({
        let api = api.clone();
        move || loop {
            thread::sleep(Duration::from_secs(2));
            let rt = crate::watch::fleet::realtime(&api);
            if rt["cameras"].as_array().is_some_and(|c| !c.is_empty()) || rt["workers"].as_array().is_some_and(|w| !w.is_empty()) {
                api.bus.publish("realtime", &rt);
            }
        }
    });
    if !opts.no_local {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(exe);
        cmd.env("CW_WORKER_TOKEN", hub.api.auth.ephemeral_worker_token());
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
    // Without configured users the service is open: keep it on loopback unless told otherwise.
    let bind = match std::env::var("CW_BIND") {
        Ok(b) => b,
        Err(_) if hub.api.auth.mode() == "open-trusted-lan" => "127.0.0.1".into(),
        Err(_) => "0.0.0.0".into(),
    };
    let ip = if bind == "0.0.0.0" { primary_ip() } else { bind.clone() };
    log(&format!("watch  http://{ip}:{}", opts.port));
    if bind == "127.0.0.1" { log("open mode: loopback only. Set CW_BIND=0.0.0.0 for remote workers, after bootstrapping an admin."); }
    else { log("other PCs: kit rs watch worker"); }
    let server = Server::http(format!("{bind}:{}", opts.port)).map_err(|e| e.to_string())?;
    for mut req in server.incoming_requests() {
        let hub = hub.clone();
        thread::spawn(move || {
            let method = req.method().clone();
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or(&url).to_string();
            let path = path.as_str();
            let headers: Vec<(String, String)> = req.headers().iter().map(|h| (h.field.as_str().as_str().to_string(), h.value.as_str().to_string())).collect();
            let principal = match hub.api.auth.check(method.as_str(), path, &headers) {
                Ok(p) => p,
                Err(e) => { let _ = req.respond(v1_reply(crate::watch::api::Reply::err(e))); return; }
            };
            if path == "/api/v1/events" && method == Method::Get {
                let cursor = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("Last-Event-ID")).map(|(_, v)| v.clone())
                    .or_else(|| crate::watch::api::param(&url, "cursor")).and_then(|c| c.parse().ok()).unwrap_or(0);
                let body = crate::watch::api::SseBody::new(hub.api.clone(), cursor);
                let h = vec![Header::from_bytes(b"Content-Type", b"text/event-stream").unwrap(), Header::from_bytes(b"Cache-Control", b"no-store").unwrap()];
                let _ = req.respond(Response::new(StatusCode(200), h, body, None, None));
                return;
            }
            if method == Method::Get {
                // Footage for a worker that has no copy: streamed from the file, any byte range.
                if let Some(id) = path.strip_prefix("/api/v1/worker/media/").and_then(|r| r.strip_suffix("/stream")) {
                    let range = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("Range")).map(|(_, v)| v.clone());
                    match worker_media(&hub, id, range.as_deref()) {
                        Ok(r) => { let _ = req.respond(r); }
                        Err(e) => { let _ = req.respond(v1_reply(crate::watch::api::Reply::err(e))); }
                    }
                    return;
                }
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
            let auth_out = match (method.as_str(), path) {
                ("POST", "/api/v1/auth/login") => Some(hub.api.auth.login(&body, &req.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default())),
                ("POST", "/api/v1/auth/logout") => Some(hub.api.auth.logout(&headers)),
                ("POST", "/api/v1/auth/bootstrap") => Some(hub.api.auth.bootstrap_http(&body)),
                ("GET", "/api/v1/auth/me") => Some(hub.api.auth.me(&headers)),
                ("POST", "/api/v1/auth/users") => Some(hub.api.auth.create_user_http(&headers, &body)),
                ("POST", "/api/v1/auth/worker-tokens") => Some(hub.api.auth.create_worker_token_http(&headers, &body)),
                ("POST", "/api/v1/auth/password") => Some(hub.api.auth.change_password_http(&headers, &body)),
                _ => None,
            };
            if let Some((status, hs, text)) = auth_out {
                let mut r = Response::from_data(text.into_bytes()).with_status_code(StatusCode(status));
                let mut typed = false;
                for (k, v) in hs { typed |= k.eq_ignore_ascii_case("Content-Type"); if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) { r = r.with_header(h); } }
                if !typed { r = r.with_header(Header::from_bytes(b"Content-Type", b"application/json").unwrap()); }
                let _ = req.respond(r);
                return;
            }
            let range = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("Range")).map(|(_, v)| v.clone());
            let body = inject_actor(&method, path, body, &principal);
            if let Some(r) = crate::watch::join::handle(&hub.api, method.as_str(), &url, &body, &headers) {
                let _ = req.respond(v1_reply(r));
                return;
            }
            if let Some(r) = crate::watch::api::handle(&hub.api, method.as_str(), &url, &body, range.as_deref()) {
                let _ = req.respond(v1_reply(r));
                return;
            }
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
            .filter(|c| !matches!(c.kind.as_str(), "archive" | "image"))
            // A replayed recording (analyze:false) is played by the browser directly; no preview stream needed.
            .filter(|c| c.analyze)
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
    // Browsers ask for an icon on every page; there is none, and a 404 would show as a console error.
    if path == "/favicon.ico" { return empty(StatusCode(204)); }
    if path == "/api/health" {
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/index" && method == &Method::Post {
        return match ingest_index(hub, body) {
            Ok(n) => json(serde_json::json!({"ok":true,"indexed":n})), Err(e) => json_err(&e),
        };
    }
    if path == "/api/sources" {
        if method == &Method::Get {
            let raw = std::fs::read_to_string(config::watch_path()).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).unwrap_or(serde_json::json!({}));
            return json(serde_json::json!({"sources":raw.get("cameras").cloned().unwrap_or(serde_json::json!([])),"expanded":config::load_watch().cameras,"epoch":started_secs()}));
        }
        if method == &Method::Post { return save_sources(hub, body); }
    }
    if path == "/api/capabilities" && method == &Method::Get { return json(capabilities()); }
    if path == "/api/logs" && method == &Method::Get {
        let _journal = hub.journal.lock().unwrap();
        let logs: Vec<_> = read_journal("audit.jsonl").into_iter().rev().take(500).collect();
        let records = load_log().0.len();
        drop(_journal);
        return json(serde_json::json!({"logs":logs,"events":*hub.events.lock().unwrap(),"index":{"records":records,"storage":"JSONL","query_strategy":"scan durable metadata"}}));
    }
    if path == "/api/query" && method == &Method::Post { return run_sql(hub, body); }
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
                if !hit.jpeg.is_empty() { return bytes(hit.jpeg.clone(), "image/jpeg"); }
                if !hit.thumbnail.is_empty() {
                    if let Ok(data) = std::fs::read(data_path("thumbs").join(&hit.thumbnail)) { return bytes(data, "image/jpeg"); }
                }
            }
            drop(hub.seen.lock().unwrap());
            if let Some(hit) = load_log().0.into_iter().find(|s| s.id == id) {
                if !hit.thumbnail.is_empty() {
                    if let Ok(data) = std::fs::read(data_path("thumbs").join(hit.thumbnail)) { return bytes(data, "image/jpeg"); }
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
                    Ok(()) => { audit(hub, "configuration.save", path); json(serde_json::json!({"ok": true})) },
                    Err(e) => json_err(&e),
                }
            }
            Err(e) => json_err(&e.to_string()),
        };
    }
    if path == "/api/modules" && method == &Method::Post {
        return match serde_json::from_slice::<Vec<Module>>(body) {
            Ok(m) => match config::save_json(&config::modules_path(), &m) {
                Ok(()) => { audit(hub, "configuration.save", path); json(serde_json::json!({"ok": true})) },
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
            if let Some(m) = v.get("detect_mode").and_then(|x| x.as_str()) {
                proc.detect_mode = m.to_string();
            }
            if let Some(n) = v.get("motion_sensitivity").and_then(|x| x.as_f64()) {
                proc.motion_sensitivity = n as f32;
            }
            if let Some(n) = v.get("motion_min_area").and_then(|x| x.as_f64()) {
                proc.motion_min_area = n as f32;
            }
            if let Some(n) = v.get("region_margin").and_then(|x| x.as_f64()) {
                proc.region_margin = n as f32;
            }
            if let Some(n) = v.get("detect_interval").and_then(|x| x.as_u64()) {
                proc.detect_interval = n as u32;
            }
            if let Some(n) = v.get("keyframe_secs").and_then(|x| x.as_f64()) {
                proc.keyframe_secs = n as f32;
            }
            if let Some(n) = v.get("max_regions").and_then(|x| x.as_u64()) {
                proc.max_regions = n as u32;
            }
            if let Some(n) = v.get("segment_secs").and_then(|x| x.as_u64()) {
                proc.segment_secs = n as u32;
            }
        }
        if let Err(e) = config::save_process(&proc, watch.copies) { return json_err(&e); }
        audit(hub, "settings.save", "Processing or execution mode changed");
        return json(serde_json::json!({"ok": true}));
    }
    if path == "/api/find" && method == &Method::Post {
        return json(run_find(hub, body));
    }
    if path == "/api/searches" && method == &Method::Post {
        return match serde_json::from_slice::<Vec<config::SavedSearch>>(body) {
            Ok(list) => match config::save_json(&config::searches_path(), &list) {
                Ok(()) => { audit(hub, "configuration.save", path); json(serde_json::json!({"ok": true})) },
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
    let cores = cores.max(1);
    if !id.is_empty() {
        crate::watch::fleet::note_poll(&hub.api, &id, &host, cores, local, &query(url, "label").unwrap_or_default());
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
    let _ = nodes;
    crate::watch::api::sync_sources(&hub.api, &watch.cameras);
    // Solo keeps every camera on the head's own worker.
    let leases = if id.is_empty() { HashMap::new() } else { crate::watch::api::leases_for(&hub.api, &id, &host_of(hub, &id), cores, !solo || local) };
    let mine = crate::watch::api::cameras_for(&hub.api, &leases, &watch.cameras);
    serde_json::json!({
        "leases": leases,
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
    crate::watch::fleet::note_stats(&hub.api, &v);
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
                completed: false,
                indexed_frames: 0,
                seq: 0,
                boxes: serde_json::json!([]),
                ops: serde_json::json!({}),
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
            entry.completed = cam.get("completed").and_then(|v|v.as_bool()).unwrap_or(false);
            entry.indexed_frames = cam.get("indexed_frames").and_then(|v|v.as_u64()).unwrap_or(0);
            entry.seq = cam.get("seq").and_then(|x| x.as_u64()).unwrap_or(entry.seq);
            entry.at = cam.get("at").and_then(|x| x.as_f64()).unwrap_or(entry.at);
            entry.dur = cam.get("dur").and_then(|x| x.as_f64()).unwrap_or(entry.dur);
            entry.boxes = cam.get("boxes").cloned().unwrap_or(serde_json::json!([]));
            if let Some(o) = cam.get("ops") { entry.ops = o.clone(); }
            let state = cam.get("state").and_then(|x| x.as_str()).unwrap_or(if entry.completed { "completed" } else { "running" });
            crate::watch::api::camera_state(&hub.api, id, cid, state, cam.get("error_code").and_then(|x| x.as_str()).unwrap_or(""), cam.get("coverage"), cam.get("ops"));

        }
    }
    drop(workers);

}

/// Every inference batch is journaled before acknowledgment. Stats are display-only.
fn ingest_index(hub: &Hub, body: &[u8]) -> Result<usize, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let cid = v["camera_id"].as_str().filter(|s| !s.is_empty()).ok_or("camera_id required")?;
    let camera = v["camera"].as_str().unwrap_or(cid);
    let seq = v["seq"].as_u64().ok_or("seq required")?;
    let at = v["at"].as_f64().filter(|n| n.is_finite() && *n >= 0.0).ok_or("nonnegative at required")?;
    let ts = v["ts"].as_f64().filter(|n| n.is_finite() && *n > 0.0).ok_or("positive ts required")?;
    let detections = v["detections"].as_array().ok_or("detections array required")?;
    if detections.len() > 10000 { return Err("batch exceeds 10000 detections".into()); }
    // A timestamp changes across runs, while retries retain it. Do not merge different people.
    let key = format!("{cid}|{ts:.6}|{seq}");
    let _journal = hub.journal.lock().unwrap();
    let mut batches = hub.index_batches.lock().unwrap();
    if batches.contains_key(&key) { return Ok(0); }
    let mut seen = hub.seen.lock().unwrap();
    let mut next = hub.next_seen.lock().unwrap();
    let mut added = Vec::new();
    for b in detections {
        let class = b["class"].as_str().filter(|s| !s.is_empty()).ok_or("detection class required")?;
        let confidence = b["confidence"].as_f64().filter(|n| (0.0..=1.0).contains(n)).ok_or("confidence must be 0..1")? as f32;
        added.push(Seen {
            id: *next + added.len() as u64, ts, at, seq,
            camera: camera.into(), camera_id: cid.into(), class: class.into(),
            color: b["color"].as_str().unwrap_or("").into(),
            confidence, model: b["model"].as_str().unwrap_or("").into(),
            track_id: b["track_id"].as_u64().unwrap_or(0),
            near_person: b["near_person"].as_bool().unwrap_or_else(|| b["near_person"].as_u64().is_some()),
            thumbnail: String::new(),
            jpeg: Vec::new(),
        });
    }
    if let Some(hex) = v["thumbnail_hex"].as_str().filter(|s| !s.is_empty()) {
        let jpeg = decode_hex(hex)?;
        if jpeg.len() > 300000 { return Err("thumbnail exceeds 300KB".into()); }
        if !jpeg.starts_with(&[0xff,0xd8]) || !jpeg.ends_with(&[0xff,0xd9]) { return Err("thumbnail must be JPEG".into()); }
        let filename = format!("{}.jpg", *next);
        let dir = data_path("thumbs");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(&filename), &jpeg).map_err(|e| e.to_string())?;
        for row in &mut added { row.thumbnail = filename.clone(); }
        publish_frame(hub, cid, jpeg);
    }
    let lines: Vec<_> = added.iter().map(|s| serde_json::to_value(s).unwrap()).collect();
    append_journal("seen.jsonl", &lines)?;
    *next += added.len() as u64;
    let count = added.len();
    seen.extend(added);
    // Bound JPEG memory; durable records retain media offsets for evidence playback.
    if seen.len() > 100000 { let n = seen.len()-100000; seen.drain(..n); }
    if batches.len() > 10000 { batches.clear(); }
    batches.insert(key, seq);
    Ok(count)
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
    let _journal = hub.journal.lock().unwrap();
    let (seen, _) = load_log();
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
            confidence: s.confidence, model: s.model.clone(), track_id: s.track_id, near_person: s.near_person,
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
                        "class": s.class, "color": s.color, "confidence":s.confidence, "model":s.model, "track_id":s.track_id, "near_person":s.near_person,
                    }));
                }
            }
            serde_json::json!({"hits": out, "error": ""})
        }
        Err(e) => serde_json::json!({"hits": [], "error": e}),
    }
}

fn data_path(name: &str) -> std::path::PathBuf {
    crate::watch::repo_root().join("vision/data").join(name)
}

fn read_journal(name: &str) -> Vec<serde_json::Value> {
    use std::io::BufRead;
    let Ok(file) = File::open(data_path(name)) else { return Vec::new() };
    std::io::BufReader::new(file).lines().map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok()).collect()
}

fn load_log() -> (Vec<Seen>, u64) {
    let list: Vec<Seen> = read_journal("seen.jsonl").into_iter()
        .filter_map(|v| serde_json::from_value(v).ok()).collect();
    let next = list.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    (list, next)
}

fn append_journal(name: &str, lines: &[serde_json::Value]) -> Result<(), String> {
    use std::io::Write;
    let path = data_path(name);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    let mut batch = Vec::new();
    for line in lines { writeln!(batch, "{line}").map_err(|e| e.to_string())?; }
    file.write_all(&batch).and_then(|_| file.sync_data()).map_err(|e| e.to_string())
}

fn audit(hub: &Hub, action: &str, detail: &str) {
    let _journal = hub.journal.lock().unwrap();
    if let Err(e) = append_journal("audit.jsonl", &[serde_json::json!({"ts":now_secs(),"action":action,"detail":detail})]) {
        log(&format!("audit write failed: {e}"));
    }
}

fn media_path(id: &str) -> Option<std::path::PathBuf> {
    let id = id.split('?').next().unwrap_or(id);
    let watch = config::load_watch();
    let cam = watch.cameras.iter().find(|c| c.id == id)?;
    if !matches!(cam.kind.as_str(), "file" | "archive") {
        return None;
    }
    let path = std::path::PathBuf::from(config::resolve_url(&cam.url));
    if path.is_file() { Some(path) } else { None }
}

pub(crate) fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
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

/// `GET /api/v1/worker/media/{id}/stream`: the recording as a stream, honouring `Range`, without buffering it.
fn worker_media(hub: &Hub, id: &str, range: Option<&str>) -> Result<Response<Box<dyn Read + Send>>, crate::watch::domain::ApiError> {
    use crate::watch::domain::ApiError;
    let (_, path) = crate::watch::api::media_file(&hub.api, id)?;
    let mut file = File::open(&path).map_err(|e| ApiError::new("unavailable", e.to_string()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 { return Err(ApiError::new("unavailable", "media file is empty")); }
    let (start, end, status) = match range.and_then(|h| h.trim().strip_prefix("bytes=")).and_then(|r| r.split_once('-')) {
        Some((a, b)) => {
            let start: u64 = if a.is_empty() { 0 } else { a.parse().map_err(|_| ApiError::new("invalid_request", "bad Range"))? };
            let end = if b.is_empty() { len - 1 } else { b.parse::<u64>().map_err(|_| ApiError::new("invalid_request", "bad Range"))?.min(len - 1) };
            if start >= len || start > end { return Err(ApiError::new("invalid_request", "unsatisfiable range")); }
            (start, end, 206)
        }
        None => (0, len - 1, 200),
    };
    file.seek(SeekFrom::Start(start)).map_err(|e| ApiError::new("unavailable", e.to_string()))?;
    let n = end - start + 1;
    let mut headers = vec![Header::from_bytes(b"Content-Type", b"video/mp4").unwrap(), Header::from_bytes(b"Accept-Ranges", b"bytes").unwrap(), Header::from_bytes(b"Cache-Control", b"no-store").unwrap()];
    if status == 206 { headers.push(Header::from_bytes(b"Content-Range", format!("bytes {start}-{end}/{len}").as_bytes()).unwrap()); }
    let body: Box<dyn Read + Send> = Box::new(file.take(n));
    Ok(Response::new(StatusCode(status), headers, body, Some(n as usize), None))
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
    let records: Vec<_> = events.iter().map(|e| serde_json::to_value(e).unwrap()).collect();
    drop(events);
    let _journal = hub.journal.lock().unwrap();
    if let Err(e) = append_journal("events.jsonl", &records) { log(&format!("event persistence failed: {e}")); }
    let mut events = hub.events.lock().unwrap();
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
    let record = events.iter().find(|e| e.id == id).map(|e| serde_json::to_value(e).unwrap());
    drop(events);
    if let Some(record) = record {
        let _journal = hub.journal.lock().unwrap();
        if let Err(e) = append_journal("events.jsonl", &[record]) { log(&format!("event persistence failed: {e}")); }
        drop(_journal);
        audit(hub, "alert.review", &format!("Alert {id}: {verdict}"));
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
        for (cid, cam) in w.cameras.iter().filter(|(cid, _)| !cid.starts_with("bench-")) {
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
                "error": cam.error,
                "seq": cam.seq,
                "completed":cam.completed,
                "indexed_frames":cam.indexed_frames,
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
                "ops": cam.ops,
                // Flat copies for the Rules page: detector passes, static skips, regions, keyframes, pixels.
                "detector_passes": cam.ops.get("detector_passes"), "static": cam.ops.get("static"), "regions": cam.ops.get("regions"),
                "keyframes": cam.ops.get("keyframes"), "pixels_inferred": cam.ops.get("pixels_inferred"), "pixels_full": cam.ops.get("pixels_full"),
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
    // Finished imports are no longer reported by any worker; keep them on the wall and in the savings panel.
    {
        let have: std::collections::HashSet<String> = cam_views.iter().filter_map(|v| v["id"].as_str().map(String::from)).collect();
        for c in &watch.cameras {
            if !have.contains(&c.id) {
                if let Some(v) = crate::watch::api::finished_camera(&hub.api, &c.id, &c.name, !c.analyze) { cam_views.push(v); }
            }
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
        "/sources" => ("sources.html", "text/html; charset=utf-8"),
        "/logs" => ("logs.html", "text/html; charset=utf-8"),
        "/ui/sources.js" => ("sources.js", "text/javascript; charset=utf-8"),
        "/ui/logs.js" => ("logs.js", "text/javascript; charset=utf-8"),
        "/analytics" => ("analytics.html", "text/html; charset=utf-8"),
        "/join" => ("join.html", "text/html; charset=utf-8"),
        "/ui/join.js" => ("join.js", "text/javascript; charset=utf-8"),
        "/ui/cluster.js" => ("cluster.js", "text/javascript; charset=utf-8"),
        "/ui/cluster.css" => ("cluster.css", "text/css; charset=utf-8"),
        "/setup" => ("setup.html", "text/html; charset=utf-8"),
        "/clip" => ("clip.html", "text/html; charset=utf-8"),
        "/review" => ("review.html", "text/html; charset=utf-8"),
        "/login" => ("login.html", "text/html; charset=utf-8"),
        "/ui/review.js" => ("review.js", "text/javascript; charset=utf-8"),
        "/ui/review.css" => ("review.css", "text/css; charset=utf-8"),
        "/ui/timeline.js" => ("timeline.js", "text/javascript; charset=utf-8"),
        "/ui/timeline.css" => ("timeline.css", "text/css; charset=utf-8"),
        "/ui/product.css" => ("product.css", "text/css; charset=utf-8"),
        "/ui/fixtures/review.json" => ("fixtures/review.json", "application/json"),
        "/ui/app.css" => ("app.css", "text/css; charset=utf-8"),
        "/ui/app.js" => ("app.js", "text/javascript; charset=utf-8"),
        "/ui/wall.js" => ("wall.js", "text/javascript; charset=utf-8"),
        "/ui/find.js" => ("find.js", "text/javascript; charset=utf-8"),
        "/ui/clip.js" => ("clip.js", "text/javascript; charset=utf-8"),
        "/ui/analytics.js" => ("analytics.js", "text/javascript; charset=utf-8"),
        "/ui/setup.js" => ("setup.js", "text/javascript; charset=utf-8"),
        "/ui/nodes.js" => ("nodes.js", "text/javascript; charset=utf-8"),
        "/ui/nodes.css" => ("nodes.css", "text/css; charset=utf-8"),
        "/nodes" => ("nodes.html", "text/html; charset=utf-8"),
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
        "sources.html" => include_str!("ui/sources.html").as_bytes(),
        "logs.html" => include_str!("ui/logs.html").as_bytes(),
        "sources.js" => include_str!("ui/sources.js").as_bytes(),
        "logs.js" => include_str!("ui/logs.js").as_bytes(),
        "analytics.html" => include_str!("ui/analytics.html").as_bytes(),
        "join.html" => include_str!("ui/join.html").as_bytes(),
        "join.js" => include_str!("ui/join.js").as_bytes(),
        "cluster.js" => include_str!("ui/cluster.js").as_bytes(),
        "cluster.css" => include_str!("ui/cluster.css").as_bytes(),
        "setup.html" => include_str!("ui/setup.html").as_bytes(),
        "clip.html" => include_str!("ui/clip.html").as_bytes(),
        "review.html" => include_str!("ui/review.html").as_bytes(),
        "login.html" => include_str!("ui/login.html").as_bytes(),
        "review.js" => include_str!("ui/review.js").as_bytes(),
        "review.css" => include_str!("ui/review.css").as_bytes(),
        "timeline.js" => include_str!("ui/timeline.js").as_bytes(),
        "timeline.css" => include_str!("ui/timeline.css").as_bytes(),
        "product.css" => include_str!("ui/product.css").as_bytes(),
        "fixtures/review.json" => include_str!("ui/fixtures/review.json").as_bytes(),
        "app.css" => include_str!("ui/app.css").as_bytes(),
        "app.js" => include_str!("ui/app.js").as_bytes(),
        "wall.js" => include_str!("ui/wall.js").as_bytes(),
        "find.js" => include_str!("ui/find.js").as_bytes(),
        "clip.js" => include_str!("ui/clip.js").as_bytes(),
        "analytics.js" => include_str!("ui/analytics.js").as_bytes(),
        "setup.js" => include_str!("ui/setup.js").as_bytes(),
        "nodes.js" => include_str!("ui/nodes.js").as_bytes(),
        "nodes.css" => include_str!("ui/nodes.css").as_bytes(),
        "nodes.html" => include_str!("ui/nodes.html").as_bytes(),
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

fn save_sources(hub: &Hub, body: &[u8]) -> Response<std::io::Cursor<Vec<u8>>> {
    let list = match serde_json::from_slice::<Vec<Camera>>(body) { Ok(v) => v, Err(e) => return json_err(&e.to_string()) };
    let mut ids = std::collections::HashSet::new();
    for c in &list {
        if c.id.is_empty() || !c.id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
            return json_err("Source IDs must contain only letters, digits, underscores or hyphens");
        }
        if !ids.insert(&c.id) { return json_err("Source IDs must be unique"); }
        if c.name.trim().is_empty() || c.url.trim().is_empty() { return json_err("Source name and location are required"); }
        if !matches!(c.kind.as_str(), "live" | "webcam" | "file" | "archive" | "image" | "folder") {
            return json_err("Source kind must be live, webcam, archive, image, folder or file (looping demo)");
        }
        if !c.start.is_finite() || !(0.0..1.0).contains(&c.start) { return json_err("Start must be a fraction from 0 (inclusive) to 1 (exclusive)"); }
        for z in &c.zones {
            if z.rect.iter().any(|n| !n.is_finite() || !(0.0..=1.0).contains(n)) || z.rect[2] <= 0.0 || z.rect[3] <= 0.0 || z.rect[0]+z.rect[2]>1.0 || z.rect[1]+z.rect[3]>1.0 {
                return json_err("Zone rectangles must fit within normalized image coordinates");
            }
        }
    }
    let mut v = std::fs::read_to_string(config::watch_path()).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).unwrap_or(serde_json::json!({}));
    if !v.is_object() { return json_err("watch.json must be an object"); }
    v["cameras"] = serde_json::to_value(&list).unwrap();
    // An explicit empty source list must stay empty instead of creating sample feeds.
    v["configured"] = serde_json::json!(true);
    match config::save_json(&config::watch_path(), &v) {
        Ok(()) => { audit(hub, "sources.save", &format!("Saved {} sources", list.len())); json(serde_json::json!({"ok":true,"count":list.len()})) }
        Err(e) => json_err(&e),
    }
}

fn capabilities() -> serde_json::Value {
    let modules: Vec<_> = config::load_modules().into_iter().map(|m| {
        let path = config::resolve_url(&m.model);
        serde_json::json!({"id":m.id,"name":m.name,"kind":m.kind,"enabled":m.enabled,"available":!m.model.is_empty() && std::path::Path::new(&path).is_file(),"labels":m.labels,"model":m.model})
    }).collect();
    serde_json::json!({
        "sources":["live","webcam","archive","image","folder"],
        "query":{"table":"sightings","language":"SELECT subset; unsupported syntax returns an error","count_unit":"indexed detections, not distinct people","history":"Durable JSONL metadata; queries scan the journal"},
        "modules":modules,
        "relationships":{"near":"Spatial proximity heuristic; does not prove holding"},
        "accuracy":"Model confidence is not an accuracy guarantee. Validate thresholds and recall on labeled footage. Small phones and occluded objects can be missed.",
        "auth":"No login. Trusted LAN demonstration only. Do not expose this service to the internet.",
        "cluster":"Camera affinity, weighted by physical cores; paths and models must be accessible on each worker. One camera runs on one worker.",
        "archive":"Finite scan with backpressure; processing presets still control which frames are sampled. Use Accuracy with filters off to scan every decoded frame."
    })
}

fn run_sql(hub: &Hub, body: &[u8]) -> Response<std::io::Cursor<Vec<u8>>> {
    let v: serde_json::Value = match serde_json::from_slice(body) { Ok(v)=>v, Err(e)=>return json_err(&e.to_string()) };
    let Some(statement) = v["sql"].as_str() else { return json_err("sql string required") };
    let _journal = hub.journal.lock().unwrap();
    let (seen, _) = load_log();
    let hits: Vec<_> = seen.iter().map(|s| crate::watch::query::HitIn {
        id:s.id, ts:s.ts, at:s.at, camera:s.camera.clone(), camera_id:s.camera_id.clone(),
        class:s.class.clone(), color:s.color.clone(), confidence:s.confidence, model:s.model.clone(), track_id:s.track_id, near_person:s.near_person,
    }).collect();
    match crate::watch::query::sql(&hits, statement) {
        Ok(result) => {
            let ids: std::collections::HashSet<_> = result.ids.iter().copied().collect();
            let rows: Vec<_> = seen.iter().filter(|s| ids.contains(&s.id)).map(|s| serde_json::to_value(s).unwrap()).collect();
            json(serde_json::json!({"result":result,"rows":rows,"count_unit":"indexed detections"}))
        }
        Err(e) => json_err(&e),
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() > 600000 || s.len()%2 != 0 { return Err("invalid thumbnail hex length".into()); }
    s.as_bytes().chunks_exact(2).map(|p| {
        let text = std::str::from_utf8(p).map_err(|_| "invalid thumbnail hex".to_string())?;
        u8::from_str_radix(text,16).map_err(|_| "invalid thumbnail hex".to_string())
    }).collect()
}

fn host_of(hub: &Hub, id: &str) -> String {
    hub.workers.lock().unwrap().get(id).map(|w| w.host.clone()).unwrap_or_else(|| "pc".into())
}

fn v1_reply(r: crate::watch::api::Reply) -> Response<std::io::Cursor<Vec<u8>>> {
    let n = r.body.len();
    let mut out = Response::new(StatusCode(r.status), vec![Header::from_bytes(b"Content-Type", r.ctype.as_bytes()).unwrap()], std::io::Cursor::new(r.body), Some(n), None).with_chunked_threshold(usize::MAX);
    let mut cached = false;
    for (k, v) in r.headers {
        cached |= k.eq_ignore_ascii_case("Cache-Control");
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) { out = out.with_header(h); }
    }
    if !cached { out = out.with_header(Header::from_bytes(b"Cache-Control", b"no-store").unwrap()); }
    out
}

/// Audited v1 mutations carry the authenticated actor, not whatever the client claims.
fn inject_actor(method: &Method, path: &str, body: Vec<u8>, p: &crate::watch::auth::Principal) -> Vec<u8> {
    if p.anonymous || p.worker || method == &Method::Get || !path.starts_with("/api/v1/") { return body; }
    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(mut v) if v.is_object() => { v["actor"] = serde_json::json!(p.name); serde_json::to_vec(&v).unwrap_or(body) }
        _ => body,
    }
}

/// Unix seconds when this head started. Replayed "live" recordings begin at their `start` position at this
/// moment, so every demo run opens on the chosen part of the footage and all viewers agree on the position.
pub fn started_secs() -> f64 {
    static START: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *START.get_or_init(now_secs)
}
