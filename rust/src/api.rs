//! HTTP API + SSE + the embedded console on dash_port() (API-01, D3). Routes and JSON shapes: docs/API.md.
//! Auth and roles: crate::users. One tiny_http server, a pool of threads each looping on `recv()`.

use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tiny_http::{Header, Response, Server};

use crate::jobs::{normal_priority, now_ms, takes_tasks, Ctx, JobRow, JobSpec, NodeRow, NODE_STATES, PRIORITIES};
use crate::proto::{dash_port, KIND_CMD, KIND_LIST, KIND_SHA, KIND_SYNTH};
use crate::users::{valid_name, Auth, LoginErr, Role, Who};
use crate::{dash, head, reduce, sched, select, users};

fn mb(key: &str) -> usize {
    (crate::tune::get(key) * 1048576.0) as usize
}
const POOL: usize = 32; // ponytail: one thread per open SSE stream; a pool this size caps viewers, switch to async if it matters

pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub cookie: Option<String>,
    pub bearer: Option<String>,
    pub body: Vec<u8>,
    pub ip: IpAddr,
    /// the client sent Accept-Encoding: gzip (console files are embedded pre-compressed too)
    pub gz: bool,
}

pub enum Out {
    Plain { status: u16, ctype: &'static str, headers: Vec<(&'static str, String)>, body: Vec<u8> },
    Sse,
}

fn plain(status: u16, ctype: &'static str, body: Vec<u8>) -> Out {
    Out::Plain { status, ctype, headers: vec![], body }
}

fn json_out(status: u16, v: Value) -> Out {
    plain(status, "application/json", v.to_string().into_bytes())
}

fn fail(status: u16, code: &str, msg: &str) -> Out {
    json_out(status, json!({"error": {"code": code, "message": msg}}))
}

fn bad(msg: &str) -> Out {
    fail(400, "invalid_request", msg)
}

fn not_found() -> Out {
    fail(404, "not_found", "no such route or object")
}

fn with_cookie(o: Out, cookie: String) -> Out {
    match o {
        Out::Plain { status, ctype, mut headers, body } => {
            headers.push(("Set-Cookie", cookie));
            Out::Plain { status, ctype, headers, body }
        }
        o => o,
    }
}

fn session_cookie(sid: &str, max_age: u32) -> String {
    format!("ck_session={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}")
}

fn audit(ctx: &Ctx, kind: &str, actor: &str, detail: &str) {
    if let Ok(e) = ctx.store.log_event(now_ms(), kind, actor, detail) {
        ctx.bus.publish("event", &serde_json::to_string(&e).unwrap_or_default());
    }
}

/// The `cluster` snapshot, built at most once per ~second and shared by every SSE stream and GET /cluster, so N
/// console viewers cost the dispatcher one State-lock pass per second instead of N (measured: 25 viewers + 5 pollers
/// slowed dispatch ~20 % before this).
pub fn cluster_cached(ctx: &Ctx) -> std::sync::Arc<String> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<(Instant, Arc<String>)>>> = OnceLock::new();
    let mut g = CACHE.get_or_init(Default::default).lock().unwrap(); // single flight: others wait for this build
    if let Some((t, s)) = g.as_ref() {
        if t.elapsed() < Duration::from_millis(900) {
            return s.clone();
        }
    }
    let s = Arc::new(cluster_json(ctx).to_string());
    *g = Some((Instant::now(), s.clone()));
    s
}

pub fn cluster_json(ctx: &Ctx) -> Value {
    let workers = {
        let st = ctx.sh.0.lock().unwrap();
        let mut ws: Vec<Value> = st.workers.iter().map(|(id, w)| dash::worker_json(*id, w)).collect();
        ws.sort_by_key(|w| w["id"].as_u64());
        ws
    };
    let jobs = running_jobs(ctx);
    // PCs that are enrolled and active but not connected (asleep or off): the SDK planner reads this instead of fetching /nodes
    let on = online_ids(ctx);
    let asleep = ctx.store.nodes().iter().filter(|n| n.state == "active" && !on.contains(&n.node_id)).count();
    json!({"now_ms": now_ms(), "workers": workers, "job": jobs.first(), "jobs": jobs, "asleep": asleep})
}

/// All running jobs, as the head's dispatcher sees them.
fn running_jobs(ctx: &Ctx) -> Vec<JobRow> {
    etas(ctx, crate::head::running_jobs(ctx).into_iter().filter_map(|id| ctx.store.job(id)).collect())
}

fn parse_body(r: &Req) -> Result<Value, Out> {
    match serde_json::from_slice::<Value>(&r.body) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(bad("body must be a JSON object")),
    }
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}

/// A non-negative whole number (JSON 1e6 counts).
fn uint(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0 && *f < 9.0e15).map(|f| f as u64))
}

fn page(r: &Req) -> (usize, Option<u64>) {
    let limit = r.query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50).clamp(1, 500);
    (limit, r.query.get("before").and_then(|s| s.parse().ok()))
}

fn next_of(len: usize, limit: usize, last: Option<u64>) -> Value {
    if len == limit { json!(last) } else { Value::Null }
}

/// Validate a POST /jobs body into a spec (docs/API.md).
pub fn job_spec(v: &Value, who: &Who) -> Result<JobSpec, String> {
    let kind = str_field(v, "kind").ok_or("kind is required")?;
    let from = match v.get("from") {
        None => 1,
        Some(f) => uint(f).ok_or("from must be a non-negative whole number")?,
    };
    let is_list = kind == "list";
    let n = if is_list { 1 } else { v.get("n").and_then(uint).filter(|n| *n > 0).ok_or("n must be a whole number above 0")? };
    let chunk = match v.get("chunk") {
        None => 0,
        Some(c) => uint(c).ok_or("chunk must be a non-negative whole number")?,
    };
    let from = if is_list { 0 } else { from };
    let end = from.checked_add(n).ok_or("from + n is too large")?;
    let mut items: Vec<String> = vec![];
    let (has_sha, has_cmd, has_ns) = (v.get("sha").is_some(), v.get("cmd").is_some(), v.get("synth_ns").is_some());
    let (k, arg) = match kind {
        "sha" if !has_cmd && !has_ns => {
            let s = str_field(v, "sha").ok_or("sha must be a hex string")?;
            if s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("sha must be 1 to 64 hex characters".into());
            }
            (KIND_SHA, s.chars().map(|c| c.to_digit(16).unwrap() as u8).collect())
        }
        "cmd" if !has_sha && !has_ns => {
            let c = str_field(v, "cmd").filter(|c| !c.trim().is_empty()).ok_or("cmd must be a non-empty string")?;
            if who.role < Role::Operator {
                return Err("forbidden: cmd jobs need operator".into());
            }
            (KIND_CMD, c.as_bytes().to_vec())
        }
        "synth" if !has_sha && !has_cmd => {
            let ns = v.get("synth_ns").and_then(uint).ok_or("synth_ns must be a non-negative whole number")?;
            (KIND_SYNTH, ns.to_le_bytes().to_vec())
        }
        "list" if !has_sha && !has_ns => {
            let c = str_field(v, "cmd").filter(|c| !c.trim().is_empty()).ok_or("cmd must be a non-empty string")?;
            if who.role < Role::Operator {
                return Err("forbidden: list jobs need operator".into());
            }
            let a = v.get("items").and_then(Value::as_array).filter(|a| !a.is_empty() && a.len() <= 1_000_000).ok_or("items must be a list of 1 to 1000000 strings")?;
            items = a.iter().map(|x| x.as_str().filter(|s| s.len() <= 4096).map(String::from)).collect::<Option<_>>().ok_or("every item must be a string of at most 4096 bytes")?;
            (KIND_LIST, c.as_bytes().to_vec())
        }
        "sha" | "cmd" | "synth" | "list" => return Err(format!("a {kind} job takes only its own field")),
        _ => return Err("kind must be sha, cmd, synth or list".into()),
    };
    let only = match v.get("only") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<_>>().ok_or("only must be a list of addresses")?,
        _ => return Err("only must be a list of addresses".into()),
    };
    let name = str_field(v, "name").filter(|s| !s.is_empty()).unwrap_or(kind).chars().take(200).collect();
    let selector = match v.get("selector") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            select::parse(s)?;
            s.trim().to_string()
        }
        _ => return Err("selector must be a string".into()),
    };
    let priority = match v.get("priority") {
        None | Some(Value::Null) => normal_priority(),
        Some(Value::String(p)) => PRIORITIES.iter().position(|x| x == p).ok_or_else(|| format!("priority must be one of {}", PRIORITIES.join(", ")))? as u8,
        _ => return Err(format!("priority must be one of {}", PRIORITIES.join(", "))),
    };
    let policy = match str_field(v, "policy") {
        None | Some("") => String::new(),
        Some(p) if sched::parse_policy(p).is_some() => p.to_string(),
        _ => return Err("policy must be adaptive, guided or fixed".into()),
    };
    let reducer = match str_field(v, "reducer") {
        None => String::new(),
        Some(r) if reduce::valid(r) => r.to_string(),
        Some(_) => return Err(format!("reducer must be one of {}", reduce::REDUCERS.join(", "))),
    };
    let end = if is_list { items.len() as u64 } else { end };
    let max_pcs = match v.get("max_pcs") {
        None | Some(Value::Null) => 0,
        Some(x) => uint(x).filter(|n| *n <= 1_000_000).ok_or("max_pcs must be a whole number, 0 = no limit")? as u32,
    };
    let placement = match str_field(v, "placement") {
        None | Some("") | Some("auto") => String::new(),
        Some(p) if crate::jobs::PLACEMENTS.contains(&p) => p.to_string(),
        Some(_) => return Err(format!("placement must be one of {}", crate::jobs::PLACEMENTS.join(", "))),
    };
    let mem_mb = match v.get("mem_mb") {
        None | Some(Value::Null) => 0,
        Some(x) => uint(x).filter(|n| *n <= 16 << 20).ok_or("mem_mb must be a whole number of MB")?,
    };
    let blobs: Vec<String> = match v.get("blobs") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().filter(|s| crate::blobs::valid_sha(s)).map(String::from)).collect::<Option<_>>().ok_or("blobs must be a list of sha256 hex strings")?,
        _ => return Err("blobs must be a list of sha256 hex strings".into()),
    };
    let blob_mode = str_field(v, "blob_mode").unwrap_or("").to_string();
    let preempt = match v.get("preempt") {
        None | Some(Value::Null) => false,
        Some(x) => x.as_bool().ok_or("preempt must be true or false")?,
    };
    Ok(JobSpec { preempt, blobs, blob_mode, max_pcs, placement, mem_mb, name, kind: k, from, end, chunk, arg, only, submitter: who.name.clone(), selector, priority, policy, reducer, items, item_base: 0 })
}

/// Fill in the estimated finish of a running job (SCH-03).
fn with_eta(mut j: JobRow, rates: &HashMap<u64, f64>) -> JobRow {
    j.eta_ms = rates.get(&j.id).filter(|_| j.state == crate::jobs::JobState::Running).and_then(|r| sched::eta_secs(j.total.saturating_sub(j.done), *r)).map(|s| now_ms() + (s * 1000.0) as i64);
    j
}

/// Job JSON: the JobRow plus max_pcs / placement / mem_mb read from the stored spec.
fn job_json(ctx: &Ctx, j: &JobRow) -> Value {
    let (max_pcs, placement, mem_mb) = ctx.store.job_limits(j.id);
    let mut v = json!(j);
    v["max_pcs"] = json!(max_pcs);
    v["placement"] = json!(placement);
    v["mem_mb"] = json!(mem_mb);
    v["files"] = json!(ctx.store.job_file_count(j.id));
    let wait = if j.state.finished() { None } else { head::wait_reason(ctx, j.id) };
    v["wait_reason"] = json!(wait);
    if wait == Some("quota") {
        v["state"] = json!("queued"); // the row says running; to the owner it waits for its quota
    }
    if let Some(sp) = ctx.store.spec(j.id).filter(|sp| !sp.blobs.is_empty()) {
        v["blobs"] = json!(sp.blobs);
        v["blob_mode"] = json!(sp.blob_mode);
    }
    v
}

fn etas(ctx: &Ctx, jobs: Vec<JobRow>) -> Vec<JobRow> {
    let rates = head::job_rates(ctx);
    jobs.into_iter().map(|j| with_eta(j, &rates)).collect()
}

fn tz_offset() -> i32 {
    std::env::var("CK_TZ_OFFSET_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(330)
}

thread_local!(static USER: std::cell::RefCell<String> = Default::default());

/// Wraps the router: one `api.request` log line per call (no bodies, no query string). Reads are debug, writes info.
pub fn route(ctx: &Arc<Ctx>, auth: &Auth, r: &Req) -> Out {
    let t = std::time::Instant::now();
    USER.with(|u| u.borrow_mut().clear());
    let out = route_in(ctx, auth, r);
    if r.path.starts_with("/api/") {
        let status = match &out { Out::Plain { status, .. } => *status, Out::Sse => 200 };
        if r.method != "GET" && status < 300 {
            for what in changed_what(&r.path) {
                ctx.bus.publish("changed", &json!({"what": what}).to_string());
            }
        }
        let lvl = if r.method != "GET" { crate::log::Level::Info } else if status >= 400 { crate::log::Level::Warn } else { crate::log::Level::Debug };
        if crate::log::enabled(lvl) {
            let user = USER.with(|u| u.borrow().clone());
            crate::log::emit(lvl, "api.request", json!({"method": r.method, "path": r.path, "status": status, "ms": t.elapsed().as_secs_f64() * 1000.0, "user": user, "ip": r.ip.to_string()}));
        }
    }
    out
}

/// SSE `changed` (wave 14): which lists a successful write touched, so the console refetches them.
fn changed_what(path: &str) -> &'static [&'static str] {
    let p: Vec<&str> = path.trim_matches('/').split('/').skip(2).collect();
    match p.as_slice() {
        ["reserves", ..] => &["reserves"],
        ["reservations", ..] => &["bookings", "timetable"],
        ["windows", ..] => &["windows", "timetable"],
        ["pools", ..] => &["pools"],
        ["templates", ..] => &["templates"],
        ["caps", ..] => &["caps"],
        ["nodes", _, "labels"] => &["labels"],
        ["ha", "settings"] => &["candidates"],
        _ => &[],
    }
}

fn route_in(ctx: &Arc<Ctx>, auth: &Auth, r: &Req) -> Out {
    if r.path == "/stats" {
        return plain(200, "application/json", dash::stats(&ctx.sh, r.query.get("detail").is_some_and(|d| d == "1")).into_bytes());
    }
    if !r.path.starts_with("/api/") {
        if r.path == "/legacy" {
            return Out::Plain { status: 200, ctype: "text/html; charset=utf-8", headers: vec![("Cache-Control", "no-store".into())], body: dash::LEGACY.as_bytes().to_vec() };
        }
        return match dash::ui_file(&r.path) {
            Some(f) => {
                let cache = if f.0.starts_with("/_app/immutable/") { "public, max-age=31536000, immutable" } else { "no-cache" };
                let mut headers = vec![("Cache-Control", cache.into()), ("Vary", "Accept-Encoding".into())];
                // the same file pre-compressed at console build time (console/scripts/gzip.ts): ~4x fewer bytes on the lab LAN
                let body = match dash::ui_file_exact(&format!("{}.gz", f.0)).filter(|_| r.gz) {
                    Some(z) => (headers.push(("Content-Encoding", "gzip".into())), z.2.to_vec()).1,
                    None => f.2.to_vec(),
                };
                Out::Plain { status: 200, ctype: f.1, headers, body }
            }
            None => Out::Plain { status: 200, ctype: "text/html; charset=utf-8", headers: vec![("Cache-Control", "no-store".into())], body: dash::LEGACY.as_bytes().to_vec() },
        };
    }
    let st = ctx.repl.status.lock().unwrap().clone();
    if st.role == "standby" && !matches!(r.path.trim_end_matches('/'), "/api/v1/ha" | "/api/v1/me" | "/api/v1/login" | "/api/v1/logout") {
        let primary = if st.primary.is_empty() { "unknown" } else { st.primary.as_str() };
        return fail(503, "unavailable", &format!("this head is a standby; the primary is {primary}"));
    }
    let who = auth.who(&ctx.store, r.cookie.as_deref(), r.bearer.as_deref());
    let who = match (users::min_role(&r.method, &r.path), who) {
        (None, w) => w,
        (Some(_), None) => return fail(401, "unauthorized", "sign in first"),
        (Some(min), Some(w)) if w.role < min => return fail(403, "forbidden", &format!("needs the {} role", min.name())),
        (Some(_), w) => w,
    };
    // segments are percent-decoded after splitting, so ids with '/' (virtual nodes: "<id>/v3") arrive as %2F
    let decoded: Vec<String> = r.path.trim_matches('/').split('/').skip(2).map(pct_decode).collect();
    let segs: Vec<&str> = decoded.iter().map(String::as_str).collect();
    let me = who.as_ref().map(|w| w.name.as_str()).unwrap_or("");
    USER.with(|u| *u.borrow_mut() = me.to_string());
    match (r.method.as_str(), segs.as_slice()) {
        ("GET", ["me"]) => json_out(
            200,
            json!({"user": who.as_ref().map(|w| json!({"name": w.name, "role": w.role.name()})), "mode": if auth.open { "open" } else { "required" },
                   "needs_bootstrap": !auth.open && ctx.store.users().is_empty()}),
        ),
        ("GET", ["ha"]) => json_out(200, ha_json(ctx)),
        ("GET", ["wire"]) => json_out(200, crate::proto::wire::json()),
        ("POST", ["wire", "reset"]) => {
            crate::proto::wire::reset();
            json_out(200, json!({"ok": true}))
        }
        ("POST", ["sim"]) => post_sim(who.as_ref(), r),
        ("POST", ["ha", "switchover"]) => {
            let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
            let Some(to) = v["to"].as_str() else { return bad("give {\"to\": STANDBY_NODE_ID}") };
            match crate::head::switch_to(ctx.clone(), to, me) {
                Ok(peer) => json_out(202, json!({"to": peer})),
                Err(e) => fail(409, "conflict", &e),
            }
        }
        ("POST", ["nodes", "power"]) => node_power(ctx, r, me),
        ("GET", ["power"]) => json_out(200, power_json(ctx)),
        ("PUT", ["power"]) => put_power(ctx, r, me),
        ("GET", ["logs"]) => get_logs(r),
        ("GET", ["tunables"]) => json_out(200, json!({"tunables": crate::tune::listing()})),
        ("PUT", ["tunables"]) => put_tunables(ctx, r, me),
        ("GET", ["hierarchy"]) => json_out(200, crate::hier::json(ctx)),
        ("PUT", ["hierarchy", "settings"]) => put_hier_settings(ctx, r, me),
        ("POST", ["hierarchy", "labs", lab, "head"]) => {
            let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
            let Some(node) = v["node"].as_str() else { return bad("give {\"node\": NODE_ID}") };
            match crate::hier::pin(ctx, lab, node, me) { Ok(v) => json_out(200, v), Err(e) => bad(&e) }
        }
        ("DELETE", ["hierarchy", "labs", lab, "head"]) => match crate::hier::unpin(ctx, lab, me) { Ok(v) => json_out(200, v), Err(e) => fail(404, "not_found", &e) },
        ("POST", ["hierarchy", "labs", lab, "drain"]) => {
            let v = parse_body(r).unwrap_or_else(|_| json!({}));
            match crate::hier::drain(ctx, lab, v["on"].as_bool().unwrap_or(true), me) { Ok(v) => json_out(200, v), Err(e) => bad(&e) }
        }
        ("GET", ["ha", "candidates"]) => json_out(200, json!({"candidates": crate::head::candidates(ctx)})),
        ("GET", ["ha", "settings"]) => json_out(200, json!({"settings": ha_settings(ctx)})),
        ("PUT", ["ha", "settings"]) => put_ha_settings(ctx, r, me),
        ("POST", ["bootstrap"]) => bootstrap(ctx, auth, r),
        ("POST", ["login"]) => login(ctx, auth, r),
        ("POST", ["logout"]) => {
            if let Some(sid) = &r.cookie {
                auth.end_session(sid);
            }
            with_cookie(json_out(200, json!({})), session_cookie("", 0))
        }
        ("POST", ["tokens"]) => {
            if auth.open {
                return fail(409, "conflict", "auth is off; tokens are not needed");
            }
            match auth.new_token(&ctx.store, me) {
                Ok(t) => json_out(200, json!({"token": t})),
                Err(e) => fail(503, "unavailable", &e),
            }
        }
        ("GET", ["users"]) => json_out(200, json!({"users": ctx.store.users().iter().map(|u| user_json(ctx, u)).collect::<Vec<_>>()})),
        ("POST", ["users"]) => create_user(ctx, r, me),
        ("PUT", ["users", name, "quota"]) => put_quota(ctx, r, name, me),
        ("GET", ["templates"]) => json_out(200, json!({"templates": ctx.store.templates().into_iter().map(template_json).collect::<Vec<_>>()})),
        ("POST", ["templates"]) => post_template(ctx, r, who.as_ref().unwrap()),
        ("DELETE", ["templates", name]) => delete_template(ctx, name, who.as_ref().unwrap()),
        ("GET", ["jobs", id, "files"]) => match id.parse::<u64>().ok().filter(|i| ctx.store.job(*i).is_some()) {
            Some(id) => json_out(200, json!({"files": ctx.store.job_files(id).into_iter().map(|(a, b, name, sha, size)| json!({"chunk": [a, b], "name": name, "sha": sha, "size": size})).collect::<Vec<_>>()})),
            None => fail(404, "not_found", "no such job"),
        },
        ("POST", ["nodes", "identify"]) => post_identify(ctx, r, me),
        ("GET", ["cluster"]) => Out::Plain { status: 200, ctype: "application/json", headers: vec![], body: cluster_cached(ctx).as_bytes().to_vec() },
        ("GET", ["nodes"]) => {
            let (on, org) = (online_ids(ctx), Org::load(ctx));
            let nodes: Vec<Value> = ctx.store.nodes().iter().map(|n| node_json(ctx, n, on.contains(&n.node_id), &org)).collect();
            json_out(200, json!({"nodes": nodes}))
        }
        ("GET", ["nodes", id]) => match ctx.store.node(id) {
            Some(n) => json_out(200, json!({"node": node_json(ctx, &n, online_ids(ctx).contains(*id), &Org::load(ctx))})),
            None => fail(404, "not_found", "no such node"),
        },
        ("GET", ["select"]) => select_preview(ctx, r),
        ("PUT", ["nodes", id, "labels"]) => set_labels(ctx, r, id, me),
        ("GET", ["pools"]) => json_out(200, json!({"pools": ctx.store.pools()})),
        ("PUT", ["pools", name]) => put_pool(ctx, r, name, me),
        ("DELETE", ["pools", name]) => delete_pool(ctx, name, me),
        ("GET", ["caps"]) => json_out(200, json!({"caps": ctx.store.caps()})),
        ("POST", ["caps"]) => add_cap(ctx, r, me),
        ("DELETE", ["caps", id]) => match id.parse::<u64>().ok().map(|i| ctx.store.delete_cap(i)) {
            Some(Ok(())) => {
                audit(ctx, "cap.deleted", me, id);
                wake_all(ctx);
                json_out(200, json!({}))
            }
            _ => fail(404, "not_found", "no such cap"),
        },
        ("GET", ["reserves"]) => {
            let views = node_views(ctx);
            let rows: Vec<Value> = ctx
                .store
                .reserves()
                .iter()
                .map(|rs| {
                    let held = reserve_view(rs).map(|rv| sched::reserved(&views, &[rv])).unwrap_or_default();
                    let mut v = serde_json::to_value(rs).unwrap_or_default();
                    v["held"] = json!(held);
                    v
                })
                .collect();
            json_out(200, json!({"reserves": rows}))
        }
        ("POST", ["reserves"]) => add_reserve(ctx, r, me),
        ("DELETE", ["reserves", id]) => match id.parse::<u64>().ok().map(|i| ctx.store.delete_reserve(i)) {
            Some(Ok(())) => {
                audit(ctx, "reserve.deleted", me, id);
                wake_all(ctx);
                json_out(200, json!({}))
            }
            _ => fail(404, "not_found", "no such reserve"),
        },
        ("GET", ["windows"]) => {
            let rows: Vec<Value> = ctx.store.windows().iter().map(|w| window_json(w)).collect();
            json_out(200, json!({"windows": rows}))
        }
        ("POST", ["windows"]) => add_window(ctx, r, me),
        ("DELETE", ["windows", id]) => match id.parse::<u64>().ok().map(|i| ctx.store.delete_window(i)) {
            Some(Ok(())) => {
                audit(ctx, "window.deleted", me, id);
                wake_all(ctx);
                json_out(200, json!({}))
            }
            _ => fail(404, "not_found", "no such window"),
        },
        ("GET", ["reservations"]) => {
            let now = now_ms();
            let rows: Vec<Value> = ctx.store.reservations(Some(now)).iter().map(|x| reservation_json(x, now)).collect();
            json_out(200, json!({"reservations": rows}))
        }
        ("POST", ["reservations"]) => add_reservation(ctx, r, me),
        ("DELETE", ["reservations", id]) => match id.parse::<u64>().ok().map(|i| ctx.store.delete_reservation(i)) {
            Some(Ok(())) => {
                audit(ctx, "reservation.deleted", me, id);
                wake_all(ctx);
                json_out(200, json!({}))
            }
            _ => fail(404, "not_found", "no such reservation"),
        },
        ("GET", ["estimate"]) => estimate(ctx, r),
        ("GET", ["blobs"]) => blobs_list(ctx),
        ("GET", ["net"]) => net_json(ctx),
        ("POST", ["net", "probe"]) => net_probe(ctx, r),
        ("GET", ["config"]) => json_out(200, export_config(ctx)),
        ("POST", ["config"]) => config_import(ctx, r, me),
        ("POST", ["nodes", "state"]) => set_states(ctx, r, me),
        ("PUT", ["nodes", id, "place"]) => place(ctx, r, id, me),
        ("GET", ["labs"]) => json_out(200, json!({"labs": ctx.store.labs()})),
        ("POST", ["labs"]) => lab_create(ctx, r, me),
        ("PATCH", ["labs", id]) => lab_update(ctx, r, id, me),
        ("DELETE", ["labs", id]) => match id.parse::<u64>().ok().map(|i| ctx.store.delete_lab(i)) {
            Some(Ok(())) => {
                audit(ctx, "lab.deleted", me, id);
                // its nodes became unplaced: tell watchers
                let (on, org) = (online_ids(ctx), Org::load(ctx));
                ctx.store.nodes().iter().filter(|n| n.lab_id.is_none()).for_each(|n| publish_node(ctx, n, on.contains(&n.node_id), &org));
                json_out(200, json!({}))
            }
            _ => fail(404, "not_found", "no such lab"),
        },
        ("POST", ["join-codes"]) => join_code(ctx, r, me),
        ("GET", ["jobs"]) => {
            let (limit, before) = page(r);
            let jobs = etas(ctx, ctx.store.jobs(limit, before));
            let next = next_of(jobs.len(), limit, jobs.last().map(|j| j.id));
            json_out(200, json!({"jobs": jobs.iter().map(|j| job_json(ctx, j)).collect::<Vec<_>>(), "next": next}))
        }
        ("POST", ["jobs"]) => {
            let who = who.unwrap();
            let mut v = match parse_body(r) {
                Ok(v) => v,
                Err(o) => return o,
            };
            if let Some(t) = v.get("template") {
                // JOB-12: the stored spec, with the body's other keys laid over it
                let Some(t) = t.as_str().and_then(|n| ctx.store.template(n)) else { return fail(404, "not_found", "no such template") };
                let mut spec: Value = serde_json::from_str(&t.1).unwrap_or(json!({}));
                for (k, x) in v.as_object().into_iter().flatten().filter(|(k, _)| *k != "template") {
                    spec[k] = x.clone();
                }
                v = spec;
            }
            match job_spec(&v, &who) {
                Err(m) if m.starts_with("forbidden: ") => fail(403, "forbidden", &m[11..]),
                Err(m) => bad(&m),
                Ok(spec) => match head::submit(ctx, spec) {
                    Ok(job) => json_out(200, json!({"job": job_json(ctx, &etas(ctx, vec![job]).remove(0))})),
                    Err(e) => fail(409, "conflict", &e),
                },
            }
        }
        ("GET", ["jobs", id]) | ("GET", ["jobs", id, "result"]) | ("POST", ["jobs", id, "cancel"]) => {
            let Some(job) = id.parse().ok().and_then(|id| ctx.store.job(id)) else { return fail(404, "not_found", "no such job") };
            match (r.method.as_str(), segs.get(2)) {
                ("GET", None) => {
                    // Round 4: ?wait=S holds the request until the job is finished (at most S seconds, 10 max); ?result=1 then carries the
                    // reduced result inline (one trip instead of poll + poll + fetch). Old heads ignore both and answer at once, so the
                    // reply's "long" field tells a client which kind of head it is talking to.
                    let wait = r.query.get("wait").and_then(|s| s.parse::<f64>().ok()).filter(|w| w.is_finite()).map(|w| w.clamp(0.0, 10.0));
                    let mut job = job;
                    if let Some(w) = wait.filter(|w| *w > 0.0 && !job.state.finished()) {
                        if let Some(_slot) = LongPoll::enter() {
                            let end = Instant::now() + Duration::from_secs_f64(w);
                            while Instant::now() < end && !job.state.finished() {
                                thread::sleep(Duration::from_millis(10));
                                if let Some(j) = ctx.store.job(job.id) {
                                    job = j;
                                }
                            }
                        }
                    }
                    let mut v = json!({"job": job_json(ctx, &etas(ctx, vec![job.clone()]).remove(0)), "parts": ctx.store.part_count(job.id)});
                    if wait.is_some() {
                        v["long"] = json!(true);
                        if job.state.as_str() == "done" && r.query.get("result").is_some_and(|x| x == "1") {
                            if let Ok(b) = reduce::reduce(&job.kind, &job.reducer, &ctx.store.parts(job.id)) {
                                if b.len() <= LONG_RESULT_MAX {
                                    v["result"] = json!(String::from_utf8_lossy(&b));
                                }
                            }
                        }
                    }
                    json_out(200, v)
                }
                ("GET", _) => {
                    let red = r.query.get("reducer").map(|s| pct_decode(s)).unwrap_or_else(|| job.reducer.clone());
                    match reduce::reduce(&job.kind, &red, &ctx.store.parts(job.id)) {
                        Ok(b) => plain(200, "text/plain; charset=utf-8", b),
                        Err(e) => bad(&e),
                    }
                }
                _ => {
                    let who = who.unwrap();
                    if job.submitter != who.name && who.role < Role::Operator {
                        return fail(403, "forbidden", "cancelling someone else's job needs the operator role");
                    }
                    if job.state.finished() {
                        return fail(409, "conflict", &format!("job {} already {}", job.id, job.state.as_str()));
                    }
                    match head::cancel_job(ctx, job.id, &who.name) {
                        Ok(j) => json_out(200, json!({"job": j})),
                        Err(e) => fail(409, "conflict", &e),
                    }
                }
            }
        }
        ("GET", ["events"]) => {
            let (limit, before) = page(r);
            let events = ctx.store.events(limit, before);
            let next = next_of(events.len(), limit, events.last().map(|e| e.id));
            json_out(200, json!({"events": events, "next": next}))
        }
        ("GET", ["stream"]) => Out::Sse,
        ("GET", ["advice"]) => advice_get(ctx),
        ("GET", ["advice", "history"]) => advice_history(ctx),
        ("PUT", ["advice", "settings"]) => advice_settings(ctx, r, who.as_ref(), me),
        ("POST", ["advice", id, op]) => advice_post(ctx, r, who.as_ref(), id, op, me),
        ("GET", ["jobs", id, "breakdown"]) => match id.parse().ok().and_then(|id| head::breakdown(ctx, id)) {
            Some(b) => json_out(200, json!({"breakdown": b})),
            None => fail(404, "not_found", "no such job, or it ran before time accounting existed"),
        },
        ("GET", ["alerts"]) => {
            // T55: 1,000 open alerts used to show as 500 with no hint. ?limit= (default 500, max 5000); "truncated" says there is more.
            let limit = r.query.get("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(500).clamp(1, 5000);
            let mut a = ctx.store.alerts(r.query.get("active").is_some_and(|v| v == "1"), limit + 1);
            let truncated = a.len() > limit;
            a.truncate(limit);
            if let Some(m) = head::MEM_ALERT.lock().unwrap().clone() { a.insert(0, m); } // disk full: the alert could not be stored
            json_out(200, json!({"alerts": a, "truncated": truncated}))
        }
        ("POST", ["alerts", id, "ack"]) => match id.parse().ok().and_then(|id| ctx.store.alert_ack(id, me)) {
            Some(a) => {
                ctx.bus.publish("alert", &serde_json::to_string(&a).unwrap_or_default());
                json_out(200, json!({"alert": a}))
            }
            None => fail(404, "not_found", "no such alert"),
        },
        ("POST", ["experiments"]) => {
            let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
            let (Some(name), Some(csv), Some(jl)) = (str_field(&v, "name"), str_field(&v, "raw_csv"), str_field(&v, "runs_jsonl")) else { return bad("give name, raw_csv and runs_jsonl (the files scripts/experiment.py writes)") };
            let row = match crate::research::parse_experiment(name, me, csv, jl, now_ms()) { Ok(r) => r, Err(e) => return bad(&e) };
            let sim = v.get("sim").filter(|s| !s.is_null()).map(|s| s.to_string());
            match ctx.store.add_experiment(&row, csv, jl, sim.as_deref()) {
                Ok(row) => {
                    head::event(ctx, "experiment.uploaded", me, &format!("experiment {} \"{}\": {} configs x {} repeats", row.id, row.name, row.configs, row.repeats));
                    json_out(200, json!({"experiment": row}))
                }
                Err(e) => fail(500, "internal", &e),
            }
        }
        ("GET", ["experiments"]) => json_out(200, json!({"experiments": ctx.store.experiments()})),
        ("GET", ["experiments", id]) => match id.parse().ok().and_then(|id| ctx.store.experiment(id)) {
            Some((row, csv, jl, sim)) => json_out(200, json!({"experiment": row, "raw_csv": csv, "runs_jsonl": jl, "sim": sim.and_then(|s| serde_json::from_str::<Value>(&s).ok())})),
            None => fail(404, "not_found", "no such experiment"),
        },
        ("GET", ["report"]) => {
            let tz = crate::research::tz_min();
            let day = r.query.get("day").filter(|d| !d.is_empty()).cloned().unwrap_or_else(|| crate::research::day_of(now_ms(), tz));
            match crate::research::day_report(&ctx.store, &day, tz) {
                Ok(v) => json_out(200, json!({"report": v})),
                Err(e) => bad(&e),
            }
        }
        _ => not_found(),
    }
}

// ---- fleet (API.md "Fleet")

fn online_ids(ctx: &Ctx) -> std::collections::HashSet<String> {
    ctx.sh.0.lock().unwrap().workers.values().flat_map(|w| w.hello.as_ref().map(|h| h.node_id.clone()).into_iter().chain(w.lab_pcs.iter().cloned())).collect() // PCs under a lab head are online too
}

/// Labels, pools and lab names of every node, loaded once per request (ORG-02..04).
pub struct Org {
    labels: HashMap<String, Vec<(String, String)>>,
    pools: HashMap<String, Vec<String>>,
    labs: HashMap<u64, String>,
    presence: HashMap<String, String>,
    via: HashMap<String, (String, Value)>, // PC behind a lab head -> (lab, what the lab head reports about it)
}

impl Org {
    pub fn load(ctx: &Ctx) -> Org {
        let mut labels: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (n, k, v) in ctx.store.labels() {
            labels.entry(n).or_default().push((k, v));
        }
        let mut pools: HashMap<String, Vec<String>> = HashMap::new();
        for p in ctx.store.pools() {
            for m in &p.members {
                pools.entry(m.clone()).or_default().push(p.name.clone());
            }
        }
        let presence = ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| Some((w.hello.as_ref()?.node_id.clone(), if w.presence.is_empty() { "idle".into() } else { w.presence.clone() }))).collect();
        let mut via = HashMap::new();
        for w in ctx.sh.0.lock().unwrap().workers.values() {
            let Some(l) = crate::head::lab_of(w) else { continue };
            for p in w.lab_stat["pcs"].as_array().into_iter().flatten().take(5000) {
                if let Some(id) = p["id"].as_str() {
                    via.insert(id.to_string(), (l.lab.clone(), p.clone()));
                }
            }
            for id in &w.lab_pcs {
                via.entry(id.clone()).or_insert_with(|| (l.lab.clone(), Value::Null));
            }
        }
        Org { labels, pools, labs: ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect(), presence, via }
    }
    fn attrs(&self, n: &NodeRow, live: Option<(u32, f64)>) -> select::Attrs {
        let (l, p) = (self.labels.get(&n.node_id).map_or(&[][..], |v| v), self.pools.get(&n.node_id).map_or(&[][..], |v| v));
        select::attrs(n, l, p, n.lab_id.and_then(|i| self.labs.get(&i)).map(String::as_str), live)
    }
}

/// Views of the connected nodes for the scheduler (one place builds them: the API for `held`, the head for dispatch).
pub fn node_views(ctx: &Ctx) -> Vec<sched::NodeView> {
    let org = Org::load(ctx);
    let st = ctx.sh.0.lock().unwrap();
    let mut v: Vec<sched::NodeView> = st
        .workers
        .iter()
        .filter_map(|(wid, w)| {
            let id = &w.hello.as_ref()?.node_id;
            let n = ctx.store.node(id)?;
            Some(sched::NodeView { wid: *wid, node_id: id.clone(), attrs: org.attrs(&n, Some((w.threads, w.cpu))), takes_tasks: takes_tasks(&w.state), job: w.inflight.first().map(|f| f.0) })
        })
        .collect();
    v.sort_by_key(|n| n.wid);
    v
}

fn node_json(ctx: &Ctx, n: &NodeRow, online: bool, org: &Org) -> Value {
    let mut v = serde_json::to_value(n).unwrap_or_default();
    v["inventory"] = serde_json::from_str(&n.inventory).unwrap_or(json!({}));
    v["online"] = json!(online);
    // the lab head this PC works under, if any (it never connected to this head directly)
    // `live` is what the lab head reports about it (threads, busy, virtual, cpu %, memory); null for a PC the root serves itself
    let via = org.via.get(&n.node_id);
    v["via_lab"] = json!(via.map(|x| &x.0));
    v["live"] = via.map_or(Value::Null, |(_, p)| json!({
        "threads": p["threads"], "busy": p["busy"], "virtual": p["virtual"], "host": p["host"], "cpu": p["cpu"],
        "mem_pct": p["mem_total"].as_u64().filter(|t| *t > 0).map(|t| (p["mem_used"].as_u64().unwrap_or(0) as f64 / t as f64 * 1000.0).round() / 10.0),
    }));
    v["presence"] = json!(org.presence.get(&n.node_id).map_or("idle", String::as_str));
    v["labels"] = json!(org.labels.get(&n.node_id).map(|l| l.iter().cloned().collect::<std::collections::BTreeMap<_, _>>()).unwrap_or_default());
    v["pools"] = json!(org.pools.get(&n.node_id).cloned().unwrap_or_default());
    v["class"] = json!(select::class_of(&n.inventory));
    v["power"] = json!(crate::head::power_state(ctx, &n.node_id));
    v["power_since_ms"] = json!(crate::head::power_since(ctx, &n.node_id));
    v["identify_until_ms"] = json!(crate::head::identify_until(&n.node_id));
    v
}

fn publish_node(ctx: &Ctx, n: &NodeRow, online: bool, org: &Org) {
    ctx.bus.publish("node", &node_json(ctx, n, online, org).to_string());
}

/// Tell the scheduler a node's state or seat changed.
/// Also rebuilds selector attributes, caps and reserves (labels, pools, caps and reserves feed the scheduler).
/// ponytail: one full refresh per changed node; batch it if bulk edits on 1,000+ PCs feel slow.
fn tell_head(ctx: &Ctx, node_id: &str) {
    crate::head::node_state_changed(ctx, node_id);
    crate::head::refresh(ctx);
}

fn changed(ctx: &Ctx, n: &NodeRow) {
    tell_head(ctx, &n.node_id);
    publish_node(ctx, n, online_ids(ctx).contains(&n.node_id), &Org::load(ctx));
}

fn power_settings(ctx: &Ctx) -> crate::jobs::PowerSettings {
    ctx.store.meta("power").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// POST /api/v1/sim (researcher): run a `ck sim` scenario on its own thread; one run at a time.
fn post_sim(who: Option<&Who>, r: &Req) -> Out {
    static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if who.is_some_and(|w| w.role < Role::Researcher) {
        return fail(403, "forbidden", "needs the researcher role");
    }
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    if BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return fail(409, "busy", "a simulation is already running; try again when it ends");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = thread::Builder::new().name("sim".into()).spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(|| crate::sim::run_api(&v)));
        BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    });
    if spawned.is_err() {
        BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        return fail(503, "unavailable", "cannot start the simulation thread");
    }
    match rx.recv() {
        Ok(Ok(Ok(v))) => json_out(200, v),
        Ok(Ok(Err(e))) => bad(&e),
        _ => fail(500, "internal", "the simulation crashed"),
    }
}

fn ha_settings(ctx: &Ctx) -> crate::jobs::HaSettings {
    ctx.store.meta("ha_settings").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn power_json(ctx: &Ctx) -> Value {
    let asleep: Vec<String> = ctx.store.meta("asleep").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    json!({"power": power_settings(ctx), "asleep": asleep, "labs": lab_power(ctx)})
}

/// Per lab: estimated watts from the same model day-mode sim uses (sim.rs W_*), and PC counts by power state.
/// A PC that is awake but not connected is not counted (its draw is unknown).
fn lab_power(ctx: &Ctx) -> Vec<Value> {
    use crate::sim::{W_BOOT, W_BUSY, W_IDLE, W_INTERACTIVE, W_SLEEP};
    let (load, nodes) = (crate::head::load_by_node(ctx), ctx.store.nodes());
    ctx.store.labs().iter().map(|l| {
        let (mut w, mut c) = (0.0, [0u32; 3]);
        for n in nodes.iter().filter(|n| n.lab_id == Some(l.id)) {
            match (crate::head::power_state(ctx, &n.node_id), load.get(&n.node_id)) {
                ("asleep", _) => { c[1] += 1; w += W_SLEEP }
                ("waking", _) => { c[2] += 1; w += W_BOOT }
                (_, Some(&(busy, user))) => { c[0] += 1; w += if user { W_INTERACTIVE } else if busy { W_BUSY } else { W_IDLE } }
                _ => {}
            }
        }
        json!({"lab": l.name, "watts": w, "awake": c[0], "asleep": c[1], "waking": c[2]})
    }).collect()
}

fn node_power(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let Some(action) = str_field(&v, "action").filter(|a| ["sleep", "wake"].contains(a)) else { return bad("action must be sleep or wake") };
    let ids: Option<Vec<String>> = v.get("node_ids").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let Some(ids) = ids.filter(|a| !a.is_empty() && a.len() <= 1000) else { return bad("node_ids must be a non-empty list of node ids") };
    if let Some(missing) = ids.iter().find(|i| ctx.store.node(i).is_none()) {
        return fail(404, "not_found", &format!("no such node {missing}"));
    }
    match crate::head::power(ctx, &ids, action, actor) {
        Ok(done) => {
            let (on, org) = (online_ids(ctx), Org::load(ctx));
            let nodes: Vec<Value> = done.iter().filter_map(|i| ctx.store.node(i)).map(|n| node_json(ctx, &n, on.contains(&n.node_id), &org)).collect();
            json_out(200, json!({"nodes": nodes}))
        }
        Err(e) => fail(409, "conflict", &e),
    }
}

fn put_power(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let mut p = power_settings(ctx);
    if let Some(m) = v.get("mode") {
        match m.as_str() { Some(m @ ("off" | "eco")) => p.mode = m.into(), _ => return bad("mode must be off or eco") }
    }
    if let Some(x) = v.get("idle_min") {
        match uint(x).filter(|n| (1..=1440).contains(n)) { Some(n) => p.idle_min = n as u32, None => return bad("idle_min must be 1-1440") }
    }
    if let Some(x) = v.get("keep_awake") {
        match uint(x).filter(|n| *n <= 10000) { Some(n) => p.keep_awake = n as u32, None => return bad("keep_awake must be 0-10000") }
    }
    if let Some(x) = v.get("wake_on_demand") {
        match x.as_bool() { Some(b) => p.wake_on_demand = b, None => return bad("wake_on_demand must be true or false") }
    }
    if let Err(e) = ctx.store.set_meta("power", &serde_json::to_string(&p).unwrap_or_default()) {
        return fail(503, "unavailable", &e);
    }
    audit(ctx, "power.settings", actor, &format!("{} idle {} min, keep {}, on-demand {}", p.mode, p.idle_min, p.keep_awake, p.wake_on_demand));
    crate::head::power_settings_changed(ctx);
    json_out(200, power_json(ctx))
}

/// GET /api/v1/logs?since_ms=&kind=&level=&role=&limit= : newest matching lines from this head's log files.
fn get_logs(r: &Req) -> Out {
    let q = |k: &str| r.query.get(k).filter(|v| !v.is_empty());
    let level = match q("level") { Some(l) => match crate::log::Level::parse(l) { Some(l) => l, None => return bad("level is error|warn|info|debug") }, None => crate::log::Level::Debug };
    let query = crate::log::Query {
        role: q("role").cloned(),
        since_ms: q("since_ms").and_then(|v| v.parse().ok()).unwrap_or(0),
        kind: q("kind").cloned(),
        level,
        limit: q("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(200).clamp(1, 5000),
    };
    let lines: Vec<Value> = crate::log::query(&crate::log::logs_dir(), &query).iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
    json_out(200, json!({"logs": lines}))
}

fn put_tunables(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let mut changes = vec![];
    for (k, x) in v.as_object().into_iter().flatten() {
        match x {
            Value::Null => changes.push((k.clone(), None)),
            _ => match x.as_f64() { Some(n) => changes.push((k.clone(), Some(n))), None => return bad(&format!("{k} must be a number or null")) },
        }
    }
    let before: Vec<f64> = changes.iter().map(|(k, _)| if crate::tune::knob(k).is_some() { crate::tune::get(k) } else { 0.0 }).collect();
    let json = match crate::tune::set(&changes) { Ok(j) => j, Err(e) => return bad(&e) };
    if let Err(e) = ctx.store.set_meta("tune", &json) {
        return fail(503, "unavailable", &e);
    }
    let diff: Vec<String> = changes.iter().zip(before).map(|((k, _), old)| format!("{k} {old} -> {}", crate::tune::get(k))).collect();
    audit(ctx, "tune.changed", actor, &diff.join(", "));
    crate::head::tune_changed(ctx);
    json_out(200, json!({"tunables": crate::tune::listing()}))
}

/// PUT /api/v1/hierarchy/settings {"mode": "off"|"manual"|"auto", "lease_s", "lab_head_min_pcs", "lab_silence_s", "lab_head_standby"}: the same tunables as `ck admin tune`.
fn put_hier_settings(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let mut changes: Vec<(String, Option<f64>)> = vec![];
    for (k, x) in v.as_object().into_iter().flatten() {
        let n = match (k.as_str(), x) {
            ("mode", Value::String(m)) => match m.as_str() { "off" => 0.0, "manual" => 1.0, "auto" => 2.0, _ => return bad("mode must be off, manual or auto") },
            ("lab_head_standby", Value::Bool(b)) => *b as u8 as f64,
            ("lease_s" | "lab_head_min_pcs" | "lab_silence_s", x) => match x.as_f64() { Some(n) => n, None => return bad(&format!("{k} must be a number")) },
            _ => return bad("settings: mode (off|manual|auto), lease_s, lab_head_min_pcs, lab_silence_s, lab_head_standby (true|false)"),
        };
        changes.push((if k == "mode" { "hier_mode".into() } else { k.clone() }, Some(n)));
    }
    let before: Vec<f64> = changes.iter().map(|(k, _)| crate::tune::get(k)).collect();
    let json = match crate::tune::set(&changes) { Ok(j) => j, Err(e) => return bad(&e) };
    if let Err(e) = ctx.store.set_meta("tune", &json) {
        return fail(503, "unavailable", &e);
    }
    let diff: Vec<String> = changes.iter().zip(before).map(|((k, _), old)| format!("{k} {old} -> {}", crate::tune::get(k))).collect();
    audit(ctx, "hier.settings", actor, &diff.join(", "));
    crate::head::tune_changed(ctx);
    json_out(200, crate::hier::json(ctx))
}

fn put_ha_settings(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let mut h = ha_settings(ctx);
    if let Some(x) = v.get("target_standbys") {
        match uint(x).filter(|n| *n <= 5) { Some(n) => h.target_standbys = n as u32, None => return bad("target_standbys must be 0-5") }
    }
    if let Some(x) = v.get("selector") {
        let Some(sel) = x.as_str() else { return bad("selector must be a string") };
        if let Err(e) = select::parse(sel) { return bad(&format!("selector: {e}")) }
        h.selector = sel.into();
    }
    if let Err(e) = ctx.store.set_meta("ha_settings", &serde_json::to_string(&h).unwrap_or_default()) {
        return fail(503, "unavailable", &e);
    }
    audit(ctx, "ha.settings", actor, &format!("{} standbys, selector {:?}", h.target_standbys, h.selector));
    crate::head::ha_settings_changed(ctx);
    json_out(200, json!({"settings": h}))
}

fn set_states(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let Some(state) = str_field(&v, "state").filter(|s| NODE_STATES.contains(s)) else { return bad(&format!("state must be one of {}", NODE_STATES.join(", "))) };
    let ids: Option<Vec<&str>> = v.get("node_ids").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect());
    let Some(ids) = ids.filter(|a| !a.is_empty() && a.len() <= 1000) else { return bad("node_ids must be a non-empty list of node ids") };
    let reason: String = str_field(&v, "reason").unwrap_or("").chars().take(200).collect();
    if let Some(missing) = ids.iter().find(|i| ctx.store.node(i).is_none()) {
        return fail(404, "not_found", &format!("no such node {missing}"));
    }
    let mut out = vec![];
    for id in &ids {
        match ctx.store.set_node_state(id, state, &reason, actor, now_ms()) {
            Ok(n) => {
                changed(ctx, &n);
                out.push(n);
            }
            Err(e) => return fail(409, "conflict", &e),
        }
    }
    audit(ctx, "node.state", actor, &format!("{} -> {state}{}{}", ids.join(","), if reason.is_empty() { "" } else { ": " }, reason));
    let (on, org) = (online_ids(ctx), Org::load(ctx));
    json_out(200, json!({"nodes": out.iter().map(|n| node_json(ctx, n, on.contains(&n.node_id), &org)).collect::<Vec<_>>()}))
}

fn place(ctx: &Ctx, r: &Req, id: &str, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let seat = match v.get("lab_id") {
        Some(Value::Null) => None,
        Some(l) => match (uint(l), v.get("x").and_then(uint), v.get("y").and_then(uint)) {
            (Some(l), Some(x), Some(y)) if x < u32::MAX as u64 && y < u32::MAX as u64 => Some((l, x as u32, y as u32)),
            _ => return bad("lab_id, x and y must be whole numbers"),
        },
        None => return bad("lab_id is required (null to unplace)"),
    };
    match ctx.store.place_node(id, seat) {
        Ok(n) => {
            let d = seat.map_or("unplaced".into(), |(l, x, y)| format!("lab {l} ({x},{y})"));
            audit(ctx, "node.placed", actor, &format!("{id} {d}"));
            changed(ctx, &n);
            json_out(200, json!({"node": node_json(ctx, &n, online_ids(ctx).contains(id), &Org::load(ctx))}))
        }
        Err(e) if e == "seat taken" => fail(409, "conflict", &e),
        Err(e) if e.starts_with("no ") => fail(404, "not_found", &e),
        Err(e) => bad(&e),
    }
}

fn dim(v: Option<&Value>) -> Result<Option<u32>, Out> {
    match v {
        None => Ok(None),
        Some(x) => uint(x).filter(|n| (1..=64).contains(n)).map(|n| Some(n as u32)).ok_or_else(|| bad("w and h must be whole numbers from 1 to 64")),
    }
}

fn lab_name(v: &Value) -> Result<Option<String>, Out> {
    match v.get("name") {
        None => Ok(None),
        Some(n) => n.as_str().map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 64).map(|s| Some(s.to_string())).ok_or_else(|| bad("name must be 1 to 64 characters")),
    }
}

fn lab_create(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, w, h) = match (lab_name(&v), dim(v.get("w")), dim(v.get("h"))) {
        (Ok(Some(n)), Ok(Some(w)), Ok(Some(h))) => (n, w, h),
        (Err(o), _, _) | (_, Err(o), _) | (_, _, Err(o)) => return o,
        _ => return bad("name, w and h are required"),
    };
    match ctx.store.create_lab(&name, w, h, now_ms()) {
        Ok(l) => {
            audit(ctx, "lab.created", actor, &format!("{} {}x{}", l.name, l.w, l.h));
            json_out(200, json!({"lab": l}))
        }
        Err(e) => fail(409, "conflict", &e),
    }
}

fn lab_update(ctx: &Ctx, r: &Req, id: &str, actor: &str) -> Out {
    let Ok(lid) = id.parse::<u64>() else { return fail(404, "not_found", "no such lab") };
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, w, h) = match (lab_name(&v), dim(v.get("w")), dim(v.get("h"))) {
        (Ok(n), Ok(w), Ok(h)) => (n, w, h),
        (Err(o), _, _) | (_, Err(o), _) | (_, _, Err(o)) => return o,
    };
    match ctx.store.update_lab(lid, name.as_deref(), w, h) {
        Ok(l) => {
            audit(ctx, "lab.updated", actor, &format!("{} {}x{}", l.name, l.w, l.h));
            let (on, org) = (online_ids(ctx), Org::load(ctx));
            ctx.store.nodes().iter().filter(|n| n.lab_id == Some(lid) || n.lab_id.is_none()).for_each(|n| publish_node(ctx, n, on.contains(&n.node_id), &org));
            json_out(200, json!({"lab": l}))
        }
        Err(e) if e.starts_with("no lab") => fail(404, "not_found", &e),
        Err(e) => bad(&e),
    }
}

// ---- organisation (API.md "Organisation")

fn reserve_view(r: &crate::jobs::ReserveRow) -> Option<sched::ReserveView> {
    Some(sched::ReserveView { selector: select::parse(&r.selector).ok()?, keep_n: r.keep_n, keep_pct: r.keep_pct })
}

/// A cap or reserve changed: let the head re-evaluate every node (it pushes thread caps and wakes the dispatcher).
fn wake_all(ctx: &Ctx) {
    for n in ctx.store.nodes() {
        tell_head(ctx, &n.node_id);
    }
}

fn select_preview(ctx: &Ctx, r: &Req) -> Out {
    let q = pct_decode(r.query.get("q").map_or("", String::as_str));
    match select::parse(&q) {
        Err(e) => json_out(200, json!({"count": 0, "node_ids": [], "error": e})),
        Ok(sel) => {
            let (org, on) = (Org::load(ctx), ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| Some((w.hello.as_ref()?.node_id.clone(), (w.threads, w.cpu)))).collect::<HashMap<_, _>>());
            let ids: Vec<String> = ctx.store.nodes().iter().filter(|n| sel.matches(&org.attrs(n, on.get(&n.node_id).copied()))).map(|n| n.node_id.clone()).collect();
            json_out(200, json!({"count": ids.len(), "node_ids": ids, "error": null}))
        }
    }
}

fn set_labels(ctx: &Ctx, r: &Req, id: &str, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if ctx.store.node(id).is_none() {
        return fail(404, "not_found", "no such node");
    }
    let mut ch = vec![];
    for (k, x) in v.as_object().unwrap() {
        let val = match x {
            Value::Null => None,
            Value::String(s) => Some(s.as_str()),
            _ => return bad("label values must be strings or null"),
        };
        // ponytail: not atomic; a bad key mid-body leaves the earlier keys set
        if let Err(e) = ctx.store.set_label(id, k, val) {
            return bad(&e);
        }
        ch.push(format!("{k}={}", val.unwrap_or("")));
    }
    audit(ctx, "label.set", actor, &format!("{id} {}", ch.join(" ")));
    let n = ctx.store.node(id).unwrap();
    changed(ctx, &n);
    json_out(200, json!({"node": node_json(ctx, &n, online_ids(ctx).contains(id), &Org::load(ctx))}))
}

fn changed_ids(ctx: &Ctx, ids: impl IntoIterator<Item = String>) {
    for id in ids {
        if let Some(n) = ctx.store.node(&id) {
            changed(ctx, &n);
        }
    }
}

fn put_pool(ctx: &Ctx, r: &Req, name: &str, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let Some(members) = v.get("members").and_then(|a| a.as_array()).and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<Vec<_>>>()) else { return bad("members must be a list of node ids") };
    let note = str_field(&v, "note").unwrap_or("").chars().take(200).collect::<String>();
    let before: Vec<String> = ctx.store.pools().into_iter().find(|p| p.name == name).map(|p| p.members).unwrap_or_default();
    match ctx.store.put_pool(name, &members, &note) {
        Ok(p) => {
            audit(ctx, "pool.put", actor, &format!("{} ({} nodes)", p.name, p.members.len()));
            changed_ids(ctx, before.into_iter().chain(p.members.iter().cloned()));
            json_out(200, json!({"pool": p}))
        }
        Err(e) => bad(&e),
    }
}

fn delete_pool(ctx: &Ctx, name: &str, actor: &str) -> Out {
    let Some(p) = ctx.store.pools().into_iter().find(|p| p.name == name) else { return fail(404, "not_found", "no such pool") };
    match ctx.store.delete_pool(name) {
        Ok(()) => {
            audit(ctx, "pool.deleted", actor, name);
            changed_ids(ctx, p.members);
            json_out(200, json!({}))
        }
        Err(e) => fail(409, "conflict", &e),
    }
}

fn sel_field(v: &Value) -> Result<String, Out> {
    let s = str_field(v, "selector").ok_or_else(|| bad("selector is required"))?;
    select::parse(s).map_err(|e| bad(&e))?;
    Ok(s.trim().to_string())
}

fn add_cap(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let sel = match sel_field(&v) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let Some(max) = v.get("max_threads").and_then(uint).filter(|n| (1..=4096).contains(n)) else { return bad("max_threads must be a whole number from 1 to 4096") };
    let note = str_field(&v, "note").unwrap_or("").chars().take(200).collect::<String>();
    match ctx.store.add_cap(&sel, Some(max as u32), &note) {
        Ok(c) => {
            audit(ctx, "cap.added", actor, &format!("#{} [{}] max {max} threads", c.id, c.selector));
            wake_all(ctx);
            json_out(200, json!({"cap": c}))
        }
        Err(e) => bad(&e),
    }
}

fn window_json(w: &crate::jobs::WindowRow) -> Value {
    let mut v = serde_json::to_value(w).unwrap_or_default();
    v["active_now"] = json!(window_view(w).is_some_and(|wv| {
        let (blocked, cap) = sched::time_rule(&select::Attrs::new(), sched::clock(now_ms(), tz_offset()), &[wv], &[]);
        blocked || cap.is_some()
    }));
    v
}

/// The scheduler's view of a window with a match-everything selector (so "active now" does not depend on a PC).
fn window_view(w: &crate::jobs::WindowRow) -> Option<sched::WindowView> {
    Some(sched::WindowView {
        selector: select::parse("").ok()?,
        days: sched::parse_days(&w.days)?,
        start_min: sched::parse_hhmm(&w.start)?,
        end_min: sched::parse_hhmm(&w.end)?,
        block: w.action == "block",
        cap: w.cap_threads,
    })
}

fn reservation_json(x: &crate::jobs::ReservationRow, now: i64) -> Value {
    let mut v = serde_json::to_value(x).unwrap_or_default();
    v["active_now"] = json!(x.start_ms <= now && now < x.end_ms);
    v
}

fn add_window(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let sel = match sel_field(&v) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let g = |k: &str| str_field(&v, k).unwrap_or("").to_string();
    let cap_threads = v.get("cap_threads").filter(|x| !x.is_null()).map(|x| uint(x).filter(|n| (1..=4096).contains(n)).map(|n| n as u32));
    if cap_threads == Some(None) {
        return bad("cap_threads must be a whole number from 1 to 4096");
    }
    let w = crate::jobs::WindowRow { id: 0, selector: sel, days: g("days"), start: g("start"), end: g("end"), action: g("action"), cap_threads: cap_threads.flatten(), note: g("note").chars().take(200).collect() };
    match ctx.store.add_window(&w) {
        Ok(w) => {
            audit(ctx, "window.added", actor, &format!("#{} [{}] {} {}-{} {}", w.id, w.selector, w.days, w.start, w.end, w.action));
            wake_all(ctx);
            json_out(200, json!({"window": window_json(&w)}))
        }
        Err(e) => bad(&e),
    }
}

fn add_reservation(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let sel = match sel_field(&v) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let (Some(a), Some(b)) = (v.get("start_ms").and_then(uint), v.get("end_ms").and_then(uint)) else { return bad("start_ms and end_ms must be Unix milliseconds") };
    let note = str_field(&v, "note").unwrap_or("").chars().take(200).collect();
    match ctx.store.add_reservation(&crate::jobs::ReservationRow { id: 0, selector: sel, start_ms: a as i64, end_ms: b as i64, note }) {
        Ok(x) => {
            audit(ctx, "reservation.added", actor, &format!("#{} [{}] {}..{}", x.id, x.selector, x.start_ms, x.end_ms));
            wake_all(ctx);
            json_out(200, json!({"reservation": reservation_json(&x, now_ms())}))
        }
        Err(e) => bad(&e),
    }
}

fn blobs_list(ctx: &Ctx) -> Out {
    let holders = ctx.sh.0.lock().unwrap().workers.values().flat_map(|w| w.have.iter().cloned()).fold(HashMap::<String, u64>::new(), |mut m, s| {
        *m.entry(s).or_default() += 1;
        m
    });
    let rows = ctx.store.blob_rows();
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let list: Vec<Value> = rows.iter().map(|(sha, size, refs, c, l)| json!({"sha256": sha, "size": size, "refs": refs, "holders": holders.get(sha).copied().unwrap_or(0), "created_ms": c, "last_used_ms": l})).collect();
    json_out(200, json!({"blobs": list, "store_bytes": total, "limit_bytes": (crate::tune::get("blob_store_gb") * 1073741824.0) as u64}))
}

fn net_json(ctx: &Ctx) -> Out {
    let st = ctx.sh.0.lock().unwrap();
    let mut labs: HashMap<String, Vec<f64>> = HashMap::new();
    let nodes: Vec<Value> = st
        .workers
        .values()
        .filter(|w| w.hello.as_ref().is_some_and(|h| !h.virtual_node))
        .map(|w| {
            let lab = crate::blobs::lab_of(&w.addr);
            labs.entry(lab.clone()).or_default().extend(w.net_mbps);
            let h = w.hello.as_ref().unwrap();
            json!({"node_id": h.node_id, "host": h.host, "net_mbps": w.net_mbps, "rtt_ms": w.rtt_ms, "probed_ms": w.probed.map(|t| now_ms() - t.elapsed().as_millis() as i64), "lab": lab})
        })
        .collect();
    drop(st);
    let labs: Vec<Value> = labs.into_iter().map(|(lab, mut v)| {
        v.sort_by(|a, b| a.total_cmp(b));
        json!({"lab": lab, "pcs": v.len(), "mbps": v.get(v.len() / 2)})
    }).collect();
    let (up, guess) = crate::blobs::head_uplink();
    json_out(200, json!({"head_uplink_mbps": up, "head_uplink_assumed": guess, "head_bytes_sent": crate::blobs::SERVED.load(std::sync::atomic::Ordering::Relaxed), "nodes": nodes, "labs": labs}))
}

fn net_probe(ctx: &Ctx, r: &Req) -> Out {
    let v = parse_body(r).unwrap_or(json!({}));
    let ids: Vec<String> = v["node_ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
    let mut st = ctx.sh.0.lock().unwrap();
    let burst = v["burst"].as_bool() == Some(true);
    let mut n = 0;
    for w in st.workers.values_mut().filter(|w| w.hello.as_ref().is_some_and(|h| !h.virtual_node && (ids.is_empty() || ids.contains(&h.node_id)))) {
        // burst: every PC at once, to learn the head uplink; otherwise measure each PC's own link, two at a time
        if burst {
            (w.probe_now, w.probe_burst) = (true, true);
        } else {
            w.probed = None;
        }
        n += 1;
    }
    json_out(200, json!({"queued": n}))
}

/// New form: ?bytes=&work_s=&items=&selector=&max_pcs=&sha= (docs/API.md "Bulk distribution").
fn estimate_bulk(ctx: &Ctx, r: &Req) -> Out {
    let num = |k: &str| r.query.get(k).and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0);
    let sel = match select::parse(&pct_decode(r.query.get("selector").map_or("", String::as_str))) {
        Ok(s) => s,
        Err(e) => return bad(&e),
    };
    let sha = r.query.get("sha").filter(|s| crate::blobs::valid_sha(s)).cloned();
    let (bytes, work_s, items, max_pcs) = (num("bytes").unwrap_or(0.0), num("work_s").unwrap_or(0.0), num("items").unwrap_or(0.0) as u64, num("max_pcs").unwrap_or(0.0) as usize);
    let (pcs, mut assumed) = crate::blobs::est_pcs(ctx, &sel, sha.as_deref());
    let (up, guess) = crate::blobs::head_uplink();
    if guess {
        assumed.push("head_uplink");
    }
    let (fanout, seeds) = (crate::tune::n("blob_peer_fanout") as usize, crate::tune::n("blob_seeds") as usize);
    let Some(p) = crate::blobs::plan(&pcs, bytes, work_s, items, up, fanout, seeds, max_pcs) else {
        return json_out(200, json!({"pcs": 0, "matching_pcs": 0, "best": null, "why": "no PC can work now", "assumed": assumed}));
    };
    let alt = |a: &crate::blobs::Alt| json!({"strategy": a.strategy, "pcs": a.pcs, "transfer_s": a.transfer_s, "finish_s": a.finish_s});
    let fin = |t: f64| t + p.compute_s + p.overhead_s;
    let why = if bytes > 0.0 {
        format!("data {}: {}, {} on {} PCs; {} would take {}", crate::blobs::human_bytes(bytes), p.best.strategy, crate::blobs::human_secs(p.best.transfer_s), p.best.pcs,
            if p.best.strategy == "peer" { "direct" } else { "peer" }, crate::blobs::human_secs(if p.best.strategy == "peer" { p.transfer_direct_s } else { p.transfer_peer_s }))
    } else {
        "no data to move".to_string()
    };
    json_out(200, json!({
        "pcs": p.best.pcs, "matching_pcs": pcs.len(), "bytes": bytes, "cached_pcs": pcs.iter().filter(|c| c.cached).count(), "work_s": work_s,
        "compute_s": p.compute_s, "overhead_s": p.overhead_s,
        "transfer_s": {"direct": p.transfer_direct_s, "peer": p.transfer_peer_s}, "finish_s": {"direct": fin(p.transfer_direct_s), "peer": fin(p.transfer_peer_s)},
        "head_bytes": {"direct": p.head_bytes_direct, "peer": p.head_bytes_peer},
        "best": {"strategy": p.best.strategy, "pcs": p.best.pcs, "finish_s": p.best.finish_s},
        "alternatives": p.alternatives.iter().map(alt).collect::<Vec<_>>(), "why": why, "head_uplink_mbps": up, "assumed": assumed
    }))
}

/// How long would n numbers take now (SCH-03)?
fn estimate(ctx: &Ctx, r: &Req) -> Out {
    if r.query.contains_key("bytes") || r.query.contains_key("work_s") {
        return estimate_bulk(ctx, r);
    }
    let n = r.query.get("n").and_then(|s| s.parse::<u64>().ok()).filter(|n| *n > 0);
    let Some(n) = n else { return bad("n must be a whole number above 0") };
    let sel = match select::parse(&pct_decode(r.query.get("selector").map_or("", String::as_str))) {
        Ok(s) => s,
        Err(e) => return bad(&e),
    };
    let (pcs, rate) = head::fleet_rate_for(ctx, &sel);
    json_out(200, json!({"matching_pcs": pcs, "fleet_rate": rate, "eta_secs": sched::eta_secs(n, rate)}))
}

fn add_reserve(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let sel = match sel_field(&v) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let get = |k: &str, max: u64| match v.get(k) {
        None | Some(Value::Null) => Some(0),
        Some(x) => uint(x).filter(|n| *n <= max),
    };
    let (Some(n), Some(pct)) = (get("keep_n", 100_000), get("keep_pct", 100)) else { return bad("keep_n must be a whole number and keep_pct 0 to 100") };
    if n == 0 && pct == 0 {
        return bad("give keep_n or keep_pct above 0");
    }
    let note = str_field(&v, "note").unwrap_or("").chars().take(200).collect::<String>();
    match ctx.store.add_reserve(&sel, n as u32, pct as u32, &note) {
        Ok(x) => {
            audit(ctx, "reserve.added", actor, &format!("#{} [{}] keep {n} / {pct}%", x.id, x.selector));
            wake_all(ctx);
            json_out(200, json!({"reserve": x}))
        }
        Err(e) => bad(&e),
    }
}

// ---- high availability

pub fn ha_json(ctx: &Ctx) -> Value {
    let s = ctx.repl.status.lock().unwrap().clone();
    json!({"role": s.role, "epoch": s.epoch, "last_seq": s.last_seq, "durable_seq": crate::repl::durable_seq(ctx),
           "standbys": s.standbys.iter().map(|(n, a, l)| json!({"node_id": n, "acked_seq": a, "lag": l})).collect::<Vec<_>>(),
           "membership": s.membership, "connected": s.connected, "dispatching": s.dispatching, "mixed": s.mixed, "head": head_json(&s)})
}

/// Who is primary now and since when (epoch start). A standby reports the primary it follows (node_id unknown there).
/// ponytail: the first epoch seen counts from process start, a later one from the moment this process saw it change.
fn head_json(s: &crate::repl::Status) -> Value {
    static SINCE: std::sync::Mutex<Option<(u64, i64)>> = std::sync::Mutex::new(None);
    static ADDR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let since = {
        let mut g = SINCE.lock().unwrap();
        match *g {
            Some((e, t)) if e == s.epoch => t,
            prev => {
                let t = if prev.is_none() { now_ms() - (head::now() * 1000.0) as i64 } else { now_ms() };
                *g = Some((s.epoch, t));
                t
            }
        }
    };
    if s.role == "standby" {
        return json!({"node_id": Value::Null, "addr": s.primary, "since_ms": since});
    }
    let addr = ADDR.get_or_init(|| format!("{}:{}", crate::proto::local_ip().map_or("127.0.0.1".into(), |i| i.to_string()), crate::proto::port()));
    json!({"node_id": crate::identity::node_id(), "addr": addr, "since_ms": since})
}

/// What makes an `ha` SSE event worth sending: role, epoch and the standby set (not the counters).
fn ha_sig(ctx: &Ctx) -> String {
    let s = ctx.repl.status.lock().unwrap();
    format!("{}/{}/{:?}", s.role, s.epoch, s.standbys.iter().map(|x| &x.0).collect::<Vec<_>>())
}

// ---- config export / import (ORG-11)

pub fn export_config(ctx: &Ctx) -> Value {
    let labs = ctx.store.labs();
    let lab_name = |id: u64| labs.iter().find(|l| l.id == id).map(|l| l.name.clone());
    let placements: Vec<Value> = ctx.store.nodes().iter().filter_map(|n| Some(json!({"node_id": n.node_id, "lab": lab_name(n.lab_id?)?, "x": n.x?, "y": n.y?}))).collect();
    json!({
        "version": 1,
        "labs": labs.iter().map(|l| json!({"name": l.name, "w": l.w, "h": l.h})).collect::<Vec<_>>(),
        "placements": placements,
        "labels": ctx.store.labels().iter().map(|(n, k, v)| json!({"node_id": n, "key": k, "value": v})).collect::<Vec<_>>(),
        "pools": ctx.store.pools(),
        "caps": ctx.store.caps().iter().map(|c| json!({"selector": c.selector, "max_threads": c.max_threads, "note": c.note})).collect::<Vec<_>>(),
        "reserves": ctx.store.reserves().iter().map(|c| json!({"selector": c.selector, "keep_n": c.keep_n, "keep_pct": c.keep_pct, "note": c.note})).collect::<Vec<_>>(),
    })
}

fn config_import(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let dry = r.query.get("dry_run").is_some_and(|d| d == "1" || d == "true");
    match import_config(ctx, &v, dry) {
        Ok(lines) => {
            if !dry {
                audit(ctx, "config.applied", actor, &format!("{} changes", lines.len()));
                changed_ids(ctx, ctx.store.nodes().into_iter().map(|n| n.node_id));
            }
            json_out(200, json!({"changes": lines}))
        }
        Err(e) => bad(&e),
    }
}

fn list<'a>(v: &'a Value, k: &str) -> Result<&'a [Value], String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        _ => Err(format!("{k} must be a list")),
    }
}

fn s_of<'a>(o: &'a Value, k: &str, what: &str) -> Result<&'a str, String> {
    o.get(k).and_then(|x| x.as_str()).ok_or_else(|| format!("{what}: {k} must be a string"))
}

/// Additive + update: creates and changes things named in the file, never deletes anything and never creates nodes
/// (entries for unknown nodes are skipped and reported). Everything is validated before the first write.
/// ponytail: a cap/reserve counts as present when selector and numbers match; changing a number adds a second row.
pub fn import_config(ctx: &Ctx, v: &Value, dry: bool) -> Result<Vec<String>, String> {
    if v.get("version").and_then(uint) != Some(1) {
        return Err("unsupported config version (this head reads version 1)".into());
    }
    let st = &ctx.store;
    let known: HashMap<String, NodeRow> = st.nodes().into_iter().map(|n| (n.node_id.clone(), n)).collect();
    let mut out = vec![];
    // validate
    let mut labs = vec![];
    for l in list(v, "labs")? {
        let (n, w, h) = (s_of(l, "name", "lab")?, l.get("w").and_then(uint), l.get("h").and_then(uint));
        let (Some(w), Some(h)) = (w.filter(|x| (1..=64).contains(x)), h.filter(|x| (1..=64).contains(x))) else { return Err(format!("lab {n}: w and h must be 1 to 64")) };
        labs.push((n, w as u32, h as u32));
    }
    let mut seats = vec![];
    for p in list(v, "placements")? {
        let id = s_of(p, "node_id", "placement")?;
        let (Some(x), Some(y)) = (p.get("x").and_then(uint), p.get("y").and_then(uint)) else { return Err(format!("placement {id}: x and y must be whole numbers")) };
        seats.push((id, s_of(p, "lab", "placement")?, x as u32, y as u32));
    }
    let mut lbls = vec![];
    for l in list(v, "labels")? {
        lbls.push((s_of(l, "node_id", "label")?, s_of(l, "key", "label")?, s_of(l, "value", "label")?));
    }
    let mut pools = vec![];
    for p in list(v, "pools")? {
        let members: Vec<String> = p.get("members").and_then(|m| m.as_array()).and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect()).ok_or("pool: members must be a list of node ids")?;
        pools.push((s_of(p, "name", "pool")?, members, p.get("note").and_then(|x| x.as_str()).unwrap_or("")));
    }
    let mut caps = vec![];
    for c in list(v, "caps")? {
        let sel = s_of(c, "selector", "cap")?;
        select::parse(sel).map_err(|e| format!("cap: {e}"))?;
        let m = c.get("max_threads").and_then(uint).filter(|n| *n >= 1).ok_or("cap: max_threads must be a whole number above 0")?;
        caps.push((sel, m as u32, c.get("note").and_then(|x| x.as_str()).unwrap_or("")));
    }
    let mut reserves = vec![];
    for c in list(v, "reserves")? {
        let sel = s_of(c, "selector", "reserve")?;
        select::parse(sel).map_err(|e| format!("reserve: {e}"))?;
        let (n, p) = (c.get("keep_n").and_then(uint).unwrap_or(0), c.get("keep_pct").and_then(uint).unwrap_or(0));
        if p > 100 || (n == 0 && p == 0) {
            return Err(format!("reserve [{sel}]: keep_n or keep_pct (0 to 100) needed"));
        }
        reserves.push((sel, n as u32, p as u32, c.get("note").and_then(|x| x.as_str()).unwrap_or("")));
    }
    // apply (or only describe)
    let mut lab_ids: HashMap<String, u64> = st.labs().into_iter().map(|l| (l.name.clone(), l.id)).collect();
    let cur_labs = st.labs();
    for (name, w, h) in &labs {
        match cur_labs.iter().find(|l| l.name == *name) {
            None => {
                out.push(format!("create lab {name} {w}x{h}"));
                if !dry {
                    lab_ids.insert(name.to_string(), st.create_lab(name, *w, *h, now_ms())?.id);
                } else {
                    lab_ids.insert(name.to_string(), 0);
                }
            }
            Some(l) if l.w != *w || l.h != *h => {
                out.push(format!("resize lab {name} {}x{} -> {w}x{h}", l.w, l.h));
                if !dry {
                    st.update_lab(l.id, None, Some(*w), Some(*h))?;
                }
            }
            Some(_) => {}
        }
    }
    for (id, lab, x, y) in &seats {
        let Some(n) = known.get(*id) else {
            out.push(format!("skip placement of unknown node {id}"));
            continue;
        };
        let Some(&lid) = lab_ids.get(*lab) else { return Err(format!("placement {id}: no lab named {lab}")) };
        if n.lab_id != Some(lid) || n.x != Some(*x) || n.y != Some(*y) {
            out.push(format!("place {id} in lab {lab} at ({x},{y})"));
            if !dry {
                st.place_node(id, Some((lid, *x, *y)))?;
            }
        }
    }
    let cur_labels = st.labels();
    for (id, k, val) in &lbls {
        if !known.contains_key(*id) {
            out.push(format!("skip label of unknown node {id}"));
        } else if !cur_labels.iter().any(|(n, kk, vv)| n == id && kk == k && vv == val) {
            out.push(format!("label {id} {k}={val}"));
            if !dry {
                st.set_label(id, k, Some(val))?;
            }
        }
    }
    let cur_pools = st.pools();
    for (name, members, note) in &pools {
        let (keep, skipped): (Vec<String>, Vec<String>) = members.iter().cloned().partition(|m| known.contains_key(m));
        skipped.iter().for_each(|m| out.push(format!("pool {name}: skip unknown node {m}")));
        match cur_pools.iter().find(|p| p.name == *name) {
            Some(p) if p.members == keep && p.note == *note => {}
            old => {
                out.push(format!("{} pool {name} ({} nodes)", if old.is_some() { "update" } else { "create" }, keep.len()));
                if !dry {
                    st.put_pool(name, &keep, note)?;
                }
            }
        }
    }
    let cur_caps = st.caps();
    for (sel, m, note) in &caps {
        if !cur_caps.iter().any(|c| c.selector == *sel && c.max_threads == Some(*m)) {
            out.push(format!("add cap [{sel}] max {m} threads"));
            if !dry {
                st.add_cap(sel, Some(*m), note)?;
            }
        }
    }
    let cur_res = st.reserves();
    for (sel, n, p, note) in &reserves {
        if !cur_res.iter().any(|c| c.selector == *sel && c.keep_n == *n && c.keep_pct == *p) {
            out.push(format!("add reserve [{sel}] keep {n} / {p}%"));
            if !dry {
                st.add_reserve(sel, *n, *p, note)?;
            }
        }
    }
    Ok(out)
}

/// XXXX-XXXX-XXXX from a 31-letter alphabet without 0/O/1/I/L lookalikes.
fn new_join_code() -> String {
    const A: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789"; // 31 symbols
    let n = crate::auth::nonce();
    let c: Vec<char> = n.iter().filter(|b| **b < 248).take(12).map(|b| A[(*b as usize) % A.len()] as char).collect(); // 248 = 8*31: no modulo bias
    assert!(c.len() == 12, "nonce too short");
    c.chunks(4).map(|g| g.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

fn join_code(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = if r.body.is_empty() { json!({}) } else { match parse_body(r) { Ok(v) => v, Err(o) => return o } };
    let get = |k: &str, d: u64, max: u64| match v.get(k) {
        None | Some(Value::Null) => Some(d),
        Some(x) => uint(x).filter(|n| (1..=max).contains(n)),
    };
    let (Some(uses), Some(ttl)) = (get("uses", 50, 500), get("ttl_min", 120, 10080)) else { return bad("uses must be 1 to 500 and ttl_min 1 to 10080") };
    let auto = v.get("auto_approve").and_then(|b| b.as_bool()).unwrap_or(false);
    let (now, code) = (now_ms(), new_join_code());
    let expires = now + ttl as i64 * 60_000;
    if let Err(e) = ctx.store.add_join_code(&users::sha256_hex(code.as_bytes()), uses as u32, expires, auto, actor, now) {
        return fail(500, "internal", &e);
    }
    audit(ctx, "joincode.created", actor, &format!("{uses} uses, {ttl} min{}", if auto { ", auto-approve" } else { "" }));
    json_out(200, json!({"code": code, "expires_ms": expires, "uses": uses, "auto_approve": auto}))
}

fn bootstrap(ctx: &Ctx, auth: &Auth, r: &Req) -> Out {
    if auth.open {
        return fail(409, "conflict", "auth is off");
    }
    if !r.ip.is_loopback() {
        return fail(403, "forbidden", "bootstrap is only accepted from 127.0.0.1");
    }
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, pw) = (str_field(&v, "name").unwrap_or(""), str_field(&v, "password").unwrap_or(""));
    if !valid_name(name) {
        return bad("name must be 1 to 64 letters, digits, '.', '_' or '-'");
    }
    if pw.chars().count() < 12 {
        return bad("password must be at least 12 characters");
    }
    let _g = auth.bootstrap_lock.lock().unwrap();
    if !ctx.store.users().is_empty() {
        return fail(409, "conflict", "already set up; sign in");
    }
    match ctx.store.create_user(name, &users::hash_password(pw, users::ITERS), "admin", now_ms()) {
        Ok(u) => {
            audit(ctx, "user.bootstrap", name, "first admin created");
            with_cookie(json_out(200, json!({"user": {"name": u.name, "role": u.role}})), session_cookie(&auth.start_session(name), 7 * 86400))
        }
        Err(e) => fail(409, "conflict", &e),
    }
}

fn login(ctx: &Ctx, auth: &Auth, r: &Req) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, pw) = (str_field(&v, "name").unwrap_or(""), str_field(&v, "password").unwrap_or(""));
    match auth.login(&ctx.store, name, pw, &r.ip.to_string()) {
        Ok((w, sid)) => {
            audit(ctx, "user.login", &w.name, &r.ip.to_string());
            with_cookie(json_out(200, json!({"user": {"name": w.name, "role": w.role.name()}})), session_cookie(&sid, 7 * 86400))
        }
        Err(LoginErr::Throttled) => {
            // no audit row: the 5th failure that caused the throttle is on record, and a flood of throttled tries must not
            // write one event row (and one 8 KiB SSE frame per open console) per request
            fail(429, "too_many_requests", "too many failed sign-ins; try again in a few minutes")
        }
        Err(LoginErr::Bad) => {
            audit(ctx, "user.login_failed", name, &r.ip.to_string());
            fail(401, "unauthorized", "wrong name or password")
        }
    }
}

fn create_user(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, pw) = (str_field(&v, "name").unwrap_or(""), str_field(&v, "password").unwrap_or(""));
    let Some(role) = str_field(&v, "role").and_then(Role::parse) else { return bad("role must be viewer, researcher, operator or admin") };
    if !valid_name(name) {
        return bad("name must be 1 to 64 letters, digits, '.', '_' or '-'");
    }
    if pw.chars().count() < 12 {
        return bad("password must be at least 12 characters");
    }
    match ctx.store.create_user(name, &users::hash_password(pw, users::ITERS), role.name(), now_ms()) {
        Ok(u) => {
            audit(ctx, "user.created", actor, &format!("{} ({})", u.name, u.role));
            json_out(200, json!({"user": user_json(ctx, &u)}))
        }
        Err(e) => fail(409, "conflict", &e),
    }
}

fn user_json(ctx: &Ctx, u: &crate::jobs::UserRow) -> Value {
    let q = head::quota_of(ctx, &u.name);
    let mut v = json!(u);
    v["quota"] = json!({"max_pcs": q.0, "max_running": q.1});
    v
}

/// PUT /api/v1/users/{name}/quota (admin): {"max_pcs", "max_running"}, 0 = no limit; a missing key keeps its value.
fn put_quota(ctx: &Ctx, r: &Req, name: &str, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let Some(u) = ctx.store.users().into_iter().find(|u| u.name == name) else { return fail(404, "not_found", "no such user") };
    let cur = head::quota_of(ctx, name);
    let get = |k: &str, old: u32| match v.get(k) {
        None => Ok(old),
        Some(x) => uint(x).filter(|n| *n <= 1_000_000).map(|n| n as u32).ok_or(format!("{k} must be a whole number from 0 to 1000000")),
    };
    let (max_pcs, max_running) = match (get("max_pcs", cur.0), get("max_running", cur.1)) { (Ok(a), Ok(b)) => (a, b), (Err(e), _) | (_, Err(e)) => return bad(&e) };
    let mut all: Value = ctx.store.meta("quotas").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if (max_pcs, max_running) == (0, 0) {
        all.as_object_mut().map(|o| o.remove(name));
    } else {
        all[name] = json!({"max_pcs": max_pcs, "max_running": max_running});
    }
    if let Err(e) = ctx.store.set_meta("quotas", &all.to_string()) {
        return fail(503, "unavailable", &e);
    }
    audit(ctx, "user.quota", actor, &format!("{name}: max_pcs {} -> {max_pcs}, max_running {} -> {max_running}", cur.0, cur.1));
    head::quotas_changed(ctx);
    json_out(200, json!({"user": user_json(ctx, &u)}))
}

fn template_json(t: (String, String, String, String, i64)) -> Value {
    json!({"name": t.0, "spec": serde_json::from_str::<Value>(&t.1).unwrap_or(Value::Null), "note": t.2, "owner": t.3, "created_ms": t.4})
}

/// POST /api/v1/templates {"name","spec","note"} (researcher): same name replaces; only the owner or an admin may.
fn post_template(ctx: &Ctx, r: &Req, who: &Who) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let name = str_field(&v, "name").unwrap_or("");
    if !valid_name(name) {
        return bad("name must be 1 to 64 letters, digits, '.', '_' or '-'");
    }
    let Some(spec) = v.get("spec").filter(|s| s.is_object()) else { return bad("spec must be a job body (see POST /jobs)") };
    let note: String = str_field(&v, "note").unwrap_or("").chars().take(500).collect();
    if let Err(m) = job_spec(spec, who) {
        return match m.strip_prefix("forbidden: ") { Some(m) => fail(403, "forbidden", m), None => bad(&format!("spec: {m}")) };
    }
    if ctx.store.template(name).is_some_and(|t| t.3 != who.name && who.role < Role::Admin) {
        return fail(403, "forbidden", "only the template's owner or an admin may replace it");
    }
    match ctx.store.put_template(name, &spec.to_string(), &note, &who.name, now_ms()) {
        Ok(()) => {
            audit(ctx, "template.saved", &who.name, name);
            crate::head::refresh(ctx); // replicate to standbys now (config entry)
            json_out(200, json!({"template": ctx.store.template(name).map(template_json)}))
        }
        Err(e) => fail(503, "unavailable", &e),
    }
}

fn delete_template(ctx: &Ctx, name: &str, who: &Who) -> Out {
    let Some(t) = ctx.store.template(name) else { return fail(404, "not_found", "no such template") };
    if t.3 != who.name && who.role < Role::Admin {
        return fail(403, "forbidden", "only the template's owner or an admin may delete it");
    }
    ctx.store.delete_template(name);
    audit(ctx, "template.deleted", &who.name, name);
    crate::head::refresh(ctx);
    json_out(200, json!({}))
}

/// POST /api/v1/nodes/identify {"nodes": [id], "secs": 30} (operator).
fn post_identify(ctx: &Ctx, r: &Req, actor: &str) -> Out {
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let secs = match v.get("secs") { None => 30, Some(x) => match uint(x).filter(|n| (1..=600).contains(n)) { Some(n) => n, None => return bad("secs must be a whole number from 1 to 600") } };
    let Some(ids) = v.get("nodes").and_then(Value::as_array).filter(|a| !a.is_empty() && a.len() <= 1000).and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<Vec<_>>>()) else { return bad("nodes must be a list of 1 to 1000 node ids") };
    if let Some(id) = ids.iter().find(|i| ctx.store.node(i).is_none()) {
        return fail(404, "not_found", &format!("no such node {id}"));
    }
    let (on, off) = head::identify(ctx, &ids, secs);
    let org = Org::load(ctx);
    for id in &on {
        if let Some(n) = ctx.store.node(id) {
            publish_node(ctx, &n, true, &org);
        }
    }
    audit(ctx, "node.identify", actor, &format!("{} PC(s) for {secs} s", on.len()));
    json_out(200, json!({"identified": on, "offline": off, "secs": secs}))
}

// ---- SSE `log` tap (wave 14). No hook in log.rs: while any viewer is connected, one shared poller reads what the
// logger appended to the files since the last poll (offsets per file), cuts a batch of <= 200 lines every 500 ms, and
// every stream sends the batches it has not seen. No viewer = nobody polls = no cost. The lines were scrubbed by log.rs.

struct Tail {
    off: HashMap<std::path::PathBuf, u64>,
    last: Option<Instant>,
    seq: u64,
    batches: std::collections::VecDeque<(u64, Arc<String>)>,
}

static TAIL: std::sync::Mutex<Option<Tail>> = std::sync::Mutex::new(None);

fn tail_poll(t: &mut Tail) {
    if t.last.is_some_and(|l| l.elapsed() < Duration::from_millis(450)) {
        return; // another stream polled just now
    }
    let stale = t.last.map_or(true, |l| l.elapsed() > Duration::from_secs(5)); // nobody watched: skip the backlog
    t.last = Some(Instant::now());
    let Ok(rd) = std::fs::read_dir(crate::log::logs_dir()) else { return };
    let mut lines: Vec<(i64, String)> = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let Ok(len) = e.metadata().map(|m| m.len()) else { continue };
        let off = t.off.entry(p.clone()).or_insert(if stale { len } else { 0 });
        if stale {
            *off = len;
            continue;
        }
        if len < *off {
            *off = 0; // truncated or replaced
        }
        if len == *off {
            continue;
        }
        let Ok(mut f) = std::fs::File::open(&p) else { continue };
        use std::io::{Seek, SeekFrom};
        let _ = f.seek(SeekFrom::Start(*off));
        let mut buf = vec![];
        let _ = f.take(len - *off).read_to_end(&mut buf);
        // only whole lines: a half-written one waits for the next poll
        let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        *off += end as u64;
        for l in String::from_utf8_lossy(&buf[..end]).lines().filter(|l| !l.is_empty()) {
            let ts = serde_json::from_str::<Value>(l).ok().and_then(|v| v["ts_ms"].as_i64()).unwrap_or(0);
            lines.push((ts, l.to_string()));
        }
    }
    t.off.retain(|p, _| p.exists());
    if lines.is_empty() {
        return;
    }
    lines.sort_by_key(|l| l.0);
    let skip = lines.len().saturating_sub(crate::tune::n("log_batch") as usize);
    let body = lines[skip..].iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join(",");
    t.seq += 1;
    let seq = t.seq;
    t.batches.push_back((seq, Arc::new(format!("{{\"lines\":[{body}]}}"))));
    while t.batches.len() > 8 {
        t.batches.pop_front();
    }
}

fn log_seq() -> u64 {
    TAIL.lock().unwrap().as_ref().map_or(0, |t| t.seq)
}

/// The next batch after `seen`, polling the files first (at most every ~500 ms across all streams).
fn log_batch_after(seen: u64) -> Option<(u64, Arc<String>)> {
    let mut g = TAIL.lock().unwrap();
    let t = g.get_or_insert_with(|| Tail { off: HashMap::new(), last: None, seq: 0, batches: Default::default() });
    tail_poll(t);
    t.batches.iter().find(|b| b.0 > seen).map(|b| (b.0, b.1.clone()))
}

/// Changes whenever the ranking of head candidates changes; recomputed at most every 5 s however many viewers.
fn cand_gen(ctx: &Ctx) -> u64 {
    static G: std::sync::Mutex<Option<(Instant, String, u64)>> = std::sync::Mutex::new(None);
    let mut g = G.lock().unwrap();
    if g.as_ref().is_some_and(|x| x.0.elapsed() < crate::tune::secs("cand_recheck_s")) {
        return g.as_ref().map_or(0, |x| x.2);
    }
    let sig = crate::head::candidates(ctx).iter().map(|c| format!("{}{}", c["node_id"].as_str().unwrap_or(""), c["eligible"])).collect::<Vec<_>>().join(",");
    let n = match g.take() {
        Some((_, old, n)) if old == sig => n,
        Some((_, _, n)) => n + 1,
        None => 1,
    };
    *g = Some((Instant::now(), sig, n));
    n
}

/// Blocking `Read` over the event bus: emits `cluster` every second, forwards bus messages, `: ping` every 15 s.
/// Returns EOF when the bus is gone; tiny_http drops it (ending the response) when the client hangs up.
pub struct Sse<F: FnMut() -> String> {
    rx: Receiver<(String, String)>,
    cluster: F,
    buf: Vec<u8>,
    pos: usize,
    next_cluster: Instant,
    next_ping: Instant,
    /// returns the `ha` event data when the role, epoch or standby set changed since the last call
    ha: Option<Box<dyn FnMut() -> Option<String> + Send>>,
    ha_due: Option<String>,
    /// every 500 ms: extra (event, data) frames (wave 14: `log` batches, `changed` candidates)
    extra: Option<Box<dyn FnMut() -> Vec<(&'static str, String)> + Send>>,
    extra_due: std::collections::VecDeque<(&'static str, String)>,
    next_extra: Instant,
}

impl<F: FnMut() -> String> Sse<F> {
    pub fn new(rx: Receiver<(String, String)>, cluster: F) -> Self {
        let now = Instant::now();
        Sse { rx, cluster, buf: vec![], pos: 0, next_cluster: now, next_ping: now + Duration::from_secs(15), ha: None, ha_due: None, extra: None, extra_due: Default::default(), next_extra: now + Duration::from_millis(500) }
    }
    fn with_ha(mut self, f: impl FnMut() -> Option<String> + Send + 'static) -> Self {
        self.ha = Some(Box::new(f));
        self
    }
    fn with_extra(mut self, f: impl FnMut() -> Vec<(&'static str, String)> + Send + 'static) -> Self {
        self.extra = Some(Box::new(f));
        self
    }
    fn frame(&mut self, event: &str, data: &str) {
        self.buf = format!("event: {event}\ndata: {data}\n\n").into_bytes();
        pad_chunk(&mut self.buf);
        self.pos = 0;
    }
}

/// tiny_http sends a streamed body through a chunked encoder that holds bytes until 8 KiB have collected, so a small
/// SSE frame would wait for the next big one (measured 0.2-2.4 s late). Pad every frame with an SSE comment to a whole
/// number of 8 KiB chunks so it leaves at once. ponytail: ~8 KiB per small event; an async server with explicit
/// flush would avoid the padding.
const SSE_CHUNK: usize = 8192;

fn pad_chunk(buf: &mut Vec<u8>) {
    let rem = buf.len() % SSE_CHUNK;
    if rem != 0 {
        let pad = SSE_CHUNK - rem;
        if pad < 4 {
            buf.resize(buf.len() + pad, b'\n'); // too small for ":\n\n"; blank lines are ignored by SSE parsers
        } else {
            buf.push(b':');
            buf.resize(buf.len() + pad - 3, b' ');
            buf.extend_from_slice(b"\n\n");
        }
    }
}

impl<F: FnMut() -> String> Read for Sse<F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            let now = Instant::now();
            if let Some(d) = self.ha_due.take() {
                self.frame("ha", &d);
            } else if let Some((e, d)) = self.extra_due.pop_front() {
                self.frame(e, &d);
            } else if now >= self.next_extra {
                self.next_extra = now + Duration::from_millis(500);
                if let Some(f) = self.extra.as_mut() {
                    self.extra_due.extend(f());
                }
            } else if now >= self.next_cluster {
                self.next_cluster = now + Duration::from_secs(1);
                let j = (self.cluster)();
                self.frame("cluster", &j);
                self.ha_due = self.ha.as_mut().and_then(|f| f());
            } else if now >= self.next_ping {
                self.next_ping = now + Duration::from_secs(15);
                self.buf = b": ping\n\n".to_vec();
                pad_chunk(&mut self.buf);
                self.pos = 0;
            } else {
                match self.rx.recv_timeout(self.next_cluster.min(self.next_ping).min(self.next_extra).saturating_duration_since(now)) {
                    Ok((ev, data)) => self.frame(&ev, &data),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Ok(0),
                }
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

/// %XX decoding of one path segment; bad escapes are kept as typed.
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// PUT /api/v1/blobs (streamed to disk) and GET/HEAD /api/v1/blobs/{sha} (streamed from disk). true = answered here;
/// Err(request) = not signed in (fall through so the usual 401/403 is produced).
fn blob_http(ctx: &Arc<Ctx>, auth: &Auth, mut rq: tiny_http::Request, path: &str, cookie: Option<&str>, bearer: Option<&str>) -> Result<(), tiny_http::Request> {
    let method = rq.method().as_str().to_string();
    // a job's own upload capability (env CK_BLOB_PUT_TOKEN on the PC): PUT only, only while that job is unfinished
    let job_tok = bearer.and_then(crate::blobs::check_put_token).filter(|j| method == "PUT" && ctx.store.job(*j).is_some_and(|r| !r.state.finished()));
    let who = match (job_tok, auth.who(&ctx.store, cookie, bearer).filter(|w| w.role >= Role::Researcher)) {
        (Some(j), _) => Who { name: format!("job-{j}"), role: Role::Researcher },
        (None, Some(w)) => w,
        _ => return Err(rq),
    };
    let json_resp = |status: u16, v: Value| Response::from_data(v.to_string().into_bytes()).with_status_code(status).with_header(header("Content-Type", "application/json"));
    if method == "PUT" {
        let max = (crate::tune::get("blob_max_mb") * 1048576.0) as u64;
        let res = crate::blobs::ingest(&mut rq.as_reader().take(max + 1), &crate::blobs::store_dir(), max);
        let resp = match res {
            Ok((sha, size, existed)) => {
                ctx.store.put_blob(&sha, size, now_ms());
                if !existed {
                    audit(ctx, "blob.put", &who.name, &format!("{sha} {size} bytes"));
                    crate::blobs::gc_store(ctx);
                }
                json_resp(200, json!({"sha256": sha, "size": size, "existed": existed}))
            }
            Err(e) => json_resp(400, json!({"error": {"code": "invalid_request", "message": format!("blob: {e}")}})),
        };
        let _ = rq.respond(resp);
        return Ok(());
    }
    let sha = path.trim_start_matches("/api/v1/blobs/");
    let file = crate::blobs::valid_sha(sha).then(|| crate::blobs::store_dir().join(sha)).and_then(|p| std::fs::File::open(p).ok().filter(|_| ctx.store.blob_size(sha).is_some()));
    let Some(f) = file else {
        let _ = rq.respond(json_resp(404, json!({"error": {"code": "not_found", "message": "no such blob"}})));
        return Ok(());
    };
    ctx.store.touch_blob(sha, now_ms());
    let len = f.metadata().map(|m| m.len() as usize).ok();
    let r = Response::new(tiny_http::StatusCode(200), vec![header("Content-Type", "application/octet-stream")], f, len, None);
    let _ = rq.respond(r);
    Ok(())
}

fn handle(ctx: &Arc<Ctx>, auth: &Auth, mut rq: tiny_http::Request) {
    let url = rq.url().to_string();
    let (path, q) = url.split_once('?').unwrap_or((&url, ""));
    let query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let hdr = |name: &str| rq.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string());
    let cookie = hdr("Cookie").and_then(|c| c.split(';').filter_map(|p| p.trim().strip_prefix("ck_session=")).next().map(String::from));
    let bearer = hdr("Authorization").and_then(|a| a.strip_prefix("Bearer ").map(|t| t.trim().to_string()));
    let ip = rq.remote_addr().map_or(IpAddr::from([0, 0, 0, 0]), |a| a.ip());
    if path != "/api/v1/wire" {
        crate::proto::wire::http(rq.method().as_str(), path);
    }
    if path.starts_with("/api/v1/blobs") && matches!(rq.method().as_str(), "PUT" | "GET" | "HEAD") && path != "/api/v1/blobs" || (path == "/api/v1/blobs" && rq.method().as_str() == "PUT") {
        match blob_http(ctx, auth, rq, path, cookie.as_deref(), bearer.as_deref()) {
            Ok(()) => return,
            Err(r) => rq = r,
        }
    }
    let mut body = vec![];
    if matches!(rq.method().as_str(), "POST" | "PUT" | "PATCH") {
        // only a signed-in operator may send a big list-job body; everyone else is held to 1 MiB before anything is read
        let signed = |min: Role| auth.who(&ctx.store, cookie.as_deref(), bearer.as_deref()).is_some_and(|w| w.role >= min);
        let max = if path == "/api/v1/jobs" && signed(Role::Operator) {
            mb("max_list_body_mb")
        } else if path == "/api/v1/experiments" && signed(Role::Researcher) {
            crate::research::EXPERIMENT_MAX + (1 << 20) // the two files plus JSON escaping headroom
        } else {
            mb("max_body_mb")
        };
        let _ = rq.as_reader().take(max as u64 + 1).read_to_end(&mut body);
    }
    let gz = rq.headers().iter().any(|h| h.field.equiv("Accept-Encoding") && accepts_gzip(h.value.as_str()));
    let req = Req { method: rq.method().as_str().to_string(), path: path.to_string(), query, cookie, bearer, body, ip, gz };
    let out = if req.body.len() > mb("max_body_mb") && !(req.method == "POST" && matches!(req.path.as_str(), "/api/v1/jobs" | "/api/v1/experiments")) { bad(&format!("body is over {} MB", crate::tune::get("max_body_mb"))) } else { route(ctx, auth, &req) };
    let _ = match out {
        Out::Plain { status, ctype, mut headers, mut body } => {
            // big JSON (nodes, cluster, candidates: ~0.7 MB per 1,000 PCs) goes compressed when the client takes gzip (~30-50x smaller)
            if gz && body.len() >= 16 << 10 && ctype.starts_with("application/json") && !headers.iter().any(|h| h.0 == "Content-Encoding") {
                body = crate::gz::gzip(&body);
                headers.push(("Content-Encoding", "gzip".into()));
                headers.push(("Vary", "Accept-Encoding".into()));
            }
            let mut r = Response::from_data(body).with_status_code(status).with_header(header("Content-Type", ctype));
            for (k, v) in headers {
                r.add_header(header(k, &v));
            }
            rq.respond(r)
        }
        Out::Sse => {
            if SSE_OPEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= SSE_MAX {
                SSE_OPEN.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let _ = rq.respond(Response::from_string("too many open event streams").with_status_code(503));
                return;
            }
            let c = ctx.clone();
            let (c2, mut last) = (ctx.clone(), String::new());
            let (c3, mut seen_log, mut seen_cand) = (ctx.clone(), log_seq(), cand_gen(ctx));
            let sse = Sse::new(ctx.bus.subscribe(), move || cluster_cached(&c).to_string()).with_extra(move || {
                let mut out = vec![];
                if let Some((seq, batch)) = log_batch_after(seen_log) {
                    seen_log = seq;
                    out.push(("log", batch.to_string()));
                }
                let g = cand_gen(&c3);
                if g != seen_cand {
                    seen_cand = g;
                    out.push(("changed", r#"{"what":"candidates"}"#.into()));
                }
                out
            }).with_ha(move || {
                let sig = ha_sig(&c2);
                (sig != last).then(|| {
                    last = sig;
                    ha_json(&c2).to_string()
                })
            });
            let mut r = Response::new(tiny_http::StatusCode(200), vec![], sse, None, None);
            for (k, v) in [("Content-Type", "text/event-stream"), ("Cache-Control", "no-cache"), ("X-Accel-Buffering", "no")] {
                r.add_header(header(k, v));
            }
            // an open stream lives for hours: it gets its own thread, so 32 browser tabs cannot use up the request pool
            // and leave every API call (and the console itself) waiting
            thread::spawn(move || {
                let _ = rq.respond(r);
                SSE_OPEN.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
            return
        }
    };
}

/// Long polls in flight (each holds a request thread); beyond the cap a wait is answered at once and the client backs off.
static LONG_OPEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const LONG_MAX: usize = 12;
const LONG_RESULT_MAX: usize = 1 << 20;

struct LongPoll;

impl LongPoll {
    fn enter() -> Option<LongPoll> {
        use std::sync::atomic::Ordering::SeqCst;
        if LONG_OPEN.fetch_add(1, SeqCst) >= LONG_MAX {
            LONG_OPEN.fetch_sub(1, SeqCst);
            return None;
        }
        Some(LongPoll)
    }
}

impl Drop for LongPoll {
    fn drop(&mut self) {
        LONG_OPEN.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

static SSE_OPEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const SSE_MAX: usize = 512; // ponytail: one thread per viewer; an async server would lift this

/// Blocks serving HTTP.
pub fn serve(ctx: Arc<Ctx>) {
    let server = Arc::new(Server::http(("0.0.0.0", dash_port())).unwrap_or_else(|e| crate::die(&format!("HTTP {}: {e}", dash_port()))));
    let auth = Arc::new(Auth::from_env());
    if auth.open {
        eprintln!("CK_AUTH=off: the API is open, everyone is an anonymous admin");
    }
    {
        let c = ctx.clone();
        thread::spawn(move || adv_loop(c));
    }
    let hs: Vec<_> = (0..POOL)
        .map(|_| {
            let (server, ctx, auth) = (server.clone(), ctx.clone(), auth.clone());
            thread::spawn(move || {
                while let Ok(rq) = server.recv() {
                    handle(&ctx, &auth, rq);
                }
            })
        })
        .collect();
    for h in hs {
        let _ = h.join();
    }
}

// ---- advisor (docs/ADVISOR.md): gathers the observation, applies actions through the normal endpoints, keeps Persist

use crate::advisor as adv;

#[derive(Default)]
struct AdvState {
    eng: adv::Engine,
    at: Option<Instant>,
    list: Vec<adv::Suggestion>,
    raw: Vec<adv::Suggestion>,
    sig: String,
}

/// One lock for the engine, the cache and the stored Persist (load-modify-save happens under it).
fn adv_state() -> std::sync::MutexGuard<'static, AdvState> {
    static S: std::sync::OnceLock<std::sync::Mutex<AdvState>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn adv_load(ctx: &Ctx) -> adv::Persist {
    ctx.store.meta("advisor").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn adv_save(ctx: &Ctx, p: &adv::Persist) {
    let _ = ctx.store.set_meta("advisor", &serde_json::to_string(p).unwrap_or_default());
}

fn adv_bd(b: &Value) -> adv::Bd {
    let f = |k: &str| b["pc_seconds"][k].as_f64().unwrap_or(0.0);
    adv::Bd {
        wall: b["wall_s"].as_f64().unwrap_or(0.0), compute: f("compute"), dispatch: f("dispatch_wait"), tail: f("tail_idle"), recompute: f("recompute"), failover: f("failover_pause"), other: f("other"),
        chunks: b["per_pc"].as_array().map_or(0, |a| a.iter().map(|p| p["chunks"].as_u64().unwrap_or(0)).sum()),
    }
}

/// (percent used, GB free) of the disk holding CK_DATA; None where `df` is missing.
fn adv_disk() -> Option<(f64, f64)> {
    let out = std::process::Command::new("df").arg("-Pk").arg(crate::blobs::data_dir()).output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).into_owned();
    let f: Vec<&str> = t.lines().nth(1)?.split_whitespace().collect();
    Some((f.get(4)?.trim_end_matches('%').parse().ok()?, f.get(3)?.parse::<f64>().ok()? / 1048576.0))
}

fn adv_gather(ctx: &Ctx) -> adv::AdvObs {
    let now = now_ms();
    let mut o = adv::AdvObs::default();
    for k in crate::tune::KNOBS {
        o.tun.insert(k.key.into(), crate::tune::get(k.key));
    }
    let (mut losses, mut before) = (HashMap::<String, u32>::new(), None);
    for _ in 0..20 {
        let page = ctx.store.events(500, before);
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        for e in page.iter().filter(|e| e.ts_ms >= now - 86_400_000) {
            match e.kind.as_str() {
                "node.lost" => {
                    if let Some(id) = e.detail.rsplit("(node ").next().and_then(|t| t.strip_suffix(')')) {
                        *losses.entry(id.to_string()).or_default() += 1;
                    }
                }
                "node.denied" | "task.timeout" | "labhead.leave" | "labhead.partition" if e.ts_ms >= now - 3_600_000 => *o.events.entry(e.kind.clone()).or_default() += 1,
                _ => {}
            }
        }
        if last.ts_ms < now - 86_400_000 {
            break;
        }
    }
    let live: HashMap<String, bool> = ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| Some((w.hello.as_ref()?.node_id.clone(), !w.inflight.is_empty()))).collect();
    o.nodes = ctx.store.nodes().into_iter().map(|n| adv::AdvNode { online: live.contains_key(&n.node_id), busy: live.get(&n.node_id).copied().unwrap_or(false), losses_24h: losses.get(&n.node_id).copied().unwrap_or(0), id: n.node_id, host: n.host, state: n.state }).collect();
    o.alerts = ctx.store.alerts(true, 200).into_iter().map(|a| (a.kind, a.node_id, a.detail)).collect();
    let running = head::running_jobs(ctx);
    for r in ctx.store.jobs(60, None) {
        let run = running.contains(&r.id);
        let recent = r.finished_ms.is_some_and(|t| t >= now - 3_600_000);
        let failed = r.state == crate::jobs::JobState::Failed && r.finished_ms.is_some_and(|t| t >= now - 86_400_000);
        if !(run || recent || failed) {
            continue;
        }
        let spec = ctx.store.spec(r.id);
        o.jobs.push(adv::AdvJob {
            running: run, user: r.submitter.clone(), wait: if run { head::wait_reason(ctx, r.id).map(String::from) } else { None }, failed, error: r.error.clone(),
            blobs: spec.as_ref().map_or(0, |s| s.blobs.len()), blob_mode: spec.map(|s| s.blob_mode).unwrap_or_default(), bd: if run || recent { head::breakdown(ctx, r.id).map(|b| adv_bd(&b)) } else { None },
        });
    }
    let q = crate::log::Query { role: None, since_ms: now - 3_600_000, kind: None, level: crate::log::Level::Warn, limit: 2000 };
    let mut warns: HashMap<String, (u32, String)> = HashMap::new();
    for l in crate::log::query(&crate::log::logs_dir(), &q) {
        if let Ok(v) = serde_json::from_str::<Value>(&l) {
            let msg = v["msg"].as_str().or(v["fields"]["msg"].as_str()).or(v["detail"].as_str()).unwrap_or("").chars().take(160).collect::<String>();
            let e = warns.entry(v["kind"].as_str().unwrap_or("").to_string()).or_default();
            *e = (e.0 + 1, msg);
        }
    }
    o.warns = warns.into_iter().map(|(k, (n, m))| (k, n, m)).collect();
    o.standbys = ctx.repl.status.lock().unwrap().standbys.len();
    o.target_standbys = ctx.store.meta("ha_settings").and_then(|s| serde_json::from_str::<crate::jobs::HaSettings>(&s).ok()).map_or(crate::tune::n("target_standbys") as usize, |h| h.target_standbys as usize);
    o.best_head = head::candidates(ctx).into_iter().find(|c| c["role"] == "candidate" && c["eligible"] == true).and_then(|c| Some((c["node_id"].as_str()?.to_string(), c["host"].as_str()?.to_string(), c["score"].as_f64()?)));
    o.disk = adv_disk();
    {
        let v = crate::hier::json(ctx);
        o.hier = adv::HierObs {
            mode: crate::hier::mode(), labs: crate::hier::advice_labs(&v),
            bad: v["labs"].as_array().into_iter().flatten().filter(|l| l["pinned"].is_string() && matches!(l["state"].as_str(), Some("partitioned" | "released" | "absent"))).map(|l| (l["lab"].as_str().unwrap_or("").to_string(), l["state"].as_str().unwrap_or("").to_string())).collect(),
            leaves_1h: o.events.get("labhead.leave").copied().unwrap_or(0), partitions_1h: o.events.get("labhead.partition").copied().unwrap_or(0),
        };
    }
    let pw = power_settings(ctx);
    (o.power_mode, o.keep_awake) = (pw.mode, pw.keep_awake);
    o.blob_sent_gb = crate::blobs::SERVED.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9;
    o
}

fn status_of(o: &Out) -> u16 {
    match o { Out::Plain { status, .. } => *status, Out::Sse => 200 }
}

/// Run an action through the same handler the matching endpoint uses.
fn adv_run(ctx: &Ctx, a: &adv::Action, actor: &str) -> Result<(), Out> {
    use adv::Action::*;
    let (method, path, body) = match a {
        Tune { key, to, .. } => ("PUT", "/api/v1/tunables", Value::Object([(key.clone(), json!(to))].into_iter().collect())),
        NodeState { node_ids, to, .. } => ("POST", "/api/v1/nodes/state", json!({"state": to, "node_ids": node_ids, "reason": "advisor"})),
        HaTarget { to, .. } => ("PUT", "/api/v1/ha/settings", json!({"target_standbys": to})),
        Power { to, .. } => ("PUT", "/api/v1/power", json!({"mode": to})),
        Doc { .. } => return Err(bad("this option has no one-click action; follow its steps")),
    };
    let req = Req { method: method.into(), path: path.into(), query: HashMap::new(), cookie: None, bearer: None, body: body.to_string().into_bytes(), ip: [127, 0, 0, 1].into(), gz: false };
    let out = match a { Tune { .. } => put_tunables(ctx, &req, actor), NodeState { .. } => set_states(ctx, &req, actor), HaTarget { .. } => put_ha_settings(ctx, &req, actor), _ => put_power(ctx, &req, actor) };
    if status_of(&out) < 300 { Ok(()) } else { Err(out) }
}

/// Apply `opt` of `s` and remember it for the follow-up check and for undo. The suggestion is snoozed for the follow-up window.
fn adv_do(ctx: &Ctx, p: &mut adv::Persist, s: &adv::Suggestion, opt: &adv::Opt, actor: &str, auto: bool, m: &HashMap<String, f64>) -> Result<adv::Applied, Out> {
    let now = now_ms();
    let a = opt.action.clone().filter(adv::Action::changes).ok_or_else(|| bad("this option has no one-click action; follow its steps"))?;
    if auto && !(opt.auto_safe && opt.reversible) {
        return Err(bad("not auto-safe"));
    }
    adv_run(ctx, &a, actor)?;
    let rec = adv::Applied {
        id: s.id.clone(), kind: s.kind.clone(), option: opt.id.clone(), action: a.clone(), by: actor.into(), auto, ts_ms: now, metric: opt.metric.clone(),
        before: opt.metric.as_ref().and_then(|k| m.get(k)).copied().unwrap_or(opt.metric_from), predicted: opt.metric_to, follow_s: adv::FOLLOW_S, undone: false, outcome: None,
    };
    audit(ctx, if auto { "advice.auto" } else { "advice.applied" }, actor, &format!("{} option {}: {}", s.id, opt.id, serde_json::to_string(&a).unwrap_or_default()));
    p.dismissed.insert(s.id.clone(), now + adv::FOLLOW_S * 1000);
    p.push(rec.clone());
    Ok(rec)
}

fn adv_refresh_in(ctx: &Ctx, st: &mut AdvState) {
    let now = now_ms();
    let o = adv_gather(ctx);
    let m = adv::metrics(&o);
    let mut p = adv_load(ctx);
    let before = serde_json::to_string(&p).unwrap_or_default();
    for a in adv::track(&mut p, now, &m) {
        let oc = a.outcome.as_ref().unwrap();
        audit(ctx, "advice.outcome", "advisor", &format!("{} option {}: predicted {} {:.3} -> {:.3}, observed {}, {}", a.id, a.option, a.metric.as_deref().unwrap_or("-"), a.before, a.predicted, oc.observed.map_or("n/a".into(), |x| format!("{x:.3}")), if oc.hit { "held" } else { "missed" }));
    }
    st.raw = adv::advise(&o, now);
    let list = st.eng.step(now, st.raw.clone(), &p);
    for s in &list {
        if let Some(opt) = adv::auto_pick(s, &p, now).cloned() {
            let _ = adv_do(ctx, &mut p, s, &opt, "advisor", true, &m);
        }
    }
    if serde_json::to_string(&p).unwrap_or_default() != before {
        adv_save(ctx, &p);
    }
    let list = st.eng.step(now, st.raw.clone(), &p); // auto-applied ones are snoozed now
    let sig = list.iter().map(|s| format!("{}:{}", s.id, s.severity)).collect::<Vec<_>>().join(",");
    if sig != st.sig {
        st.sig = sig;
        ctx.bus.publish("advice", &json!({"suggestions": list}).to_string());
    }
    st.list = list;
    st.at = Some(Instant::now());
}

fn adv_loop(ctx: Arc<Ctx>) {
    loop {
        thread::sleep(Duration::from_secs(10));
        if ctx.repl.status.lock().unwrap().role != "standby" {
            adv_refresh_in(&ctx, &mut adv_state());
        }
    }
}

fn adv_accuracy(p: &adv::Persist) -> Value {
    let mut kinds: Vec<&str> = p.applied.iter().map(|a| a.kind.as_str()).collect();
    kinds.sort();
    kinds.dedup();
    Value::Object(kinds.into_iter().filter_map(|k| Some((k.to_string(), json!(adv::accuracy(p, k)?)))).collect())
}

fn advice_get(ctx: &Ctx) -> Out {
    let mut st = adv_state();
    if st.at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
        adv_refresh_in(ctx, &mut st);
    }
    let p = adv_load(ctx);
    json_out(200, json!({"enabled": p.enabled, "policy": p.policy, "suggestions": st.list, "accuracy": adv_accuracy(&p)}))
}

fn advice_history(ctx: &Ctx) -> Out {
    let p = adv_load(ctx);
    json_out(200, json!({"history": p.applied, "accuracy": adv_accuracy(&p)}))
}

fn advice_settings(ctx: &Ctx, r: &Req, who: Option<&Who>, actor: &str) -> Out {
    if let Some(o) = adv_need(who, Role::Admin) { return o }
    let v = match parse_body(r) { Ok(v) => v, Err(o) => return o };
    let mut st = adv_state();
    let mut p = adv_load(ctx);
    if let Some(e) = v.get("enabled") {
        match e.as_bool() { Some(b) => p.enabled = b, None => return bad("enabled must be true or false") }
    }
    for (k, m) in v.get("policy").and_then(Value::as_object).into_iter().flatten() {
        match m {
            Value::Null => { p.policy.remove(k); }
            Value::String(s) if adv::MODES.contains(&s.as_str()) => { p.policy.insert(k.clone(), s.clone()); }
            _ => return bad("policy values are off, suggest, auto or null"),
        }
    }
    adv_save(ctx, &p);
    audit(ctx, "advice.settings", actor, &format!("enabled {}, policy {}", p.enabled, serde_json::to_string(&p.policy).unwrap_or_default()));
    adv_refresh_in(ctx, &mut st);
    json_out(200, json!({"enabled": p.enabled, "policy": p.policy, "suggestions": st.list}))
}

fn adv_need(who: Option<&Who>, min: Role) -> Option<Out> {
    if who.is_some_and(|w| w.role >= min) { None } else { Some(fail(403, "forbidden", &format!("needs the {} role", min.name()))) }
}

fn advice_post(ctx: &Ctx, r: &Req, who: Option<&Who>, id: &str, op: &str, actor: &str) -> Out {
    let need = if matches!(op, "apply" | "undo") { Role::Admin } else if matches!(op, "preview" | "dismiss") { Role::Operator } else { return fail(404, "not_found", "no such advice action") };
    if let Some(o) = adv_need(who, need) { return o }
    let body = serde_json::from_slice::<Value>(&r.body).ok().filter(Value::is_object).unwrap_or(json!({}));
    let mut st = adv_state();
    let mut p = adv_load(ctx);
    let now = now_ms();
    match op {
        "dismiss" => {
            let h = body.get("snooze_h").and_then(Value::as_f64).unwrap_or(24.0).clamp(0.0, 720.0);
            p.dismissed.insert(id.into(), now + (h * 3_600_000.0) as i64);
            adv_save(ctx, &p);
            audit(ctx, "advice.dismissed", actor, &format!("{id} for {h} h"));
        }
        "undo" => {
            let Some(rec) = p.applied.iter_mut().find(|a| a.id == id && !a.undone) else { return fail(404, "not_found", "nothing applied for this suggestion, or it was already undone") };
            let Some(inv) = rec.action.inverse() else { return bad("nothing to undo") };
            if let Err(o) = adv_run(ctx, &inv, actor) { return o }
            rec.undone = true;
            audit(ctx, "advice.undone", actor, &format!("{id} option {}: {}", rec.option, serde_json::to_string(&inv).unwrap_or_default()));
            p.dismissed.insert(id.into(), now + 3_600_000);
            adv_save(ctx, &p);
        }
        _ => {
            adv_refresh_in(ctx, &mut st); // re-check against fresh data
            let Some(s) = st.raw.iter().find(|s| s.id == id).cloned() else { return fail(409, "conflict", "this suggestion no longer applies") };
            let want = body.get("option").and_then(Value::as_str);
            let Some(opt) = s.options.iter().find(|o| want.map_or(o.recommended, |w| o.id == w)).cloned() else { return bad("no such option") };
            if op == "preview" {
                let pv = opt.action.as_ref().map(|a| adv::preview(a, |k| crate::tune::knob(k).map(|_| crate::tune::get(k))));
                return json_out(200, json!({"id": id, "option": opt, "preview": pv, "changes_nothing": true}));
            }
            if p.snoozed(id, now) && p.applied.iter().any(|a| a.id == id && !a.undone && now - a.ts_ms < adv::FOLLOW_S * 1000) {
                return fail(409, "conflict", "already applied; undo it first or wait for the follow-up window");
            }
            let m = adv::metrics(&adv_gather(ctx));
            let rec = match adv_do(ctx, &mut p, &s, &opt, actor, false, &m) { Ok(r) => r, Err(o) => return o };
            adv_save(ctx, &p);
            adv_refresh_in(ctx, &mut st);
            return json_out(200, json!({"applied": rec, "suggestions": st.list}));
        }
    }
    adv_refresh_in(ctx, &mut st);
    json_out(200, json!({"suggestions": st.list}))
}

/// `Accept-Encoding` lists gzip without `q=0` (which means "never gzip").
fn accepts_gzip(v: &str) -> bool {
    v.split(',').any(|e| {
        let mut p = e.split(';');
        p.next().is_some_and(|c| c.trim().eq_ignore_ascii_case("gzip")) && !p.any(|q| q.trim().strip_prefix("q=").and_then(|x| x.parse::<f32>().ok()) == Some(0.0))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_only_when_accepted() {
        assert!(accepts_gzip("gzip, deflate, br") && accepts_gzip("br;q=1, GZIP;q=0.5"));
        assert!(!accepts_gzip("gzip;q=0") && !accepts_gzip("identity, gzip; q=0.0") && !accepts_gzip("x-gzip") && !accepts_gzip(""));
    }
    use crate::jobs::Bus;
    use crate::store::Store;

    fn ctx() -> Arc<Ctx> {
        Arc::new(Ctx { sh: Default::default(), store: Arc::new(Store::memory()), bus: Bus::default(), repl: Default::default() })
    }

    fn req(m: &str, path: &str, body: &str) -> Req {
        Req { method: m.into(), path: path.into(), query: HashMap::new(), cookie: None, bearer: None, body: body.as_bytes().to_vec(), ip: [127, 0, 0, 1].into(), gz: false }
    }

    fn status(o: &Out) -> u16 {
        match o {
            Out::Plain { status, .. } => *status,
            Out::Sse => 200,
        }
    }

    #[test]
    fn body_validation() {
        let w = Who { name: "ann".into(), role: Role::Researcher };
        let ok = |s: &str| job_spec(&serde_json::from_str(s).unwrap(), &w);
        let s = ok(r#"{"kind":"sha","n":1e6,"sha":"00ab"}"#).unwrap();
        assert_eq!((s.kind, s.from, s.end, s.arg.clone(), s.submitter.as_str()), (KIND_SHA, 1, 1_000_001, vec![0, 0, 10, 11], "ann"));
        let s = ok(r#"{"kind":"synth","from":5,"n":10,"synth_ns":250}"#).unwrap();
        assert_eq!((s.end, s.arg), (15, 250u64.to_le_bytes().to_vec()));
        assert!(ok(r#"{"kind":"sha","n":0,"sha":"0"}"#).is_err());
        assert!(ok(r#"{"kind":"sha","n":1,"sha":"xyz"}"#).is_err());
        assert!(ok(&format!(r#"{{"kind":"sha","n":1,"sha":"{}"}}"#, "a".repeat(65))).is_err());
        assert!(ok(r#"{"kind":"sha","n":1,"sha":"0","cmd":"ls"}"#).is_err(), "extra field for another kind");
        assert!(ok(r#"{"kind":"synth","n":1}"#).is_err());
        assert!(ok(r#"{"kind":"nope","n":1}"#).is_err());
        assert!(ok(r#"{"kind":"synth","from":18446744073709551615,"n":2,"synth_ns":1}"#).is_err(), "overflow");
        assert!(ok(r#"{"kind":"cmd","n":1,"cmd":"ls"}"#).unwrap_err().starts_with("forbidden"), "researcher cannot submit cmd");
        let op = Who { name: "op".into(), role: Role::Operator };
        assert_eq!(job_spec(&serde_json::from_str(r#"{"kind":"cmd","n":1,"cmd":"ls"}"#).unwrap(), &op).unwrap().arg, b"ls");
    }

    #[test]
    fn auth_gates_routes() {
        let c = ctx();
        let a = Auth::new(false);
        c.store.create_user("v", &users::hash_password("viewerpassword", 100), "viewer", 1).unwrap();
        let tok = a.new_token(&c.store, "v").unwrap();
        assert_eq!(status(&route(&c, &a, &req("GET", "/api/v1/jobs", ""))), 401);
        let mut r = req("GET", "/api/v1/jobs", "");
        r.bearer = Some(tok.clone());
        assert_eq!(status(&route(&c, &a, &r)), 200);
        let mut r = req("POST", "/api/v1/jobs", r#"{"kind":"synth","n":1,"synth_ns":1}"#);
        r.bearer = Some(tok.clone());
        assert_eq!(status(&route(&c, &a, &r)), 403, "viewer cannot submit");
        let mut r = req("GET", "/api/v1/users", "");
        r.bearer = Some(tok);
        assert_eq!(status(&route(&c, &a, &r)), 403);
        assert_eq!(status(&route(&c, &Auth::new(true), &req("GET", "/api/v1/users", ""))), 200, "open mode");
        // bootstrap is refused once a user exists and from non-loopback addresses
        assert_eq!(status(&route(&c, &a, &req("POST", "/api/v1/bootstrap", r#"{"name":"x","password":"123456789012"}"#))), 409);
        let c2 = ctx();
        let mut r = req("POST", "/api/v1/bootstrap", r#"{"name":"x","password":"123456789012"}"#);
        r.ip = [10, 0, 0, 5].into();
        assert_eq!(status(&route(&c2, &a, &r)), 403);
    }

    #[test]
    fn every_route_has_a_cli_command() {
        for (m, p) in users::ROUTES {
            let hit = crate::jobcli::CLI_MAP.iter().find(|c| c.0 == *m && c.1 == *p);
            assert!(hit.is_some_and(|c| c.2.starts_with("ck ") || c.2.starts_with("no CLI needed: ")), "{m} {p} has no CLI command or console-only reason");
        }
        assert_eq!(crate::jobcli::CLI_MAP.len(), users::ROUTES.len(), "stale CLI_MAP entry");
    }

    #[test]
    fn wave14_contracts() {
        let (c, a) = (ctx(), Auth::new(false));
        let (vw, rs) = (tok(&c, &a, "vw", "viewer"), tok(&c, &a, "rs", "researcher"));
        let (_, v) = call(&c, &a, &vw, "GET", "/api/v1/ha", "");
        assert!(v["head"]["since_ms"].as_i64().unwrap() > 0 && v["head"]["addr"].as_str().unwrap().contains(':'), "{v}");
        let (s, v) = call(&c, &a, &vw, "GET", "/api/v1/power", "");
        assert_eq!((s, v["labs"].is_array()), (200, true));
        assert_eq!(call(&c, &a, &vw, "POST", "/api/v1/sim", "{}").0, 403);
        let (s, v) = call(&c, &a, &rs, "POST", "/api/v1/sim", r#"{"nodes":[{"count":4,"rate":1e5}],"job":{"n":1e6}}"#);
        assert_eq!((s, v["summary"]["finished"].as_bool()), (200, Some(true)), "{v}");
        assert_eq!(call(&c, &a, &rs, "POST", "/api/v1/sim", r#"{"nodes":[{"count":30000}]}"#).0, 400);
        let rx = c.bus.subscribe();
        let op = tok(&c, &a, "op", "operator");
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/reserves", r#"{"selector":"","keep_n":0,"keep_pct":10}"#).0, 200);
        assert_eq!(rx.try_iter().filter(|e| e.0 == "changed" && e.1.contains("reserves")).count(), 1);
        assert_eq!(changed_what("/api/v1/nodes/pc1/labels"), &["labels"]);
    }

    #[test]
    fn ha_status_and_standby_rule() {
        let (c, a) = (ctx(), Auth::new(false));
        let vw = tok(&c, &a, "vw", "viewer");
        {
            let mut st = c.repl.status.lock().unwrap();
            st.role = "primary".into();
            st.epoch = 3;
            st.standbys = vec![("s1".into(), 7, 2)];
        }
        let (code, v) = call(&c, &a, &vw, "GET", "/api/v1/ha", "");
        assert_eq!((code, v["role"].as_str(), v["epoch"].as_u64(), v["standbys"][0]["lag"].as_u64()), (200, Some("primary"), Some(3), Some(2)));
        assert!(v["durable_seq"].is_u64() && v["dispatching"].is_boolean());
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/jobs", "").0, 200);
        c.repl.status.lock().unwrap().role = "standby".into();
        let (code, v) = call(&c, &a, &vw, "GET", "/api/v1/jobs", "");
        assert_eq!((code, v["error"]["code"].as_str()), (503, Some("unavailable")));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/ha", "").0, 200);
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/me", "").0, 200);
        assert_eq!(status(&route(&c, &a, &req("GET", "/", ""))), 200, "console files still served");
        // the stream announces ha once, then only on change
        let mut last = String::new();
        let mut f = || {
            let sig = ha_sig(&c);
            (sig != last).then(|| {
                last = sig;
                ha_json(&c).to_string()
            })
        };
        assert!(f().is_some() && f().is_none());
        c.repl.status.lock().unwrap().epoch = 4;
        assert!(f().is_some());
    }

    #[test]
    fn config_export_import_round_trip() {
        let c = ctx();
        c.store.create_lab("A", 3, 3, 1).unwrap();
        let v = export_config(&c);
        let c2 = ctx();
        assert_eq!(import_config(&c2, &v, true).unwrap().len(), 1, "dry run lists the lab");
        assert!(c2.store.labs().is_empty());
        import_config(&c2, &v, false).unwrap();
        assert_eq!(export_config(&c2), v);
    }

    fn tok(c: &Arc<Ctx>, a: &Auth, name: &str, role: &str) -> String {
        c.store.create_user(name, &users::hash_password("longenoughpw1", 100), role, 1).unwrap();
        a.new_token(&c.store, name).unwrap()
    }

    fn call(c: &Arc<Ctx>, a: &Auth, t: &str, m: &str, p: &str, b: &str) -> (u16, Value) {
        let (p, q) = p.split_once('?').unwrap_or((p, ""));
        let mut r = req(m, p, b);
        r.query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        r.bearer = Some(t.into());
        match route(c, a, &r) {
            Out::Plain { status, body, .. } => (status, serde_json::from_slice(&body).unwrap_or(Value::Null)),
            Out::Sse => (200, Value::Null),
        }
    }

    #[test]
    fn fleet_routes() {
        let (c, a) = (ctx(), Auth::new(false));
        let (op, vw) = (tok(&c, &a, "op", "operator"), tok(&c, &a, "vw", "viewer"));
        c.store.upsert_node("n1", "h", "1.1.1.1", "{}", 1).unwrap();
        assert_eq!(call(&c, &a, &vw, "POST", "/api/v1/labs", r#"{"name":"A","w":3,"h":3}"#).0, 403);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/labs", r#"{"name":"A","w":65,"h":3}"#).0, 400);
        let (s, v) = call(&c, &a, &op, "POST", "/api/v1/labs", r#"{"name":"A","w":3,"h":3}"#);
        assert_eq!((s, v["lab"]["name"].as_str()), (200, Some("A")));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/labs", "").1["labs"].as_array().map(Vec::len), Some(1));
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/state", r#"{"node_ids":["n1"],"state":"bogus"}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/state", r#"{"node_ids":["zz"],"state":"active"}"#).0, 404);
        let (s, v) = call(&c, &a, &op, "POST", "/api/v1/nodes/state", r#"{"node_ids":["n1"],"state":"active","reason":"ok"}"#);
        assert_eq!((s, v["nodes"][0]["state"].as_str(), v["nodes"][0]["online"].as_bool()), (200, Some("active"), Some(false)));
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/place", r#"{"lab_id":1,"x":1,"y":1}"#).0, 200);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/place", r#"{"lab_id":1,"x":9,"y":1}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/place", r#"{"lab_id":null}"#).1["node"]["lab_id"], Value::Null);
        assert_eq!(pct_decode("abc%2Fv3"), "abc/v3");
        assert_eq!(pct_decode("50%"), "50%");
        let (s, v) = call(&c, &a, &op, "POST", "/api/v1/join-codes", r#"{"uses":2}"#);
        let code = v["code"].as_str().unwrap().to_string();
        assert_eq!((s, code.len(), v["uses"].as_u64()), (200, 14, Some(2)));
        assert_eq!(c.store.use_join_code(&users::sha256_hex(code.as_bytes()), now_ms()), Some(false), "stored hashed");
        assert!(c.store.events(50, None).iter().all(|e| !e.detail.contains(&code)), "code never audited");
        assert!(c.store.events(50, None).iter().any(|e| e.kind == "node.state"));
        assert_eq!(call(&c, &a, &op, "DELETE", "/api/v1/labs/1", "").0, 200);
        assert_eq!(call(&c, &a, &op, "DELETE", "/api/v1/labs/1", "").0, 404);
    }

    #[test]
    fn power_and_ha_settings_routes() {
        let (c, a) = (ctx(), Auth::new(false));
        let (ad, op, vw) = (tok(&c, &a, "ad", "admin"), tok(&c, &a, "op", "operator"), tok(&c, &a, "vw", "viewer"));
        assert_eq!(call(&c, &a, &vw, "PUT", "/api/v1/power", "{}").0, 403);
        assert_eq!(call(&c, &a, &vw, "POST", "/api/v1/nodes/power", "{}").0, 403);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/ha/settings", "{}").0, 403, "ha settings are admin only");
        let (s, v) = call(&c, &a, &vw, "GET", "/api/v1/power", "");
        assert_eq!((s, v["power"]["mode"].as_str(), v["asleep"].is_array()), (200, Some("off"), true));
        for bad in [r#"{"mode":"nap"}"#, r#"{"idle_min":0}"#, r#"{"idle_min":1441}"#, r#"{"keep_awake":10001}"#, r#"{"wake_on_demand":"y"}"#] {
            assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/power", bad).0, 400, "{bad}");
        }
        let (s, v) = call(&c, &a, &op, "PUT", "/api/v1/power", r#"{"mode":"eco","idle_min":2,"keep_awake":0}"#);
        assert_eq!((s, v["power"]["mode"].as_str(), v["power"]["idle_min"].as_u64(), v["power"]["wake_on_demand"].as_bool()), (200, Some("eco"), Some(2), Some(true)));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/ha/settings", "").1["settings"]["target_standbys"], 2);
        for bad in [r#"{"target_standbys":6}"#, r#"{"selector":"host=\"x"}"#] {
            assert_eq!(call(&c, &a, &ad, "PUT", "/api/v1/ha/settings", bad).0, 400, "{bad}");
        }
        let (s, v) = call(&c, &a, &ad, "PUT", "/api/v1/ha/settings", r#"{"target_standbys":1,"selector":"!virtual"}"#);
        assert_eq!((s, v["settings"]["target_standbys"].as_u64()), (200, Some(1)));
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/power", r#"{"node_ids":["x"],"action":"nap"}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/power", r#"{"node_ids":["ghost"],"action":"wake"}"#).0, 404);
    }

    #[test]
    fn templates_quota_identify_and_files_routes() {
        let (c, a) = (ctx(), Auth::new(false));
        let (ad, op, rs, rs2, vw) = (tok(&c, &a, "ad", "admin"), tok(&c, &a, "op", "operator"), tok(&c, &a, "rs", "researcher"), tok(&c, &a, "rs2", "researcher"), tok(&c, &a, "vw", "viewer"));
        // templates: researcher saves, viewer only reads, the owner or an admin replaces
        let body = r#"{"name":"t1","note":"n","spec":{"kind":"synth","n":1000,"synth_ns":1,"name":"from t1"}}"#;
        assert_eq!(call(&c, &a, &vw, "POST", "/api/v1/templates", body).0, 403);
        assert_eq!(call(&c, &a, &rs, "POST", "/api/v1/templates", body).0, 200);
        assert_eq!(call(&c, &a, &rs, "POST", "/api/v1/templates", r#"{"name":"bad name","spec":{}}"#).0, 400);
        assert_eq!(call(&c, &a, &rs, "POST", "/api/v1/templates", r#"{"name":"t2","spec":{"kind":"cmd","n":1,"cmd":"x"}}"#).0, 403, "researchers cannot save cmd jobs");
        assert_eq!(call(&c, &a, &rs2, "POST", "/api/v1/templates", body).0, 403, "not the owner");
        assert_eq!(call(&c, &a, &ad, "POST", "/api/v1/templates", body).0, 200, "admin may replace");
        let (s, v) = call(&c, &a, &vw, "GET", "/api/v1/templates", "");
        assert_eq!((s, v["templates"][0]["name"].as_str(), v["templates"][0]["owner"].as_str(), v["templates"][0]["spec"]["n"].as_u64()), (200, Some("t1"), Some("rs"), Some(1000)));
        // run it with an override: n from the body, the rest from the spec
        let (s, v) = call(&c, &a, &rs2, "POST", "/api/v1/jobs", r#"{"template":"t1","n":777}"#);
        assert_eq!((s, v["job"]["total"].as_u64(), v["job"]["name"].as_str()), (200, Some(777), Some("from t1")), "{v}");
        assert_eq!(call(&c, &a, &rs2, "POST", "/api/v1/jobs", r#"{"template":"nope"}"#).0, 404);
        assert_eq!(call(&c, &a, &rs2, "DELETE", "/api/v1/templates/t1", "").0, 403);
        assert_eq!(call(&c, &a, &rs, "DELETE", "/api/v1/templates/t1", "").0, 200);
        assert_eq!(call(&c, &a, &rs, "DELETE", "/api/v1/templates/t1", "").0, 404);
        // quota: admin only, shown on the user, 0/0 removes it
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/users/rs/quota", r#"{"max_pcs":2}"#).0, 403);
        assert_eq!(call(&c, &a, &ad, "PUT", "/api/v1/users/rs/quota", r#"{"max_pcs":-1}"#).0, 400);
        assert_eq!(call(&c, &a, &ad, "PUT", "/api/v1/users/ghost/quota", r#"{"max_pcs":2}"#).0, 404);
        let (s, v) = call(&c, &a, &ad, "PUT", "/api/v1/users/rs/quota", r#"{"max_pcs":2,"max_running":3}"#);
        assert_eq!((s, v["user"]["quota"]["max_pcs"].as_u64(), v["user"]["quota"]["max_running"].as_u64()), (200, Some(2), Some(3)));
        let (_, v) = call(&c, &a, &ad, "GET", "/api/v1/users", "");
        let q = |n: &str| v["users"].as_array().unwrap().iter().find(|u| u["name"] == n).unwrap()["quota"].clone();
        assert_eq!((q("rs")["max_pcs"].as_u64(), q("op")["max_running"].as_u64()), (Some(2), Some(0)));
        call(&c, &a, &ad, "PUT", "/api/v1/users/rs/quota", r#"{"max_pcs":0,"max_running":0}"#);
        assert!(c.store.meta("quotas").is_some_and(|m| m == "{}"));
        // identify: operator, secs 1..600, known nodes only
        assert_eq!(call(&c, &a, &rs, "POST", "/api/v1/nodes/identify", r#"{"nodes":["n1"]}"#).0, 403);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/identify", r#"{"nodes":["n1"],"secs":601}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/identify", r#"{"nodes":[]}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/nodes/identify", r#"{"nodes":["ghost"]}"#).0, 404);
        // files: listed per job, with the job's file count in its JSON
        let (_, v) = call(&c, &a, &rs, "POST", "/api/v1/jobs", r#"{"kind":"synth","n":10,"synth_ns":1}"#);
        let id = v["job"]["id"].as_u64().unwrap();
        c.store.put_job_files(id, 0, 5, &[("a.txt".into(), "ab".repeat(32), 3), ("b.txt".into(), "cd".repeat(32), 4)]);
        let (s, v) = call(&c, &a, &vw, "GET", &format!("/api/v1/jobs/{id}/files"), "");
        assert_eq!((s, v["files"].as_array().map(Vec::len), v["files"][1]["name"].as_str(), v["files"][1]["chunk"][1].as_u64()), (200, Some(2), Some("b.txt"), Some(5)));
        assert_eq!(call(&c, &a, &vw, "GET", &format!("/api/v1/jobs/{id}"), "").1["job"]["files"].as_u64(), Some(2));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/jobs/99999/files", "").0, 404);
    }

    #[test]
    fn tunables_routes() {
        let (c, a) = (ctx(), Auth::new(false));
        let (ad, op, vw) = (tok(&c, &a, "ad", "admin"), tok(&c, &a, "op", "operator"), tok(&c, &a, "vw", "viewer"));
        assert_eq!(call(&c, &a, &vw, "PUT", "/api/v1/tunables", "{}").0, 403);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/tunables", "{}").0, 403);
        let (s, v) = call(&c, &a, &vw, "GET", "/api/v1/tunables", "");
        assert_eq!((s, v["tunables"][0]["key"].as_str()), (200, Some("heartbeat_s")));
        for bad in [r#"{"nope":1}"#, r#"{"ring_max":1}"#, r#"{"ring_max":"x"}"#, r#"{"ring_max":5000,"nope":1}"#] {
            assert_eq!(call(&c, &a, &ad, "PUT", "/api/v1/tunables", bad).0, 400, "{bad}");
        }
        let val = |v: &Value| v["tunables"].as_array().unwrap().iter().find(|t| t["key"] == "ring_max").map(|t| (t["value"].as_f64().unwrap(), t["source"].as_str().unwrap().to_string())).unwrap();
        assert_eq!(val(&call(&c, &a, &vw, "GET", "/api/v1/tunables", "").1).0, 100_000.0, "all or nothing");
        let (s, v) = call(&c, &a, &ad, "PUT", "/api/v1/tunables", r#"{"ring_max":5000}"#);
        assert_eq!((s, val(&v)), (200, (5000.0, "db".to_string())));
        assert!(c.store.meta("tune").unwrap().contains("ring_max"));
        let (s, v) = call(&c, &a, &ad, "PUT", "/api/v1/tunables", r#"{"ring_max":null}"#);
        assert_eq!((s, val(&v)), (200, (100_000.0, "default".to_string())));
        assert!(c.store.events(10, None).iter().any(|e| e.kind == "tune.changed"));
    }

    #[test]
    fn logs_route_is_operator_only() {
        let (c, a) = (ctx(), Auth::new(false));
        let (op, vw) = (tok(&c, &a, "op", "operator"), tok(&c, &a, "vw", "viewer"));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/logs", "").0, 403);
        let (s, v) = call(&c, &a, &op, "GET", "/api/v1/logs", "");
        assert_eq!((s, v["logs"].is_array()), (200, true));
        assert_eq!(call(&c, &a, &op, "GET", "/api/v1/logs?level=loud", "").0, 400);
    }

    #[test]
    fn organisation_routes() {
        let (c, a) = (ctx(), Auth::new(false));
        let (ad, op, vw) = (tok(&c, &a, "ad", "admin"), tok(&c, &a, "op", "operator"), tok(&c, &a, "vw", "viewer"));
        for id in ["n1", "n2"] {
            c.store.upsert_node(id, "h", "1.1.1.1", "{}", 1).unwrap();
        }
        // roles
        assert_eq!(call(&c, &a, &vw, "PUT", "/api/v1/nodes/n1/labels", r#"{"a":"b"}"#).0, 403);
        assert_eq!(call(&c, &a, &vw, "POST", "/api/v1/caps", "{}").0, 403);
        assert_eq!(call(&c, &a, &op, "GET", "/api/v1/config", "").0, 403, "config is admin only");
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/pools", "").0, 200);
        // labels
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/nodes/zz/labels", r#"{"a":"b"}"#).0, 404);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/labels", r#"{"a":1}"#).0, 400);
        let (s, v) = call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/labels", r#"{"room":"x1"}"#);
        assert_eq!((s, v["node"]["labels"]["room"].as_str(), v["node"]["pools"].is_array(), v["node"]["class"].is_string()), (200, Some("x1"), true, true));
        // pools
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/pools/night", r#"{"members":"n1"}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "PUT", "/api/v1/pools/night", r#"{"members":["ghost"]}"#).0, 400);
        let (s, v) = call(&c, &a, &op, "PUT", "/api/v1/pools/night", r#"{"members":["n1","n2"],"note":"x"}"#);
        assert_eq!((s, v["pool"]["members"].as_array().map(Vec::len)), (200, Some(2)));
        assert_eq!(call(&c, &a, &vw, "GET", "/api/v1/nodes/n2", "").1["node"]["pools"][0], "night");
        // select preview
        let mut r = req("GET", "/api/v1/select", "");
        r.query.insert("q".into(), "pool%3Dnight".into());
        r.bearer = Some(vw.clone());
        let Out::Plain { body, .. } = route(&c, &a, &r) else { panic!() };
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!((v["count"].as_u64(), v["error"].is_null()), (Some(2), true));
        r.query.insert("q".into(), "cores>>8".into());
        let Out::Plain { body, .. } = route(&c, &a, &r) else { panic!() };
        assert!(serde_json::from_slice::<Value>(&body).unwrap()["error"].is_string());
        // caps and reserves
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/caps", r#"{"selector":"cores>>8","max_threads":4}"#).0, 400);
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/caps", r#"{"selector":"pool=night","max_threads":0}"#).0, 400);
        let (s, v) = call(&c, &a, &op, "POST", "/api/v1/caps", r#"{"selector":"pool=night","max_threads":4}"#);
        assert_eq!((s, v["cap"]["max_threads"].as_u64()), (200, Some(4)));
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/reserves", r#"{"selector":"pool=night"}"#).0, 400, "needs keep_n or keep_pct");
        assert_eq!(call(&c, &a, &op, "POST", "/api/v1/reserves", r#"{"selector":"pool=night","keep_pct":101}"#).0, 400);
        let (s, v) = call(&c, &a, &op, "POST", "/api/v1/reserves", r#"{"selector":"pool=night","keep_n":1}"#);
        assert_eq!((s, v["reserve"]["keep_n"].as_u64()), (200, Some(1)));
        assert!(call(&c, &a, &vw, "GET", "/api/v1/reserves", "").1["reserves"][0]["held"].is_array());
        assert_eq!(call(&c, &a, &op, "DELETE", "/api/v1/caps/99", "").0, 404);
        // jobs: selector + priority
        let w = Who { name: "ann".into(), role: Role::Researcher };
        let ok = |s: &str| job_spec(&serde_json::from_str(s).unwrap(), &w);
        let sp = ok(r#"{"kind":"synth","n":1,"synth_ns":1,"selector":"lab=A","priority":"urgent"}"#).unwrap();
        assert_eq!((sp.selector.as_str(), sp.priority), ("lab=A", 0));
        assert_eq!(ok(r#"{"kind":"synth","n":1,"synth_ns":1}"#).unwrap().priority, 2);
        assert!(ok(r#"{"kind":"synth","n":1,"synth_ns":1,"selector":"a>>b"}"#).is_err());
        assert!(ok(r#"{"kind":"synth","n":1,"synth_ns":1,"priority":"asap"}"#).is_err());
        // config round trip
        call(&c, &a, &op, "POST", "/api/v1/labs", r#"{"name":"A","w":3,"h":3}"#);
        call(&c, &a, &op, "PUT", "/api/v1/nodes/n1/place", r#"{"lab_id":1,"x":1,"y":1}"#);
        let (s, exp) = call(&c, &a, &ad, "GET", "/api/v1/config", "");
        assert_eq!((s, exp["version"].as_u64()), (200, Some(1)));
        let c2 = ctx();
        for id in ["n1", "n2"] {
            c2.store.upsert_node(id, "h", "1.1.1.1", "{}", 1).unwrap();
        }
        let a2 = Auth::new(false);
        let ad2 = tok(&c2, &a2, "ad", "admin");
        let (s, v) = call(&c2, &a2, &ad2, "POST", "/api/v1/config?dry_run=1", &exp.to_string());
        let n = v["changes"].as_array().map(Vec::len).unwrap_or(0);
        assert!(s == 200 && n >= 6, "{v}");
        assert!(call(&c2, &a2, &ad2, "GET", "/api/v1/config", "").1["labs"].as_array().unwrap().is_empty(), "dry run wrote nothing");
        assert_eq!(call(&c2, &a2, &ad2, "POST", "/api/v1/config", &exp.to_string()).0, 200);
        assert_eq!(call(&c2, &a2, &ad2, "GET", "/api/v1/config", "").1, exp);
        assert_eq!(call(&c2, &a2, &ad2, "POST", "/api/v1/config", &exp.to_string()).1["changes"].as_array().map(Vec::len), Some(0), "idempotent");
        assert_eq!(call(&c2, &a2, &ad2, "POST", "/api/v1/config", r#"{"version":2}"#).0, 400);
        assert!(c2.store.events(50, None).iter().any(|e| e.kind == "config.applied"));
    }

    #[test]
    fn unknown_paths() {
        let c = ctx();
        let a = Auth::new(true);
        assert_eq!(status(&route(&c, &a, &req("GET", "/api/v1/nope", ""))), 404);
        match route(&c, &a, &req("GET", "/some/spa/route", "")) {
            Out::Plain { status: 200, ctype, .. } => assert!(ctype.starts_with("text/html")),
            _ => panic!("console fallback"),
        }
    }

    #[test]
    fn list_job_validation() {
        let (op, res) = (Who { name: "op".into(), role: Role::Operator }, Who { name: "r".into(), role: Role::Researcher });
        let j = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let sp = job_spec(&j(r#"{"kind":"list","cmd":"wc -c","items":["a","bb"],"reducer":"sum","policy":"fixed"}"#), &op).unwrap();
        assert_eq!((sp.kind, sp.from, sp.end, sp.items.len(), sp.arg.as_slice(), sp.reducer.as_str(), sp.policy.as_str()), (KIND_LIST, 0, 2, 2, &b"wc -c"[..], "sum", "fixed"));
        assert!(job_spec(&j(r#"{"kind":"list","cmd":"x","items":["a"]}"#), &res).unwrap_err().starts_with("forbidden: "));
        for bad in [r#"{"kind":"list","cmd":"x","items":[]}"#, r#"{"kind":"list","items":["a"]}"#, r#"{"kind":"list","cmd":"x"}"#, r#"{"kind":"list","cmd":"x","items":[1]}"#, r#"{"kind":"list","cmd":"x","sha":"ab","items":["a"]}"#] {
            assert!(job_spec(&j(bad), &op).is_err(), "{bad}");
        }
        let long = format!(r#"{{"kind":"list","cmd":"x","items":["{}"]}}"#, "a".repeat(4097));
        assert!(job_spec(&j(&long), &op).is_err());
        assert!(job_spec(&j(r#"{"kind":"synth","n":1,"synth_ns":1,"reducer":"top:0"}"#), &op).is_err());
        assert!(job_spec(&j(r#"{"kind":"synth","n":1,"synth_ns":1,"policy":"zzz"}"#), &op).is_err());
        assert_eq!(job_spec(&j(r#"{"kind":"synth","n":1,"synth_ns":1,"reducer":"top:5"}"#), &op).unwrap().reducer, "top:5");
    }

    #[test]
    fn windows_and_reservations_crud() {
        let (c, a) = (ctx(), Auth::new(false));
        let (v, o) = (tok(&c, &a, "vv", "viewer"), tok(&c, &a, "oo", "operator"));
        let w = r#"{"selector":"","days":"mon-sun","start":"00:00","end":"23:59","action":"block","note":"all day"}"#;
        assert_eq!(call(&c, &a, &v, "POST", "/api/v1/windows", w).0, 403);
        let (s, b) = call(&c, &a, &o, "POST", "/api/v1/windows", w);
        assert_eq!(s, 200, "{b}");
        let id = b["window"]["id"].as_u64().unwrap();
        assert_eq!(call(&c, &a, &o, "POST", "/api/v1/windows", r#"{"selector":"","days":"xx","start":"00:00","end":"01:00","action":"block"}"#).0, 400);
        assert_eq!(call(&c, &a, &o, "POST", "/api/v1/windows", r#"{"selector":"","days":"mon","start":"00:00","end":"01:00","action":"cap"}"#).0, 400, "cap needs threads");
        assert_eq!(call(&c, &a, &o, "POST", "/api/v1/windows", r#"{"selector":"lab=","days":"mon","start":"00:00","end":"01:00","action":"block"}"#).0, 400, "bad selector");
        let (s, b) = call(&c, &a, &v, "GET", "/api/v1/windows", "");
        assert_eq!((s, b["windows"].as_array().unwrap().len(), b["windows"][0]["id"].as_u64()), (200, 1, Some(id)));
        assert!(b["windows"][0]["active_now"].is_boolean());
        let now = now_ms();
        let rv = format!(r#"{{"selector":"","start_ms":{},"end_ms":{},"note":"x"}}"#, now - 1000, now + 3_600_000);
        let (s, b) = call(&c, &a, &o, "POST", "/api/v1/reservations", &rv);
        assert_eq!((s, b["reservation"]["active_now"].as_bool()), (200, Some(true)), "{b}");
        let rid = b["reservation"]["id"].as_u64().unwrap();
        assert_eq!(call(&c, &a, &o, "POST", "/api/v1/reservations", &format!(r#"{{"selector":"","start_ms":{now},"end_ms":{now}}}"#)).0, 400);
        assert_eq!(call(&c, &a, &v, "DELETE", &format!("/api/v1/reservations/{rid}"), "").0, 403);
        assert_eq!(call(&c, &a, &v, "GET", "/api/v1/reservations", "").1["reservations"].as_array().unwrap().len(), 1);
        assert_eq!(call(&c, &a, &o, "DELETE", &format!("/api/v1/reservations/{rid}"), "").0, 200);
        assert_eq!(call(&c, &a, &o, "DELETE", &format!("/api/v1/windows/{id}"), "").0, 200);
        assert_eq!(call(&c, &a, &o, "DELETE", &format!("/api/v1/windows/{id}"), "").0, 404);
        let kinds: Vec<String> = c.store.events(20, None).into_iter().map(|e| e.kind).collect();
        for k in ["window.added", "window.deleted", "reservation.added", "reservation.deleted"] {
            assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
        }
    }

    #[test]
    fn estimate_shape() {
        let (c, a) = (ctx(), Auth::new(true));
        let r = route(&c, &a, &Req { query: [("n".to_string(), "1000".to_string())].into(), ..req("GET", "/api/v1/estimate", "") });
        let Out::Plain { status: 200, body, .. } = r else { panic!("estimate") };
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v["matching_pcs"].is_u64() && v["fleet_rate"].is_f64() && (v["eta_secs"].is_null() || v["eta_secs"].is_f64()), "{v}");
        assert_eq!(status(&route(&c, &a, &req("GET", "/api/v1/estimate", ""))), 400, "n is required");
    }

    /// The pre-reducer result_text, kept as the reference for parity.
    fn old_result_text(parts: &[(u64, u64, Vec<u8>)], kind: &str) -> Vec<u8> {
        match kind {
            "sha" => {
                let mut hits: Vec<(u64, &[u8])> = parts.iter().flat_map(|p| p.2.chunks_exact(40)).map(|r| (u64::from_le_bytes(r[..8].try_into().unwrap()), &r[8..])).collect();
                hits.sort_unstable();
                hits.iter().map(|(n, d)| format!("{n} {}\n", users::hex(d))).collect::<String>().into_bytes()
            }
            "synth" => format!("{}\n", parts.iter().map(|p| u64::from_le_bytes(p.2.get(..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 8]))).sum::<u64>()).into_bytes(),
            _ => parts.iter().flat_map(|p| p.2.clone()).collect(),
        }
    }

    #[test]
    fn result_via_reducers_matches_old_text() {
        let sha = |n: u64| [n.to_le_bytes().to_vec(), vec![n as u8; 32]].concat();
        let parts = vec![(0, 10, [sha(7), sha(3)].concat()), (10, 20, sha(15)), (20, 30, vec![])];
        let synth = vec![(0, 5, 5u64.to_le_bytes().to_vec()), (5, 9, 4u64.to_le_bytes().to_vec())];
        let cmd = vec![(0, 5, b"a\nb\n".to_vec()), (5, 9, b"c\n".to_vec())];
        for (kind, p) in [("sha", &parts), ("synth", &synth), ("cmd", &cmd)] {
            assert_eq!(reduce::reduce(kind, "", p).unwrap(), old_result_text(p, kind), "{kind}");
        }
        // and through the route, with ?reducer=
        let (c, a) = (ctx(), Auth::new(true));
        let spec = JobSpec { name: "t".into(), kind: KIND_CMD, from: 0, end: 4, arg: b"x".to_vec(), submitter: "u".into(), ..Default::default() };
        let id = c.store.create_job(&spec, 1).unwrap().id;
        c.store.add_part(id, 0, 2, b"1\n2\n").unwrap();
        c.store.add_part(id, 2, 4, b"3\n").unwrap();
        let body = |q: &str| match route(&c, &a, &Req { query: q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(), ..req("GET", &format!("/api/v1/jobs/{id}/result"), "") }) {
            Out::Plain { status, body, .. } => (status, String::from_utf8(body).unwrap()),
            _ => panic!(),
        };
        assert_eq!(body(""), (200, "1\n2\n3\n".into()));
        let _ = body("reducer=sum"); // value checked in reduce::tests; here only that the route accepts it
        assert_eq!(body("reducer=nope").0, 400);
    }

    #[test]
    fn sse_ends_on_hangup() {
        let bus = Bus::default();
        let mut s = Sse::new(bus.subscribe(), || "{}".into());
        bus.publish("job", "{\"id\":1}");
        // frames are padded to whole 8 KiB chunks (pad_chunk) so each leaves tiny_http at once
        let mut b = vec![0u8; SSE_CHUNK];
        let mut read_frame = |s: &mut Sse<_>| {
            let mut f = vec![];
            while f.len() < SSE_CHUNK {
                let n = s.read(&mut b[..SSE_CHUNK - f.len()]).unwrap();
                f.extend_from_slice(&b[..n]);
            }
            String::from_utf8(f).unwrap()
        };
        let got = read_frame(&mut s);
        assert!(got.starts_with("event: cluster\ndata: {}") && got.len() % SSE_CHUNK == 0, "{}", &got[..40]);
        assert!(read_frame(&mut s).starts_with("event: job"));
        drop(bus);
        let mut rest = vec![];
        let _ = s.read_to_end(&mut rest); // cluster ticks may precede EOF
    }
}
