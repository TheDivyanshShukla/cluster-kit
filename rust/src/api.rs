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

pub fn cluster_json(ctx: &Ctx) -> Value {
    let workers = {
        let st = ctx.sh.0.lock().unwrap();
        let mut ws: Vec<Value> = st.workers.iter().map(|(id, w)| dash::worker_json(*id, w)).collect();
        ws.sort_by_key(|w| w["id"].as_u64());
        ws
    };
    let jobs = running_jobs(ctx);
    json!({"now_ms": now_ms(), "workers": workers, "job": jobs.first(), "jobs": jobs})
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
    Ok(JobSpec { max_pcs, placement, mem_mb, name, kind: k, from, end, chunk, arg, only, submitter: who.name.clone(), selector, priority, policy, reducer, items })
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
        let lvl = if r.method != "GET" { crate::log::Level::Info } else if status >= 400 { crate::log::Level::Warn } else { crate::log::Level::Debug };
        if crate::log::enabled(lvl) {
            let user = USER.with(|u| u.borrow().clone());
            crate::log::emit(lvl, "api.request", json!({"method": r.method, "path": r.path, "status": status, "ms": t.elapsed().as_secs_f64() * 1000.0, "user": user, "ip": r.ip.to_string()}));
        }
    }
    out
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
                Out::Plain { status: 200, ctype: f.1, headers: vec![("Cache-Control", cache.into())], body: f.2.to_vec() }
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
        ("GET", ["users"]) => json_out(200, json!({"users": ctx.store.users()})),
        ("POST", ["users"]) => create_user(ctx, r, me),
        ("GET", ["cluster"]) => json_out(200, cluster_json(ctx)),
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
            let v = match parse_body(r) {
                Ok(v) => v,
                Err(o) => return o,
            };
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
                ("GET", None) => json_out(200, json!({"job": job_json(ctx, &etas(ctx, vec![job.clone()]).remove(0)), "parts": ctx.store.parts(job.id).len()})),
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
        _ => not_found(),
    }
}

// ---- fleet (API.md "Fleet")

fn online_ids(ctx: &Ctx) -> std::collections::HashSet<String> {
    ctx.sh.0.lock().unwrap().workers.values().filter_map(|w| w.hello.as_ref().map(|h| h.node_id.clone())).collect()
}

/// Labels, pools and lab names of every node, loaded once per request (ORG-02..04).
pub struct Org {
    labels: HashMap<String, Vec<(String, String)>>,
    pools: HashMap<String, Vec<String>>,
    labs: HashMap<u64, String>,
    presence: HashMap<String, String>,
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
        Org { labels, pools, labs: ctx.store.labs().into_iter().map(|l| (l.id, l.name)).collect(), presence }
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
            Some(sched::NodeView { wid: *wid, node_id: id.clone(), attrs: org.attrs(&n, Some((w.threads, w.cpu))), takes_tasks: takes_tasks(&w.state), job: w.inflight.first().map(|f| f.0), inflight: w.inflight.len() })
        })
        .collect();
    v.sort_by_key(|n| n.wid);
    v
}

fn node_json(ctx: &Ctx, n: &NodeRow, online: bool, org: &Org) -> Value {
    let mut v = serde_json::to_value(n).unwrap_or_default();
    v["inventory"] = serde_json::from_str(&n.inventory).unwrap_or(json!({}));
    v["online"] = json!(online);
    v["presence"] = json!(org.presence.get(&n.node_id).map_or("idle", String::as_str));
    v["labels"] = json!(org.labels.get(&n.node_id).map(|l| l.iter().cloned().collect::<std::collections::BTreeMap<_, _>>()).unwrap_or_default());
    v["pools"] = json!(org.pools.get(&n.node_id).cloned().unwrap_or_default());
    v["class"] = json!(select::class_of(&n.inventory));
    v["power"] = json!(crate::head::power_state(ctx, &n.node_id));
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

fn ha_settings(ctx: &Ctx) -> crate::jobs::HaSettings {
    ctx.store.meta("ha_settings").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn power_json(ctx: &Ctx) -> Value {
    let asleep: Vec<String> = ctx.store.meta("asleep").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    json!({"power": power_settings(ctx), "asleep": asleep})
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
    let before: Vec<f64> = changes.iter().map(|(k, _)| crate::tune::get(k)).collect();
    let json = match crate::tune::set(&changes) { Ok(j) => j, Err(e) => return bad(&e) };
    if let Err(e) = ctx.store.set_meta("tune", &json) {
        return fail(503, "unavailable", &e);
    }
    let diff: Vec<String> = changes.iter().zip(before).map(|((k, _), old)| format!("{k} {old} -> {}", crate::tune::get(k))).collect();
    audit(ctx, "tune.changed", actor, &diff.join(", "));
    crate::head::tune_changed(ctx);
    json_out(200, json!({"tunables": crate::tune::listing()}))
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

/// How long would n numbers take now (SCH-03)?
fn estimate(ctx: &Ctx, r: &Req) -> Out {
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
           "membership": s.membership, "connected": s.connected, "dispatching": s.dispatching})
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
            audit(ctx, "user.login_failed", name, &format!("{} (throttled)", r.ip));
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
            json_out(200, json!({"user": u}))
        }
        Err(e) => fail(409, "conflict", &e),
    }
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
}

impl<F: FnMut() -> String> Sse<F> {
    pub fn new(rx: Receiver<(String, String)>, cluster: F) -> Self {
        let now = Instant::now();
        Sse { rx, cluster, buf: vec![], pos: 0, next_cluster: now, next_ping: now + Duration::from_secs(15), ha: None, ha_due: None }
    }
    fn with_ha(mut self, f: impl FnMut() -> Option<String> + Send + 'static) -> Self {
        self.ha = Some(Box::new(f));
        self
    }
    fn frame(&mut self, event: &str, data: &str) {
        self.buf = format!("event: {event}\ndata: {data}\n\n").into_bytes();
        self.pos = 0;
    }
}

impl<F: FnMut() -> String> Read for Sse<F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            let now = Instant::now();
            if let Some(d) = self.ha_due.take() {
                self.frame("ha", &d);
            } else if now >= self.next_cluster {
                self.next_cluster = now + Duration::from_secs(1);
                let j = (self.cluster)();
                self.frame("cluster", &j);
                self.ha_due = self.ha.as_mut().and_then(|f| f());
            } else if now >= self.next_ping {
                self.next_ping = now + Duration::from_secs(15);
                self.buf = b": ping\n\n".to_vec();
                self.pos = 0;
            } else {
                match self.rx.recv_timeout(self.next_cluster.min(self.next_ping).saturating_duration_since(now)) {
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

fn handle(ctx: &Arc<Ctx>, auth: &Auth, mut rq: tiny_http::Request) {
    let url = rq.url().to_string();
    let (path, q) = url.split_once('?').unwrap_or((&url, ""));
    let query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let hdr = |name: &str| rq.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string());
    let cookie = hdr("Cookie").and_then(|c| c.split(';').filter_map(|p| p.trim().strip_prefix("ck_session=")).next().map(String::from));
    let bearer = hdr("Authorization").and_then(|a| a.strip_prefix("Bearer ").map(|t| t.trim().to_string()));
    let ip = rq.remote_addr().map_or(IpAddr::from([0, 0, 0, 0]), |a| a.ip());
    let mut body = vec![];
    if matches!(rq.method().as_str(), "POST" | "PUT" | "PATCH") {
        // only a signed-in operator may send a big list-job body; everyone else is held to 1 MiB before anything is read
        let operator = || auth.who(&ctx.store, cookie.as_deref(), bearer.as_deref()).is_some_and(|w| w.role >= Role::Operator);
        let max = if path == "/api/v1/jobs" && operator() { mb("max_list_body_mb") } else { mb("max_body_mb") };
        let _ = rq.as_reader().take(max as u64 + 1).read_to_end(&mut body);
    }
    let req = Req { method: rq.method().as_str().to_string(), path: path.to_string(), query, cookie, bearer, body, ip };
    let out = if req.body.len() > mb("max_body_mb") && !(req.method == "POST" && req.path == "/api/v1/jobs") { bad(&format!("body is over {} MB", crate::tune::get("max_body_mb"))) } else { route(ctx, auth, &req) };
    let _ = match out {
        Out::Plain { status, ctype, headers, body } => {
            let mut r = Response::from_data(body).with_status_code(status).with_header(header("Content-Type", ctype));
            for (k, v) in headers {
                r.add_header(header(k, &v));
            }
            rq.respond(r)
        }
        Out::Sse => {
            let c = ctx.clone();
            let (c2, mut last) = (ctx.clone(), String::new());
            let sse = Sse::new(ctx.bus.subscribe(), move || cluster_json(&c).to_string()).with_ha(move || {
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
            rq.respond(r)
        }
    };
}

/// Blocks serving HTTP.
pub fn serve(ctx: Arc<Ctx>) {
    let server = Arc::new(Server::http(("0.0.0.0", dash_port())).unwrap_or_else(|e| crate::die(&format!("HTTP {}: {e}", dash_port()))));
    let auth = Arc::new(Auth::from_env());
    if auth.open {
        eprintln!("CK_AUTH=off: the API is open, everyone is an anonymous admin");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Bus;
    use crate::store::Store;

    fn ctx() -> Arc<Ctx> {
        Arc::new(Ctx { sh: Default::default(), store: Arc::new(Store::memory()), bus: Bus::default(), repl: Default::default() })
    }

    fn req(m: &str, path: &str, body: &str) -> Req {
        Req { method: m.into(), path: path.into(), query: HashMap::new(), cookie: None, bearer: None, body: body.as_bytes().to_vec(), ip: [127, 0, 0, 1].into() }
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
        let mut got = String::new();
        let mut b = [0u8; 256];
        let n = s.read(&mut b).unwrap();
        got.push_str(&String::from_utf8_lossy(&b[..n]));
        assert!(got.starts_with("event: cluster\ndata: {}"), "{got}");
        let n = s.read(&mut b).unwrap();
        assert!(String::from_utf8_lossy(&b[..n]).starts_with("event: job"));
        drop(bus);
        let mut rest = vec![];
        let _ = s.read_to_end(&mut rest); // cluster ticks may precede EOF
    }
}
