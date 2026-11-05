//! `ck job ls | show ID | cancel ID | result ID | token` over the HTTP API (API-02). Contract: `run(&Opts)`.
//! Head = `--head IP` (default 127.0.0.1), port = dash_port(); auth = `Authorization: Bearer $CK_TOKEN`.
//! `ck job token --name U` (or `token()` from main) reads the password from CK_PASSWORD or stdin, signs in and prints a new token.
//! No HTTP crate: one request per connection, `Connection: close`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

use crate::die;
use crate::proto::dash_port;
use crate::Opts;

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn request(o: &Opts, method: &str, path: &str, body: Option<&str>, extra: &[(&str, String)]) -> Result<Reply, String> {
    let host = o.head.clone().unwrap_or_else(|| "127.0.0.1".into());
    let mut s = TcpStream::connect((host.as_str(), dash_port())).map_err(|e| format!("cannot reach the head at {host}:{}: {e}", dash_port()))?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in extra {
        req += &format!("{k}: {v}\r\n");
    }
    if let Ok(t) = std::env::var("CK_TOKEN") {
        req += &format!("Authorization: Bearer {}\r\n", t.trim());
    }
    s.write_all(format!("{req}\r\n{body}").as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = vec![];
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    parse_reply(&raw)
}

fn parse_reply(raw: &[u8]) -> Result<Reply, String> {
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("bad reply from the head")?;
    let head = String::from_utf8_lossy(&raw[..at]).into_owned();
    let mut lines = head.lines();
    let status = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).ok_or("bad reply from the head")?;
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    Ok(Reply { status, headers, body: raw[at + 4..].to_vec() })
}

/// Reply body as JSON, or the API's error message.
fn ok_json(r: Reply) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    if r.status >= 400 {
        let m = v["error"]["message"].as_str().map_or_else(|| String::from_utf8_lossy(&r.body).into_owned(), String::from);
        return Err(format!("{} (HTTP {})", m, r.status));
    }
    Ok(v)
}

fn call(o: &Opts, method: &str, path: &str) -> Value {
    request(o, method, path, None, &[]).and_then(ok_json).unwrap_or_else(|e| die(&e))
}

fn id_arg(o: &Opts) -> u64 {
    o.rest.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| die("usage: ck job show|cancel|result ID"))
}

fn dash(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

fn row(j: &Value) -> String {
    let prio = j["priority"].as_u64().and_then(|p| crate::jobs::PRIORITIES.get(p as usize)).copied().unwrap_or("-");
    format!(
        "{:>6}  {:<9} {:<5} {:<10} {:>14}/{:<14} {:<12} {} {}",
        dash(&j["id"]),
        dash(&j["state"]),
        dash(&j["kind"]),
        prio,
        dash(&j["done"]),
        dash(&j["total"]),
        dash(&j["submitter"]),
        dash(&j["name"]),
        j["selector"].as_str().filter(|s| !s.is_empty()).map_or(String::new(), |s| format!("[{s}]"))
    )
}

pub fn run(o: &Opts) {
    match o.rest.first().map(String::as_str) {
        Some("ls") => {
            let v = call(o, "GET", "/api/v1/jobs?limit=50");
            println!("{:>6}  {:<9} {:<5} {:<10} {:>29} {:<12} NAME [SELECTOR]", "ID", "STATE", "KIND", "PRIORITY", "DONE/TOTAL", "SUBMITTER");
            v["jobs"].as_array().into_iter().flatten().for_each(|j| println!("{}", row(j)));
        }
        Some("show") => {
            let v = call(o, "GET", &format!("/api/v1/jobs/{}", id_arg(o)));
            let j = &v["job"];
            for k in ["id", "name", "kind", "state", "submitter", "from", "end", "chunk", "done", "total", "priority", "selector", "created_ms", "started_ms", "finished_ms", "error"] {
                println!("{k:<12} {}", dash(&j[k]));
            }
            println!("{:<12} {}", "parts", dash(&v["parts"]));
        }
        Some("cancel") => {
            let v = call(o, "POST", &format!("/api/v1/jobs/{}/cancel", id_arg(o)));
            println!("{}", row(&v["job"]));
        }
        Some("result") => {
            // ponytail: Opts has no --reducer flag (main.rs is the lead's); CK_REDUCER or a trailing `--reducer R` instead
            let red = std::env::var("CK_REDUCER").ok().or_else(|| o.rest.iter().position(|a| a == "--reducer").and_then(|i| o.rest.get(i + 1).cloned()));
            let q = red.map_or(String::new(), |r| format!("?reducer={}", enc(&r)));
            let r = request(o, "GET", &format!("/api/v1/jobs/{}/result{q}", id_arg(o)), None, &[]).unwrap_or_else(|e| die(&e));
            if r.status >= 400 {
                die(&ok_json(r).unwrap_err());
            }
            std::io::stdout().write_all(&r.body).ok();
        }
        Some("token") => token(o),
        _ => die("usage: ck job ls | show ID | cancel ID | result ID | token --name USER   (head: --head IP; auth: CK_TOKEN)"),
    }
}

/// Sign in with CK_USER (or --name) and CK_PASSWORD (or one line on stdin), print a new API token.
pub fn token(o: &Opts) {
    let name = std::env::var("CK_USER").unwrap_or_else(|_| o.name.clone());
    let pw = std::env::var("CK_PASSWORD").unwrap_or_else(|_| {
        eprint!("password for {name}: ");
        let mut l = String::new();
        std::io::stdin().read_line(&mut l).ok();
        l.trim_end_matches(['\r', '\n']).to_string()
    });
    let body = serde_json::json!({"name": name, "password": pw}).to_string();
    let r = request(o, "POST", "/api/v1/login", Some(&body), &[]).unwrap_or_else(|e| die(&e));
    let cookie = r.headers.iter().find(|h| h.0 == "set-cookie").and_then(|h| h.1.split(';').next()).map(String::from);
    ok_json(r).unwrap_or_else(|e| die(&e));
    let cookie = cookie.unwrap_or_else(|| die("the head set no session"));
    let v = request(o, "POST", "/api/v1/tokens", None, &[("Cookie", cookie)]).and_then(ok_json).unwrap_or_else(|e| die(&e));
    println!("{}", v["token"].as_str().unwrap_or_default());
}

/// API-02: every route (users::ROUTES) maps to a CLI command or says why not. Checked in api::tests.
pub const CLI_MAP: &[(&str, &str, &str)] = &[
    ("GET", "/me", "no CLI needed: session"), ("GET", "/ha", "ck admin ha"), ("POST", "/ha/switchover", "ck admin switchover NODE"),
    ("POST", "/nodes/power", "ck admin sleep|wake"), ("GET", "/power", "ck admin power"), ("PUT", "/power", "ck admin power"), ("GET", "/ha/settings", "ck admin standbys"), ("PUT", "/ha/settings", "ck admin standbys"), ("GET", "/tunables", "ck admin tune"), ("PUT", "/tunables", "ck admin tune KEY VALUE|default"), ("GET", "/ha/candidates", "ck admin heads"), ("GET", "/logs", "ck logs"), ("POST", "/bootstrap", "no CLI needed: first-run setup in the browser"),
    ("POST", "/login", "ck token"), ("POST", "/logout", "no CLI needed: session"), ("POST", "/tokens", "ck token"),
    ("GET", "/users", "no CLI needed: console user admin"), ("POST", "/users", "no CLI needed: console user admin"),
    ("GET", "/cluster", "no CLI needed: stream (see ck status)"),
    ("GET", "/nodes", "ck admin nodes"), ("GET", "/nodes/{id}", "ck admin nodes"),
    ("POST", "/nodes/state", "ck admin state|approve"), ("PUT", "/nodes/{id}/place", "ck admin place|unplace"),
    ("GET", "/labs", "ck admin labs"), ("POST", "/labs", "ck admin lab-add"),
    ("PATCH", "/labs/{id}", "no CLI needed: console lab editor"), ("DELETE", "/labs/{id}", "no CLI needed: console lab editor"),
    ("POST", "/join-codes", "ck admin join-code"),
    ("GET", "/jobs", "ck job ls"), ("POST", "/jobs", "ck run"), ("GET", "/jobs/{id}", "ck job show"),
    ("GET", "/jobs/{id}/result", "ck job result"), ("POST", "/jobs/{id}/cancel", "ck job cancel"),
    ("GET", "/events", "no CLI needed: console activity page"), ("GET", "/stream", "no CLI needed: stream"),
    ("GET", "/select", "ck admin select"), ("PUT", "/nodes/{id}/labels", "ck admin label"),
    ("GET", "/pools", "ck admin pools"), ("PUT", "/pools/{name}", "ck admin pool"), ("DELETE", "/pools/{name}", "ck admin pool-rm"),
    ("GET", "/caps", "ck admin caps"), ("POST", "/caps", "ck admin cap"), ("DELETE", "/caps/{id}", "ck admin cap-rm"),
    ("GET", "/reserves", "ck admin reserves"), ("POST", "/reserves", "ck admin reserve"), ("DELETE", "/reserves/{id}", "ck admin reserve-rm"),
    ("GET", "/windows", "ck admin windows"), ("POST", "/windows", "ck admin window"), ("DELETE", "/windows/{id}", "ck admin window-rm"),
    ("GET", "/reservations", "ck admin reservations"), ("POST", "/reservations", "ck admin reservation"), ("DELETE", "/reservations/{id}", "ck admin reservation-rm"),
    ("GET", "/estimate", "ck admin estimate"),
    ("GET", "/config", "ck admin config-export"), ("POST", "/config", "ck admin config-import"),
];

fn tz_offset() -> i64 {
    std::env::var("CK_TZ_OFFSET_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(330)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "YYYY-MM-DD HH:MM" local time -> Unix ms.
fn parse_local(s: &str, offset_min: i64) -> Option<i64> {
    let (d, t) = s.trim().split_once(' ')?;
    let (mut dp, mut tp) = (d.split('-'), t.trim().split(':'));
    let n = |p: &mut std::str::Split<char>| p.next()?.parse::<i64>().ok();
    let (y, mo, da, h, mi) = (n(&mut dp)?, n(&mut dp)?, n(&mut dp)?, n(&mut tp)?, n(&mut tp)?);
    if !((1..=12).contains(&mo) && (1..=31).contains(&da) && (0..24).contains(&h) && (0..60).contains(&mi)) {
        return None;
    }
    Some(((days_from_civil(y, mo, da) * 24 + h) * 60 + mi - offset_min) * 60_000)
}

fn fmt_local(ms: i64, offset_min: i64) -> String {
    let min = ms.div_euclid(60_000) + offset_min;
    let (y, m, d) = civil_from_days(min.div_euclid(1440));
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", min.rem_euclid(1440) / 60, min.rem_euclid(60))
}

fn usage_admin() -> ! {
    die("usage: ck admin nodes | approve ID... | state STATE ID... | labs | lab-add NAME W H | place NODE LAB X Y | unplace NODE | join-code [USES] [TTL_MIN]\n  | select SELECTOR | label NODE KEY=VALUE|KEY= ... | pools | pool NAME NODE... | pool-rm NAME | caps | cap SELECTOR THREADS | cap-rm ID\n  | reserves | reserve SELECTOR N [PCT] | reserve-rm ID | windows | window SELECTOR DAYS START END block|cap [THREADS] | window-rm ID\n  | reservations | reservation SELECTOR START END (local \"YYYY-MM-DD HH:MM\", quote them) | reservation-rm ID | estimate N [SELECTOR] | ha | switchover NODE | sleep ID... | wake ID... | power [off|eco] [IDLE_MIN] [KEEP_AWAKE] | standbys [N] [SELECTOR] | heads | tune [KEY VALUE|default] | config-export > file | config-import FILE   (env: CK_REASON, CK_AUTO=1, CK_DRY_RUN=1, CK_TOKEN)")
}

fn send(o: &Opts, method: &str, path: &str, body: Value) -> Value {
    request(o, method, path, Some(&body.to_string()), &[]).and_then(ok_json).unwrap_or_else(|e| die(&e))
}

/// Percent-encode everything but unreserved characters (path segments and query values).
fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn arg(o: &Opts, i: usize) -> &str {
    o.rest.get(i).map(String::as_str).unwrap_or_else(|| usage_admin())
}

fn num(o: &Opts, i: usize) -> u64 {
    o.rest.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage_admin())
}

fn node_line(n: &Value) -> String {
    let seat = if n["lab_id"].is_null() { "-".to_string() } else { format!("lab{}({},{})", dash(&n["lab_id"]), dash(&n["x"]), dash(&n["y"])) };
    format!("{:<24} {:<12} {:<7} {:<15} {:<14} {}", dash(&n["node_id"]), dash(&n["state"]), if n["online"] == true { "online" } else { "offline" }, dash(&n["addr"]), seat, dash(&n["host"]))
}

fn set_state(o: &Opts, state: &str, ids: &[String]) {
    if ids.is_empty() {
        usage_admin();
    }
    let v = send(o, "POST", "/api/v1/nodes/state", serde_json::json!({"node_ids": ids, "state": state, "reason": std::env::var("CK_REASON").unwrap_or_default()}));
    v["nodes"].as_array().into_iter().flatten().for_each(|n| println!("{}", node_line(n)));
}

/// `ck admin ...` (args in o.rest).
pub fn admin(o: &Opts) {
    let rest = &o.rest;
    match rest.first().map(String::as_str) {
        Some("nodes") => {
            println!("{:<24} {:<12} {:<7} {:<15} {:<14} HOST", "NODE", "STATE", "LINK", "ADDR", "SEAT");
            call(o, "GET", "/api/v1/nodes")["nodes"].as_array().into_iter().flatten().for_each(|n| println!("{}", node_line(n)));
        }
        Some("approve") => set_state(o, "active", &rest[1..]),
        Some("state") => set_state(o, rest.get(1).map_or_else(|| usage_admin(), String::as_str), rest.get(2..).unwrap_or(&[])),
        Some("labs") => {
            println!("{:>4}  {:<24} SIZE", "ID", "NAME");
            call(o, "GET", "/api/v1/labs")["labs"].as_array().into_iter().flatten().for_each(|l| println!("{:>4}  {:<24} {}x{}", dash(&l["id"]), dash(&l["name"]), dash(&l["w"]), dash(&l["h"])));
        }
        Some("lab-add") => {
            let name = rest.get(1).unwrap_or_else(|| usage_admin());
            let v = send(o, "POST", "/api/v1/labs", serde_json::json!({"name": name, "w": num(o, 2), "h": num(o, 3)}));
            println!("lab {} {} {}x{}", dash(&v["lab"]["id"]), dash(&v["lab"]["name"]), dash(&v["lab"]["w"]), dash(&v["lab"]["h"]));
        }
        Some("place") => {
            let node = rest.get(1).unwrap_or_else(|| usage_admin());
            let v = send(o, "PUT", &format!("/api/v1/nodes/{}/place", node.replace('%', "%25").replace('/', "%2F")), serde_json::json!({"lab_id": num(o, 2), "x": num(o, 3), "y": num(o, 4)}));
            println!("{}", node_line(&v["node"]));
        }
        Some("unplace") => {
            let node = rest.get(1).unwrap_or_else(|| usage_admin());
            let v = send(o, "PUT", &format!("/api/v1/nodes/{}/place", node.replace('%', "%25").replace('/', "%2F")), serde_json::json!({"lab_id": null}));
            println!("{}", node_line(&v["node"]));
        }
        Some("select") => {
            let v = call(o, "GET", &format!("/api/v1/select?q={}", enc(&rest[1..].join(" "))));
            match v["error"].as_str() {
                Some(e) => die(e),
                None => println!("{} match\n{}", dash(&v["count"]), v["node_ids"].as_array().into_iter().flatten().map(dash).collect::<Vec<_>>().join("\n")),
            }
        }
        Some("label") => {
            let mut b = serde_json::Map::new();
            for kv in rest.get(2..).unwrap_or(&[]) {
                let (k, val) = kv.split_once('=').unwrap_or_else(|| usage_admin());
                b.insert(k.into(), if val.is_empty() { Value::Null } else { val.into() });
            }
            if b.is_empty() {
                usage_admin();
            }
            let v = send(o, "PUT", &format!("/api/v1/nodes/{}/labels", enc(arg(o, 1))), Value::Object(b));
            println!("{}  labels {}", dash(&v["node"]["node_id"]), dash(&v["node"]["labels"]));
        }
        Some("pools") => {
            println!("{:<20} {:>7}  MEMBERS", "POOL", "NODES");
            call(o, "GET", "/api/v1/pools")["pools"].as_array().into_iter().flatten().for_each(|p| {
                let m: Vec<String> = p["members"].as_array().into_iter().flatten().map(dash).collect();
                println!("{:<20} {:>7}  {}", dash(&p["name"]), m.len(), m.join(","));
            });
        }
        Some("pool") => {
            let v = send(o, "PUT", &format!("/api/v1/pools/{}", enc(arg(o, 1))), serde_json::json!({"members": &rest[2..], "note": std::env::var("CK_NOTE").unwrap_or_default()}));
            println!("pool {} {} nodes", dash(&v["pool"]["name"]), v["pool"]["members"].as_array().map_or(0, Vec::len));
        }
        Some("pool-rm") => {
            send(o, "DELETE", &format!("/api/v1/pools/{}", enc(arg(o, 1))), Value::Null);
            println!("deleted pool {}", arg(o, 1));
        }
        Some("caps") => {
            println!("{:>4}  {:>7}  SELECTOR", "ID", "THREADS");
            call(o, "GET", "/api/v1/caps")["caps"].as_array().into_iter().flatten().for_each(|c| println!("{:>4}  {:>7}  {}", dash(&c["id"]), dash(&c["max_threads"]), dash(&c["selector"])));
        }
        Some("cap") => {
            let v = send(o, "POST", "/api/v1/caps", serde_json::json!({"selector": arg(o, 1), "max_threads": num(o, 2), "note": std::env::var("CK_NOTE").unwrap_or_default()}));
            println!("cap {} [{}] max {} threads", dash(&v["cap"]["id"]), dash(&v["cap"]["selector"]), dash(&v["cap"]["max_threads"]));
        }
        Some("cap-rm") => {
            send(o, "DELETE", &format!("/api/v1/caps/{}", num(o, 1)), Value::Null);
            println!("deleted cap {}", num(o, 1));
        }
        Some("reserves") => {
            println!("{:>4}  {:>4} {:>4}  {:<30} HELD", "ID", "N", "PCT", "SELECTOR");
            call(o, "GET", "/api/v1/reserves")["reserves"].as_array().into_iter().flatten().for_each(|r| {
                let h: Vec<String> = r["held"].as_array().into_iter().flatten().map(dash).collect();
                println!("{:>4}  {:>4} {:>4}  {:<30} {}", dash(&r["id"]), dash(&r["keep_n"]), dash(&r["keep_pct"]), dash(&r["selector"]), h.join(","));
            });
        }
        Some("reserve") => {
            let mut b = serde_json::json!({"selector": arg(o, 1), "keep_n": num(o, 2), "note": std::env::var("CK_NOTE").unwrap_or_default()});
            if rest.len() > 3 {
                b["keep_pct"] = num(o, 3).into();
            }
            let v = send(o, "POST", "/api/v1/reserves", b);
            println!("reserve {} [{}] keep {} / {}%", dash(&v["reserve"]["id"]), dash(&v["reserve"]["selector"]), dash(&v["reserve"]["keep_n"]), dash(&v["reserve"]["keep_pct"]));
        }
        Some("reserve-rm") => {
            send(o, "DELETE", &format!("/api/v1/reserves/{}", num(o, 1)), Value::Null);
            println!("deleted reserve {}", num(o, 1));
        }
        Some("windows") => {
            println!("{:>4}  {:<14} {:>5}-{:<5} {:<6} {:>7} {:<6} SELECTOR", "ID", "DAYS", "START", "END", "ACTION", "THREADS", "NOW");
            call(o, "GET", "/api/v1/windows")["windows"].as_array().into_iter().flatten().for_each(|w| {
                println!("{:>4}  {:<14} {:>5}-{:<5} {:<6} {:>7} {:<6} {}", dash(&w["id"]), dash(&w["days"]), dash(&w["start"]), dash(&w["end"]), dash(&w["action"]), dash(&w["cap_threads"]), if w["active_now"] == true { "active" } else { "-" }, dash(&w["selector"]))
            });
        }
        Some("window") => {
            let mut b = serde_json::json!({"selector": arg(o, 1), "days": arg(o, 2), "start": arg(o, 3), "end": arg(o, 4), "action": arg(o, 5), "note": std::env::var("CK_NOTE").unwrap_or_default()});
            if rest.len() > 6 {
                b["cap_threads"] = num(o, 6).into();
            }
            let w = &send(o, "POST", "/api/v1/windows", b)["window"];
            println!("window {} [{}] {} {}-{} {}", dash(&w["id"]), dash(&w["selector"]), dash(&w["days"]), dash(&w["start"]), dash(&w["end"]), dash(&w["action"]));
        }
        Some("window-rm") => {
            send(o, "DELETE", &format!("/api/v1/windows/{}", num(o, 1)), Value::Null);
            println!("deleted window {}", num(o, 1));
        }
        Some("reservations") => {
            println!("{:>4}  {:<16} {:<16} {:<6} SELECTOR", "ID", "START", "END", "NOW");
            call(o, "GET", "/api/v1/reservations")["reservations"].as_array().into_iter().flatten().for_each(|x| {
                let t = |k: &str| x[k].as_i64().map_or("-".into(), |ms| fmt_local(ms, tz_offset()));
                println!("{:>4}  {:<16} {:<16} {:<6} {}", dash(&x["id"]), t("start_ms"), t("end_ms"), if x["active_now"] == true { "active" } else { "-" }, dash(&x["selector"]))
            });
        }
        Some("reservation") => {
            let ms = |i: usize| parse_local(arg(o, i), tz_offset()).unwrap_or_else(|| die("times are \"YYYY-MM-DD HH:MM\" (local, CK_TZ_OFFSET_MIN, default 330)"));
            let b = serde_json::json!({"selector": arg(o, 1), "start_ms": ms(2), "end_ms": ms(3), "note": std::env::var("CK_NOTE").unwrap_or_default()});
            let x = &send(o, "POST", "/api/v1/reservations", b)["reservation"];
            println!("reservation {} [{}] {} .. {}", dash(&x["id"]), dash(&x["selector"]), arg(o, 2), arg(o, 3));
        }
        Some("reservation-rm") => {
            send(o, "DELETE", &format!("/api/v1/reservations/{}", num(o, 1)), Value::Null);
            println!("deleted reservation {}", num(o, 1));
        }
        Some("estimate") => {
            let v = call(o, "GET", &format!("/api/v1/estimate?n={}&selector={}", num(o, 1), enc(&rest[2..].join(" "))));
            match v["eta_secs"].as_f64() {
                Some(s) => println!("{} PCs, {:.0} numbers/s: about {:.1} s", dash(&v["matching_pcs"]), v["fleet_rate"].as_f64().unwrap_or(0.0), s),
                None => println!("{} PCs, rate unknown: no estimate", dash(&v["matching_pcs"])),
            }
        }
        Some("switchover") => {
            let node = rest.get(1).unwrap_or_else(|| usage_admin());
            let v = send(o, "POST", "/api/v1/ha/switchover", serde_json::json!({"to": node}));
            println!("handing over to {} at {}", node, dash(&v["to"]));
        }
        Some(a @ ("sleep" | "wake")) => {
            if rest.len() < 2 { usage_admin() }
            let v = send(o, "POST", "/api/v1/nodes/power", serde_json::json!({"node_ids": &rest[1..], "action": a}));
            v["nodes"].as_array().cloned().unwrap_or_default().iter().for_each(|n| println!("{}  {}", dash(&n["node_id"]), dash(&n["power"])));
        }
        Some("power") => {
            let mut body = serde_json::Map::new();
            if let Some(m) = rest.get(1) { body.insert("mode".into(), m.clone().into()); }
            if rest.len() > 2 { body.insert("idle_min".into(), num(o, 2).into()); }
            if rest.len() > 3 { body.insert("keep_awake".into(), num(o, 3).into()); }
            let v = if body.is_empty() { call(o, "GET", "/api/v1/power") } else { send(o, "PUT", "/api/v1/power", Value::Object(body)) };
            let p = &v["power"];
            println!("power {}  idle {} min  keep awake {}  wake on demand {}  asleep {}", dash(&p["mode"]), dash(&p["idle_min"]), dash(&p["keep_awake"]), dash(&p["wake_on_demand"]), v["asleep"].as_array().map_or(0, |a| a.len()));
        }
        Some("standbys") => {
            let mut body = serde_json::Map::new();
            if rest.len() > 1 { body.insert("target_standbys".into(), num(o, 1).into()); }
            if rest.len() > 2 { body.insert("selector".into(), rest[2..].join(" ").into()); }
            let v = if body.is_empty() { call(o, "GET", "/api/v1/ha/settings") } else { send(o, "PUT", "/api/v1/ha/settings", Value::Object(body)) };
            println!("keep {} standby(s)  selector {}", dash(&v["settings"]["target_standbys"]), dash(&v["settings"]["selector"]));
        }
        Some("heads") => {
            let v = call(o, "GET", "/api/v1/ha/candidates");
            println!("{:>5}  {:<14} {:<12} {:<10} {:<9} why not", "score", "host", "lab", "role", "eligible");
            for c in v["candidates"].as_array().cloned().unwrap_or_default() {
                println!("{:>5}  {:<14} {:<12} {:<10} {:<9} {}", dash(&c["score"]), dash(&c["host"]), dash(&c["lab"]), dash(&c["role"]), dash(&c["eligible"]), c["why"].as_str().unwrap_or(""));
            }
        }
        Some("tune") => {
            let v = if rest.len() > 1 {
                let val = match rest.get(2).map(|s| s.as_str()) {
                    Some("default") => Value::Null,
                    Some(x) => x.parse::<f64>().map_or_else(|_| die("VALUE must be a number or default"), Value::from),
                    None => usage_admin(),
                };
                send(o, "PUT", "/api/v1/tunables", serde_json::json!({ rest[1].as_str(): val }))
            } else {
                call(o, "GET", "/api/v1/tunables")
            };
            let mut group = String::new();
            for t in v["tunables"].as_array().cloned().unwrap_or_default() {
                if rest.len() > 1 && t["key"].as_str() != Some(rest[1].as_str()) { continue }
                let g = dash(&t["group"]);
                if g != group { println!("{g}"); group = g; }
                let mark = if t["source"] == "default" { "" } else { "*" };
                println!("  {:<24} {:>12} {:<8} {:<8} {}{}", dash(&t["key"]), dash(&t["value"]), dash(&t["unit"]), dash(&t["scope"]), dash(&t["source"]), mark);
            }
        }
        Some("ha") => {
            let v = call(o, "GET", "/api/v1/ha");
            println!("role {}  epoch {}  last_seq {}  durable_seq {}", dash(&v["role"]), dash(&v["epoch"]), dash(&v["last_seq"]), dash(&v["durable_seq"]));
            let sb = v["standbys"].as_array().cloned().unwrap_or_default();
            sb.iter().for_each(|s| println!("standby {}  acked {}  lag {}", dash(&s["node_id"]), dash(&s["acked_seq"]), dash(&s["lag"])));
            let quorum = crate::repl::has_quorum(v["connected"].as_u64().unwrap_or(0) as usize, v["membership"].as_u64().unwrap_or(0) as usize);
            println!("{} standby(s)  quorum {} ({} of {} nodes connected)  dispatching {}", sb.len(), if quorum { "yes" } else { "NO" }, dash(&v["connected"]), dash(&v["membership"]), dash(&v["dispatching"]));
        }
        Some("config-export") => println!("{}", serde_json::to_string_pretty(&call(o, "GET", "/api/v1/config")).unwrap_or_default()),
        Some("config-import") => {
            let body = std::fs::read_to_string(arg(o, 1)).unwrap_or_else(|e| die(&format!("{}: {e}", arg(o, 1))));
            let dry = std::env::var("CK_DRY_RUN").is_ok_and(|v| v == "1");
            let v = request(o, "POST", &format!("/api/v1/config{}", if dry { "?dry_run=1" } else { "" }), Some(&body), &[]).and_then(ok_json).unwrap_or_else(|e| die(&e));
            let lines = v["changes"].as_array().cloned().unwrap_or_default();
            lines.iter().for_each(|l| println!("{}", dash(l)));
            println!("{} change(s){}", lines.len(), if dry { " (dry run, nothing written)" } else { " applied" });
        }
        Some("join-code") => {
            let mut b = serde_json::json!({"auto_approve": std::env::var("CK_AUTO").is_ok_and(|v| v == "1")});
            if rest.len() > 1 {
                b["uses"] = num(o, 1).into();
            }
            if rest.len() > 2 {
                b["ttl_min"] = num(o, 2).into();
            }
            let v = send(o, "POST", "/api/v1/join-codes", b);
            println!("{}\nuses {}  auto-approve {}  expires_ms {}  (shown once)", dash(&v["code"]), dash(&v["uses"]), dash(&v["auto_approve"]), dash(&v["expires_ms"]));
        }
        _ => usage_admin(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_times() {
        // 2026-11-04 09:30 IST = 04:00 UTC
        let ms = parse_local("2026-11-04 09:30", 330).unwrap();
        assert_eq!(fmt_local(ms, 330), "2026-11-04 09:30");
        assert_eq!(fmt_local(ms, 0), "2026-11-04 04:00");
        assert_eq!(parse_local("1970-01-01 00:00", 0), Some(0));
        assert!(parse_local("2026-13-01 00:00", 0).is_none() && parse_local("junk", 0).is_none());
    }

    #[test]
    fn reply_parsing_and_errors() {
        let r = parse_reply(b"HTTP/1.1 403 Forbidden\r\nSet-Cookie: ck_session=ab; HttpOnly\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((r.status, r.headers[0].0.as_str(), r.body.as_slice()), (403, "set-cookie", &b"{}"[..]));
        let r = parse_reply(b"HTTP/1.1 409 Conflict\r\n\r\n{\"error\":{\"code\":\"conflict\",\"message\":\"already done\"}}").unwrap();
        assert_eq!(ok_json(r).unwrap_err(), "already done (HTTP 409)");
        assert!(parse_reply(b"junk").is_err());
        let j = serde_json::json!({"id": 3, "state": "done", "kind": "sha", "done": 10, "total": 10, "submitter": "ann", "name": "x"});
        assert!(row(&j).contains("done") && row(&j).contains("ann"));
    }
}
