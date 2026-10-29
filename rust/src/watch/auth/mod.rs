//! Agent H: admin bootstrap, sessions, roles, worker credentials, redaction, media roots.
//!
//! Nothing here is wired into `head.rs` yet; see docs/RELEASE_CHECKLIST.md. Design notes:
//! - Users and worker-token hashes live in `<root>/vision/data/auth.json` (mode 0600).
//!   Passwords are PBKDF2-HMAC-SHA256 (>= 210_000 iterations, 16-byte random salt), hand-rolled on
//!   `sha2` because no argon2/bcrypt crate is approved. Raw worker tokens are never stored.
//! - Sessions are in memory only. A head restart logs everyone out (documented behaviour).
//! - Worker credentials (`Authorization: Bearer cwt_...`) and user sessions (`cw_session` cookie) are
//!   disjoint: a worker token on a user route and a session on a worker route are both `forbidden`.
//! - Open mode (`CW_AUTH=off`, or no users and `CW_AUTH != required`) keeps the local demo working with an anonymous
//!   administrator. The coordinator must bind to 127.0.0.1 in that mode (see `Auth::mode`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::watch::domain::{ApiError, AuditRecord};

pub const COOKIE: &str = "cw_session";
pub const PBKDF2_ITERS: u32 = 210_000;
const IDLE_SECS: u64 = 12 * 3600;
const MAX_SECS: u64 = 7 * 24 * 3600;
const LOGIN_WINDOW_SECS: u64 = 300;
const LOGIN_MAX_FAILS: usize = 5;
const MIN_PASSWORD: usize = 12;

pub type HttpOut = (u16, Vec<(String, String)>, String);

// ---------------------------------------------------------------- crypto

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// Constant-time equality (length is not secret here: all compared values are fixed-size hex).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut d = (a.len() ^ b.len()) as u32;
    for i in 0..a.len().max(b.len()) {
        d |= (*a.get(i).unwrap_or(&0) ^ *b.get(i).unwrap_or(&0)) as u32;
    }
    d == 0
}

pub(crate) fn random(n: usize) -> Result<Vec<u8>, ApiError> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| ApiError::new("unavailable", format!("no system randomness: {e}")))?;
    Ok(buf)
}

/// RFC 2104 HMAC-SHA256 with the key pads pre-absorbed so PBKDF2 can reuse them.
struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    fn new(key: &[u8]) -> Hmac {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha256::new();
        inner.update(k.map(|b| b ^ 0x36));
        let mut outer = Sha256::new();
        outer.update(k.map(|b| b ^ 0x5c));
        Hmac { inner, outer }
    }
    fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut i = self.inner.clone();
        for p in parts {
            i.update(p);
        }
        let mut o = self.outer.clone();
        o.update(i.finalize());
        o.finalize().into()
    }
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    Hmac::new(key).mac(&[msg])
}

/// RFC 8018 PBKDF2 with HMAC-SHA256.
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iters: u32, out: &mut [u8]) {
    let prf = Hmac::new(password);
    for (block, chunk) in out.chunks_mut(32).enumerate() {
        let idx = (block as u32 + 1).to_be_bytes();
        let mut u = prf.mac(&[salt, &idx]);
        let mut t = u;
        for _ in 1..iters {
            u = prf.mac(&[&u]);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

fn hash_password(password: &str, salt: &[u8], iters: u32) -> String {
    let mut out = [0u8; 32];
    pbkdf2_hmac_sha256(password.as_bytes(), salt, iters, &mut out);
    hex(&out)
}

// ---------------------------------------------------------------- model

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    ReadOnly,
    Reviewer,
    Operator,
    Administrator,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "administrator" | "admin" => Some(Role::Administrator),
            "operator" => Some(Role::Operator),
            "reviewer" => Some(Role::Reviewer),
            "read_only" | "readonly" | "read-only" => Some(Role::ReadOnly),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Role::Administrator => "administrator",
            Role::Operator => "operator",
            Role::Reviewer => "reviewer",
            Role::ReadOnly => "read_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Principal {
    pub name: String,
    pub role: Role,
    /// True when no real identity was established (open mode or public route).
    pub anonymous: bool,
    /// "open-trusted-lan" or "enforced".
    pub mode: String,
    /// True for a worker credential. A worker principal is never a user.
    pub worker: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct User {
    username: String,
    role: Role,
    salt: String,
    hash: String,
    iters: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct WorkerToken {
    name: String,
    sha256: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Persist {
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    worker_tokens: Vec<WorkerToken>,
}

struct Session {
    user: String,
    created: u64,
    last: u64,
}

pub struct Auth {
    path: PathBuf,
    required: bool,
    /// auth.json exists but could not be parsed: fail closed, never fall back to open mode.
    locked: bool,
    secure_cookie: bool,
    iters: u32,
    persist: Mutex<Persist>,
    sessions: Mutex<HashMap<String, Session>>,
    ephemeral_workers: Mutex<Vec<String>>,
    bootstrap_secret: Mutex<Option<String>>,
    bootstrap_generated: bool,
    fails: Mutex<HashMap<String, VecDeque<u64>>>,
    skew: AtomicU64,
}

fn unauthorized(m: &str) -> ApiError {
    ApiError::new("unauthorized", m)
}
fn forbidden(m: &str) -> ApiError {
    ApiError::new("forbidden", m)
}

// ---------------------------------------------------------------- route policy

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Public,
    /// Worker bearer token.
    Worker,
    /// User session with at least this role.
    Min(Role),
    /// Unknown mutation: never allowed.
    Deny,
}

use Access::{Min, Public, Worker};
const RO: Access = Min(Role::ReadOnly);
const REV: Access = Min(Role::Reviewer);
const OP: Access = Min(Role::Operator);
const ADM: Access = Min(Role::Administrator);

/// (method class, segment pattern, access). First match wins; patterns match by whole segments as a
/// prefix, `*` matches one segment, `/` matches only the root. Method class: GET (also HEAD),
/// W (any other method), * (any), or an exact method.
///
/// Role meaning: read_only sees dashboards, state, sources and query results but no footage;
/// reviewer additionally views media/thumbnails/live and records review decisions and exports;
/// operator changes sources/rules/models/settings and reads the audit log; administrator manages
/// users, worker tokens and source credentials.
static POLICY: &[(&str, &str, Access)] = &[
    // public: health and static UI shell
    ("*", "/api/health", Public),
    ("GET", "/", Public),
    ("GET", "/wall", Public),
    ("GET", "/find", Public),
    ("GET", "/sources", Public),
    ("GET", "/logs", Public),
    ("GET", "/analytics", Public),
    ("GET", "/setup", Public),
    ("GET", "/clip", Public),
    ("GET", "/review", Public),
    ("GET", "/login", Public),
    ("GET", "/favicon.ico", Public),
    ("GET", "/ui", Public),
    ("GET", "/join", Public),
    // auth
    ("POST", "/api/v1/auth/login", Public),
    ("POST", "/api/v1/auth/logout", Public),
    ("POST", "/api/v1/auth/bootstrap", Public),
    ("GET", "/api/v1/auth/me", RO),
    ("POST", "/api/v1/auth/users", ADM),
    ("POST", "/api/v1/auth/password", RO),
    ("POST", "/api/v1/auth/worker-tokens", ADM),
    // joining a PC to the cluster. The install scripts carry only the head's address. Bundles and source are public in open
    // mode and need a valid pairing code otherwise (checked by the handler); enrolling needs a code in enforced mode.
    ("GET", "/api/v1/join/info", Public),
    ("GET", "/api/v1/join/install.sh", Public),
    ("GET", "/api/v1/join/install.ps1", Public),
    ("GET", "/api/v1/join/bundle", Public),
    ("GET", "/api/v1/join/source.tar.gz", Public),
    ("POST", "/api/v1/join/enroll", Public),
    ("*", "/api/v1/join/codes", ADM),
    ("GET", "/api/v1/join/manifest", Worker),
    ("GET", "/api/v1/join/file", Worker),
    // worker credential routes (legacy and v1)
    ("POST", "/api/v1/heartbeat", Worker),
    ("POST", "/api/v1/lease", Worker),
    ("GET", "/api/v1/worker/media", Worker),
    ("POST", "/api/index", Worker),
    ("POST", "/api/v1/index", Worker),
    ("GET", "/api/work", Worker),
    ("POST", "/api/stats", Worker),
    ("POST", "/api/events", Worker),
    ("POST", "/api/preview", Worker),
    // legacy footage and audit
    ("GET", "/api/preview", REV),
    ("GET", "/api/live", REV),
    ("GET", "/api/media", REV),
    ("GET", "/api/hit", REV),
    ("GET", "/api/logs", OP),
    // legacy reads
    ("GET", "/api/state", RO),
    ("GET", "/api/sources", RO),
    ("GET", "/api/capabilities", RO),
    ("POST", "/api/query", RO),
    ("POST", "/api/find", RO),
    // legacy mutations
    ("POST", "/api/sources", OP),
    ("POST", "/api/confirm", REV),
    ("POST", "/api/rules", OP),
    ("POST", "/api/modules", OP),
    ("POST", "/api/settings", OP),
    ("POST", "/api/searches", OP),
    // v1
    ("*", "/api/v1/sources/*/credentials", ADM),
    ("GET", "/api/v1/sources", RO),
    ("W", "/api/v1/sources", OP),
    ("GET", "/api/v1/jobs", RO),
    ("W", "/api/v1/jobs", OP),
    ("GET", "/api/v1/models", RO),
    ("GET", "/api/v1/capabilities", RO),
    ("GET", "/api/v1/queries", RO),
    ("POST", "/api/v1/queries", RO),
    ("GET", "/api/v1/media", REV),
    ("GET", "/api/v1/reviews", RO),
    ("W", "/api/v1/reviews", REV),
    ("GET", "/api/v1/exports", REV),
    ("W", "/api/v1/exports", REV),
    ("GET", "/api/v1/alerts", RO),
    ("W", "/api/v1/alerts", REV),
    ("POST", "/api/v1/rules/backtest", OP),
    ("GET", "/api/v1/audit", OP),
    ("GET", "/api/v1/events", RO),
    ("GET", "/api/v1/cluster", RO),
    ("GET", "/api/v1/benchmarks", RO),
    ("W", "/api/v1/benchmarks", OP),
];

fn method_matches(class: &str, method: &str) -> bool {
    let get = method == "GET" || method == "HEAD";
    match class {
        "*" => true,
        "GET" => get,
        "W" => !get,
        m => m == method,
    }
}

fn path_matches(pattern: &str, path: &str) -> bool {
    if pattern == "/" {
        return path == "/";
    }
    let mut p = path.split('/').filter(|s| !s.is_empty());
    pattern.split('/').filter(|s| !s.is_empty()).all(|seg| match p.next() {
        Some(x) => seg == "*" || seg == x,
        None => false,
    })
}

/// Access required for a request. Unknown GET needs any session; unknown mutations are denied.
pub fn policy(method: &str, path: &str) -> Access {
    let path = path.split('?').next().unwrap_or(path);
    if path.split('/').any(|s| s == "..") {
        return Access::Deny;
    }
    for (class, pat, access) in POLICY {
        if method_matches(class, method) && path_matches(pat, path) {
            return *access;
        }
    }
    if method == "GET" || method == "HEAD" {
        RO
    } else {
        Access::Deny
    }
}

// ---------------------------------------------------------------- Auth

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn cookie_value(headers: &[(String, String)], name: &str) -> Option<String> {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("cookie") {
            for part in v.split(';') {
                if let Some((n, val)) = part.trim().split_once('=') {
                    if n == name {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

fn bearer(headers: &[(String, String)]) -> Option<String> {
    let v = header(headers, "authorization")?;
    let (scheme, tok) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| tok.trim().to_string())
}

fn valid_username(u: &str) -> bool {
    !u.is_empty() && u.len() <= 64 && u.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

fn json_out(status: u16, extra: Vec<(String, String)>, v: serde_json::Value) -> HttpOut {
    let mut h = vec![("Content-Type".to_string(), "application/json".to_string()), ("Cache-Control".to_string(), "no-store".to_string())];
    h.extend(extra);
    (status, h, v.to_string())
}

fn err_out(status: u16, e: ApiError) -> HttpOut {
    json_out(status, vec![], serde_json::json!({ "error": e }))
}

fn err_auto(e: ApiError) -> HttpOut {
    err_out(e.status(), e)
}

impl Auth {
    pub fn load(root: &Path) -> Auth {
        Auth::load_with_iters(root, PBKDF2_ITERS)
    }

    /// Tests lower the cost factor; production uses `load`.
    pub(crate) fn load_with_iters(root: &Path, iters: u32) -> Auth {
        let path = root.join("vision/data/auth.json");
        let (persist, locked) = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<Persist>(&s) {
                Ok(p) => (p, false),
                Err(_) => (Persist::default(), true),
            },
            Err(_) => (Persist::default(), false),
        };
        let (secret, generated) = if persist.users.is_empty() && !locked {
            match std::env::var("CW_BOOTSTRAP_TOKEN").ok().filter(|t| t.len() >= 16) {
                Some(t) => (Some(t), false),
                None => (random(16).ok().map(|b| hex(&b)), true),
            }
        } else {
            (None, false)
        };
        Auth {
            path,
            required: std::env::var("CW_AUTH").map(|v| v == "required").unwrap_or(false),
            locked,
            secure_cookie: std::env::var("CW_COOKIE_SECURE").map(|v| v == "1").unwrap_or(false),
            iters,
            persist: Mutex::new(persist),
            sessions: Mutex::new(HashMap::new()),
            ephemeral_workers: Mutex::new(Vec::new()),
            bootstrap_secret: Mutex::new(secret),
            bootstrap_generated: generated,
            fails: Mutex::new(HashMap::new()),
            skew: AtomicU64::new(0),
        }
    }

    fn now(&self) -> u64 {
        let real = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        real + self.skew.load(Ordering::Relaxed)
    }

    /// Test hook: move this Auth's clock forward.
    #[cfg(test)]
    fn advance(&self, secs: u64) {
        self.skew.fetch_add(secs, Ordering::Relaxed);
    }

    fn open(&self) -> bool {
        // CW_AUTH=off: local development without sign-in, even when users exist. The head then
        // binds 127.0.0.1 (open mode), so nobody else on the network gets the anonymous admin.
        if std::env::var("CW_AUTH").is_ok_and(|v| v == "off") {
            return true;
        }
        !self.required && !self.locked && self.persist.lock().unwrap().users.is_empty()
    }

    /// "open-trusted-lan" when anyone who can reach the port is an administrator; else "enforced".
    /// Coordinator: in open mode bind 127.0.0.1 unless CW_BIND is explicitly set.
    pub fn mode(&self) -> &'static str {
        if self.open() { "open-trusted-lan" } else { "enforced" }
    }

    pub fn has_users(&self) -> bool {
        !self.persist.lock().unwrap().users.is_empty()
    }

    /// Generated bootstrap token to print once on stderr. None when users exist or the operator
    /// supplied CW_BOOTSTRAP_TOKEN (never echo an operator-supplied secret).
    pub fn bootstrap_token(&self) -> Option<String> {
        if self.bootstrap_generated { self.bootstrap_secret.lock().unwrap().clone() } else { None }
    }

    /// One-line startup message for the coordinator's `log()`.
    pub fn startup_notice(&self) -> String {
        if self.locked {
            return "auth: auth.json is unreadable; all protected routes are locked (fix or restore it)".into();
        }
        if self.has_users() {
            return if self.open() { "auth: off (CW_AUTH=off); no sign-in, loopback only".into() } else { "auth: enforced".into() };
        }
        let how = match self.bootstrap_token() {
            Some(t) => format!("bootstrap token {t} (shown once)"),
            None => "bootstrap token from CW_BOOTSTRAP_TOKEN".into(),
        };
        format!("auth: no administrator yet, mode {}; create one with POST /api/v1/auth/bootstrap, {how}", self.mode())
    }

    /// Token for the head's own local worker. Memory only, valid until the head exits.
    pub fn ephemeral_worker_token(&self) -> String {
        let t = format!("cwt_{}", hex(&random(32).unwrap_or_else(|_| vec![0; 32])));
        self.ephemeral_workers.lock().unwrap().push(sha256_hex(t.as_bytes()));
        t
    }

    fn save(&self, p: &Persist) -> Result<(), ApiError> {
        use std::io::Write;
        let io = |e: std::io::Error| ApiError::new("unavailable", format!("cannot write auth store: {e}"));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let body = serde_json::to_vec_pretty(p).map_err(|e| ApiError::new("internal", e.to_string()))?;
        o.open(&tmp).and_then(|mut f| f.write_all(&body).and_then(|_| f.sync_all())).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path).map_err(io)
    }

    fn add_user(&self, p: &mut Persist, username: &str, password: &str, role: Role) -> Result<(), ApiError> {
        if !valid_username(username) {
            return Err(ApiError::new("invalid_request", "username must be 1-64 chars of letters, digits, _ . -"));
        }
        if password.len() < MIN_PASSWORD || password.len() > 256 {
            return Err(ApiError::new("invalid_request", format!("password must be {MIN_PASSWORD}-256 characters")));
        }
        if p.users.iter().any(|u| u.username.eq_ignore_ascii_case(username)) {
            return Err(ApiError::new("conflict", "user already exists"));
        }
        let salt = random(16)?;
        p.users.push(User { username: username.into(), role, salt: hex(&salt), hash: hash_password(password, &salt, self.iters), iters: self.iters });
        Ok(())
    }

    /// Create the first administrator. Allowed exactly once, with the bootstrap token.
    pub fn bootstrap(&self, username: &str, password: &str, token: &str) -> Result<(), ApiError> {
        if self.locked {
            return Err(forbidden("auth store is unreadable"));
        }
        if self.limited("bootstrap") {
            return Err(ApiError::new("rate_limited", "too many bootstrap attempts"));
        }
        let mut p = self.persist.lock().unwrap();
        if !p.users.is_empty() {
            return Err(ApiError::new("conflict", "bootstrap already done"));
        }
        let ok = match self.bootstrap_secret.lock().unwrap().as_deref() {
            Some(s) => ct_eq(s.as_bytes(), token.as_bytes()),
            None => false,
        };
        if !ok {
            self.fail("bootstrap");
            return Err(forbidden("invalid bootstrap token"));
        }
        let mut next = p.clone();
        self.add_user(&mut next, username, password, Role::Administrator)?;
        self.save(&next)?;
        *p = next;
        *self.bootstrap_secret.lock().unwrap() = None;
        Ok(())
    }

    /// Administrator-only (the HTTP wrapper checks the principal).
    pub fn create_user(&self, username: &str, password: &str, role: Role) -> Result<(), ApiError> {
        let mut p = self.persist.lock().unwrap();
        let mut next = p.clone();
        self.add_user(&mut next, username, password, role)?;
        self.save(&next)?;
        *p = next;
        Ok(())
    }

    /// Returns the raw token once; only its sha256 is stored.
    pub fn create_worker_token(&self, name: &str) -> Result<String, ApiError> {
        if !valid_username(name) {
            return Err(ApiError::new("invalid_request", "worker name must be 1-64 chars of letters, digits, _ . -"));
        }
        let token = format!("cwt_{}", hex(&random(32)?));
        let mut p = self.persist.lock().unwrap();
        let mut next = p.clone();
        next.worker_tokens.push(WorkerToken { name: name.into(), sha256: sha256_hex(token.as_bytes()) });
        self.save(&next)?;
        *p = next;
        Ok(token)
    }

    fn worker_name(&self, token: &str) -> Option<String> {
        let h = sha256_hex(token.as_bytes());
        for e in self.ephemeral_workers.lock().unwrap().iter() {
            if ct_eq(e.as_bytes(), h.as_bytes()) {
                return Some("local".into());
            }
        }
        let p = self.persist.lock().unwrap();
        let mut found = None;
        for w in &p.worker_tokens {
            if ct_eq(w.sha256.as_bytes(), h.as_bytes()) {
                found = Some(w.name.clone());
            }
        }
        found
    }

    fn limited(&self, key: &str) -> bool {
        let now = self.now();
        let mut m = self.fails.lock().unwrap();
        match m.get_mut(key) {
            Some(q) => {
                while q.front().is_some_and(|t| now.saturating_sub(*t) > LOGIN_WINDOW_SECS) {
                    q.pop_front();
                }
                q.len() >= LOGIN_MAX_FAILS
            }
            None => false,
        }
    }

    fn fail(&self, key: &str) {
        let now = self.now();
        self.fails.lock().unwrap().entry(key.to_string()).or_default().push_back(now);
    }

    fn session_user(&self, headers: &[(String, String)]) -> Option<(String, Role)> {
        let token = cookie_value(headers, COOKIE)?;
        let key = sha256_hex(token.as_bytes());
        let now = self.now();
        let name = {
            let mut s = self.sessions.lock().unwrap();
            let sess = s.get_mut(&key)?;
            if now.saturating_sub(sess.last) > IDLE_SECS || now.saturating_sub(sess.created) > MAX_SECS {
                s.remove(&key);
                return None;
            }
            sess.last = now;
            sess.user.clone()
        };
        // Role is read live so a role change or removal applies to existing sessions.
        let p = self.persist.lock().unwrap();
        let u = p.users.iter().find(|u| u.username == name)?;
        Some((u.username.clone(), u.role))
    }

    /// Decide whether this request may proceed. Call before any other handling, including the
    /// `/api/live` and `/api/media` fast paths.
    pub fn check(&self, method: &str, path: &str, headers: &[(String, String)]) -> Result<Principal, ApiError> {
        let mode = self.mode().to_string();
        let mk = |name: &str, role: Role, anonymous: bool, worker: bool| Principal { name: name.into(), role, anonymous, mode: mode.clone(), worker };
        let worker = bearer(headers).and_then(|t| self.worker_name(&t));
        match policy(method, path) {
            Access::Deny => Err(forbidden("route is not in the access policy")),
            Access::Public => Ok(mk("public", Role::ReadOnly, true, false)),
            Access::Worker => {
                if let Some(n) = worker {
                    return Ok(mk(&format!("worker:{n}"), Role::ReadOnly, false, true));
                }
                if self.open() {
                    return Ok(mk("open-worker", Role::ReadOnly, true, true));
                }
                if cookie_value(headers, COOKIE).is_some() {
                    return Err(forbidden("a user session cannot act as a worker"));
                }
                Err(unauthorized("worker credential required"))
            }
            Access::Min(need) => {
                if worker.is_some() {
                    return Err(forbidden("a worker credential cannot act as a user"));
                }
                if self.open() {
                    return Ok(mk("anonymous", Role::Administrator, true, false));
                }
                let (name, role) = self.session_user(headers).ok_or_else(|| unauthorized("login required"))?;
                if !matches!(method, "GET" | "HEAD") {
                    // CSRF belt and braces on top of SameSite=Strict.
                    if let (Some(origin), Some(host)) = (header(headers, "origin"), header(headers, "host")) {
                        let o = origin.split("://").nth(1).unwrap_or(origin);
                        if !o.eq_ignore_ascii_case(host) {
                            return Err(forbidden("cross-origin request refused"));
                        }
                    }
                }
                if role < need {
                    return Err(forbidden(&format!("requires role {}", need.name())));
                }
                Ok(mk(&name, role, false, false))
            }
        }
    }

    // ------------------------------------------------------------ pure HTTP handlers

    fn cookie_header(&self, token: &str, max_age: u64) -> (String, String) {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        ("Set-Cookie".into(), format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}"))
    }

    /// `ip` is the peer address used for rate limiting.
    pub fn login(&self, body: &[u8], ip: &str) -> HttpOut {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return err_out(400, ApiError::new("parse_error", "body must be JSON"));
        };
        let (Some(user), Some(pass)) = (v["username"].as_str(), v["password"].as_str()) else {
            return err_out(400, ApiError::new("invalid_request", "username and password required"));
        };
        let keys = [format!("user:{}", user.to_lowercase()), format!("ip:{ip}")];
        if keys.iter().any(|k| self.limited(k)) {
            let mut e = ApiError::new("rate_limited", "too many failed logins; wait a few minutes");
            e.retryable = true;
            return err_out(429, e);
        }
        let found = self.persist.lock().unwrap().users.iter().find(|u| u.username.eq_ignore_ascii_case(user)).cloned();
        let ok = match &found {
            Some(u) => {
                let salt = (0..u.salt.len() / 2).map(|i| u8::from_str_radix(&u.salt[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect::<Vec<_>>();
                ct_eq(hash_password(pass, &salt, u.iters).as_bytes(), u.hash.as_bytes())
            }
            None => {
                // Spend the same work so timing does not reveal which usernames exist.
                let _ = hash_password(pass, &[0u8; 16], self.iters);
                false
            }
        };
        let Some(u) = found.filter(|_| ok) else {
            for k in &keys {
                self.fail(k);
            }
            return err_out(401, unauthorized("invalid username or password"));
        };
        self.fails.lock().unwrap().remove(&keys[0]);
        let Ok(raw) = random(32) else { return err_out(503, ApiError::new("unavailable", "no system randomness")) };
        let token = hex(&raw);
        let now = self.now();
        {
            let mut s = self.sessions.lock().unwrap();
            s.retain(|_, x| now.saturating_sub(x.last) <= IDLE_SECS && now.saturating_sub(x.created) <= MAX_SECS);
            s.insert(sha256_hex(token.as_bytes()), Session { user: u.username.clone(), created: now, last: now });
        }
        json_out(200, vec![self.cookie_header(&token, IDLE_SECS)], serde_json::json!({ "user": { "name": u.username, "role": u.role.name() } }))
    }

    pub fn logout(&self, headers: &[(String, String)]) -> HttpOut {
        if let Some(t) = cookie_value(headers, COOKIE) {
            self.sessions.lock().unwrap().remove(&sha256_hex(t.as_bytes()));
        }
        json_out(200, vec![self.cookie_header("", 0)], serde_json::json!({ "ok": true }))
    }

    pub fn bootstrap_http(&self, body: &[u8]) -> HttpOut {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return err_out(400, ApiError::new("parse_error", "body must be JSON"));
        };
        let (Some(u), Some(p), Some(t)) = (v["username"].as_str(), v["password"].as_str(), v["bootstrap_token"].as_str()) else {
            return err_out(400, ApiError::new("invalid_request", "username, password and bootstrap_token required"));
        };
        match self.bootstrap(u, p, t) {
            Ok(()) => json_out(200, vec![], serde_json::json!({ "ok": true, "user": { "name": u, "role": "administrator" } })),
            Err(e) if e.code == "rate_limited" => err_out(429, e),
            Err(e) => err_auto(e),
        }
    }

    pub fn me(&self, headers: &[(String, String)]) -> HttpOut {
        match self.check("GET", "/api/v1/auth/me", headers) {
            Ok(p) => json_out(200, vec![], serde_json::json!({ "principal": { "name": p.name, "role": p.role.name(), "anonymous": p.anonymous, "mode": p.mode } })),
            Err(e) => err_auto(e),
        }
    }

    pub fn create_user_http(&self, headers: &[(String, String)], body: &[u8]) -> HttpOut {
        if let Err(e) = self.check("POST", "/api/v1/auth/users", headers) {
            return err_auto(e);
        }
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let (Some(u), Some(p), Some(r)) = (v["username"].as_str(), v["password"].as_str(), v["role"].as_str().and_then(Role::parse)) else {
            return err_out(400, ApiError::new("invalid_request", "username, password and role required"));
        };
        match self.create_user(u, p, r) {
            Ok(()) => json_out(200, vec![], serde_json::json!({ "ok": true, "user": { "name": u, "role": r.name() } })),
            Err(e) => err_auto(e),
        }
    }

    /// Signed-in user changes their own password; requires the current one. Other sessions stay valid.
    pub fn change_password_http(&self, headers: &[(String, String)], body: &[u8]) -> HttpOut {
        let p = match self.check("POST", "/api/v1/auth/password", headers) {
            Ok(p) if !p.anonymous && !p.worker => p,
            Ok(_) => return err_out(403, forbidden("sign in as a user to change a password")),
            Err(e) => return err_auto(e),
        };
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let (Some(cur), Some(new)) = (v["current"].as_str(), v["new"].as_str()) else {
            return err_out(400, ApiError::new("invalid_request", "current and new required"));
        };
        if new.len() < MIN_PASSWORD || new.len() > 256 {
            return err_out(400, ApiError::new("invalid_request", format!("password must be {MIN_PASSWORD}-256 characters")));
        }
        let key = format!("user:{}", p.name.to_lowercase());
        if self.limited(&key) {
            return err_out(429, ApiError::new("rate_limited", "too many failed attempts; wait a few minutes"));
        }
        let mut guard = self.persist.lock().unwrap();
        let mut next = guard.clone();
        let Some(u) = next.users.iter_mut().find(|u| u.username == p.name) else { return err_out(404, ApiError::new("not_found", "user not found")) };
        let salt = (0..u.salt.len() / 2).map(|i| u8::from_str_radix(&u.salt[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect::<Vec<_>>();
        if !ct_eq(hash_password(cur, &salt, u.iters).as_bytes(), u.hash.as_bytes()) {
            self.fail(&key);
            return err_out(401, unauthorized("current password is wrong"));
        }
        let Ok(fresh) = random(16) else { return err_out(503, ApiError::new("unavailable", "no system randomness")) };
        u.salt = hex(&fresh);
        u.hash = hash_password(new, &fresh, self.iters);
        u.iters = self.iters;
        if let Err(e) = self.save(&next) {
            return err_auto(e);
        }
        *guard = next;
        json_out(200, vec![], serde_json::json!({ "ok": true }))
    }

    pub fn create_worker_token_http(&self, headers: &[(String, String)], body: &[u8]) -> HttpOut {
        if let Err(e) = self.check("POST", "/api/v1/auth/worker-tokens", headers) {
            return err_auto(e);
        }
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let Some(name) = v["name"].as_str() else { return err_out(400, ApiError::new("invalid_request", "name required")) };
        match self.create_worker_token(name) {
            Ok(t) => json_out(200, vec![], serde_json::json!({ "name": name, "token": t, "note": "shown once; only a hash is stored" })),
            Err(e) => err_auto(e),
        }
    }
}

// ---------------------------------------------------------------- redaction, media roots, audit

const SECRET_KEYS: &[&str] = &["password", "passwd", "pwd", "pass", "secret", "token", "access_token", "api_key", "apikey", "key", "auth", "signature", "sig", "credential"];

/// Strip userinfo from URLs and mask secret-looking query values.
pub fn redact(url: &str) -> String {
    let (rest, frag) = match url.split_once('#') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let (main, query) = match rest.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let mut out = match main.find("://") {
        Some(i) => {
            let (scheme, tail) = main.split_at(i + 3);
            let end = tail.find('/').unwrap_or(tail.len());
            let authority = &tail[..end];
            let host = authority.rfind('@').map(|j| &authority[j + 1..]).unwrap_or(authority);
            format!("{scheme}{host}{}", &tail[end..])
        }
        None => main.to_string(),
    };
    if let Some(q) = query {
        let parts: Vec<String> = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) if SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str()) => format!("{k}=***"),
                _ => kv.to_string(),
            })
            .collect();
        out.push('?');
        out.push_str(&parts.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Redact free text: every URL-looking word and any `Bearer <token>`.
pub fn redact_text(s: &str) -> String {
    let mut prev_bearer = false;
    s.split_whitespace()
        .map(|w| {
            let r = if prev_bearer { "***".to_string() } else if w.contains("://") { redact(w) } else { w.to_string() };
            prev_bearer = w.eq_ignore_ascii_case("bearer");
            r
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Approved media roots: `<root>/vision/footage` plus any in CW_MEDIA_ROOTS (path list).
pub fn allowed_media(root: &Path, path: &Path) -> bool {
    let extra: Vec<PathBuf> = std::env::var_os("CW_MEDIA_ROOTS").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    allowed_media_with(root, path, &extra)
}

pub fn allowed_media_with(root: &Path, path: &Path, extra: &[PathBuf]) -> bool {
    use std::path::Component;
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let full = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    let Ok(real) = full.canonicalize() else { return false };
    let mut roots = vec![root.join("vision/footage")];
    roots.extend(extra.iter().cloned());
    roots.iter().filter_map(|r| r.canonicalize().ok()).any(|r| real.starts_with(&r))
}

/// Build an audit record with credentials removed from every free-text field.
pub fn audit_record(principal: &Principal, action: &str, targets: &[&str], result: &str, detail: &str) -> AuditRecord {
    let utc_us = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0);
    let nonce = hex(&random(6).unwrap_or_else(|_| vec![0; 6]));
    AuditRecord {
        id: format!("aud_{utc_us}_{nonce}"),
        utc_us,
        actor: if principal.anonymous { format!("{} ({})", principal.name, principal.mode) } else { principal.name.clone() },
        action: action.into(),
        targets: targets.iter().map(|t| redact(t)).collect(),
        result: result.into(),
        correlation_id: None,
        detail: redact_text(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cw-auth-{tag}-{}-{}", std::process::id(), hex(&random(4).unwrap())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }
    const PW: &str = "correct horse battery";

    /// Auth with a bootstrapped admin (fast hashes for tests).
    fn with_admin(tag: &str) -> (Auth, PathBuf) {
        let root = tmp(tag);
        let a = Auth::load_with_iters(&root, 1000);
        let t = a.bootstrap_token().unwrap();
        a.bootstrap("admin", PW, &t).unwrap();
        (a, root)
    }
    fn cookie_of(out: &HttpOut) -> Vec<(String, String)> {
        let sc = &out.1.iter().find(|(k, _)| k == "Set-Cookie").unwrap().1;
        h(&[("Cookie", sc.split(';').next().unwrap())])
    }
    fn login_as(a: &Auth, u: &str) -> Vec<(String, String)> {
        let out = a.login(format!(r#"{{"username":"{u}","password":"{PW}"}}"#).as_bytes(), "10.0.0.1");
        assert_eq!(out.0, 200, "{}", out.2);
        cookie_of(&out)
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        assert_eq!(hex(&hmac_sha256(&[0x0b; 20], b"Hi There")), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // key longer than the block size (RFC 4231 case 6)
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn pbkdf2_published_vectors() {
        let mut o = [0u8; 32];
        pbkdf2_hmac_sha256(b"password", b"salt", 1, &mut o);
        assert_eq!(hex(&o), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
        pbkdf2_hmac_sha256(b"password", b"salt", 2, &mut o);
        assert_eq!(hex(&o), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
        pbkdf2_hmac_sha256(b"password", b"salt", 4096, &mut o);
        assert_eq!(hex(&o), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
        // multi-block output (dkLen 40): first block equals the 32-byte result
        let mut o40 = [0u8; 40];
        pbkdf2_hmac_sha256(b"passwordPASSWORDpassword", b"saltSALTsaltSALTsaltSALTsaltSALTsalt", 4096, &mut o40);
        assert_eq!(hex(&o40), "348c89dbcbd32b2f32d814b8116e84cf2b17347ebc1800181c4e2a1fb8dd53e1c635518c7dac47e9");
    }

    #[test]
    fn production_cost_and_hash_verify() {
        let root = tmp("prod");
        let a = Auth::load(&root);
        assert!(PBKDF2_ITERS >= 210_000 && a.iters >= 210_000);
        let t = a.bootstrap_token().unwrap();
        a.bootstrap("root", PW, &t).unwrap();
        let stored = std::fs::read_to_string(root.join("vision/data/auth.json")).unwrap();
        assert!(!stored.contains(PW));
        let v: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(v["users"][0]["iters"].as_u64().unwrap() >= 210_000);
        assert_eq!(v["users"][0]["hash"].as_str().unwrap().len(), 64);
        assert_eq!(v["users"][0]["salt"].as_str().unwrap().len(), 32);
    }

    #[test]
    fn auth_file_is_0600_and_tokens_hashed() {
        let (a, root) = with_admin("perm");
        let raw = a.create_worker_token("pc1").unwrap();
        let path = root.join("vision/data/auth.json");
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(!s.contains(&raw) && s.contains(&sha256_hex(raw.as_bytes())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // reload from disk: user and token survive, sessions do not
        let b = Auth::load_with_iters(&root, 1000);
        assert!(b.has_users() && b.bootstrap_token().is_none());
        assert!(b.check("POST", "/api/index", &h(&[("Authorization", &format!("Bearer {raw}"))])).unwrap().worker);
    }

    #[test]
    fn bootstrap_only_once_and_needs_token() {
        let root = tmp("boot");
        let a = Auth::load_with_iters(&root, 1000);
        assert_eq!(a.mode(), "open-trusted-lan");
        let t = a.bootstrap_token().unwrap();
        assert_eq!(a.bootstrap("admin", PW, "wrong-token-wrong-token").unwrap_err().code, "forbidden");
        assert_eq!(a.bootstrap("admin", "short", &t).unwrap_err().code, "invalid_request");
        a.bootstrap("admin", PW, &t).unwrap();
        assert_eq!(a.mode(), "enforced");
        assert_eq!(a.bootstrap("admin2", PW, &t).unwrap_err().code, "conflict");
        let out = a.bootstrap_http(format!(r#"{{"username":"x","password":"{PW}","bootstrap_token":"{t}"}}"#).as_bytes());
        assert_eq!(out.0, 409);
        assert!(out.2.contains(r#""error""#));
    }

    #[test]
    fn open_mode_is_anonymous_admin_and_required_mode_is_locked() {
        let root = tmp("open");
        let a = Auth::load_with_iters(&root, 1000);
        let p = a.check("POST", "/api/rules", &[]).unwrap();
        assert!(p.anonymous && p.role == Role::Administrator && p.mode == "open-trusted-lan");
        // a corrupt auth store must fail closed, not fall back to open mode
        let root2 = tmp("corrupt");
        std::fs::create_dir_all(root2.join("vision/data")).unwrap();
        std::fs::write(root2.join("vision/data/auth.json"), "{not json").unwrap();
        let b = Auth::load_with_iters(&root2, 1000);
        assert_eq!(b.mode(), "enforced");
        assert_eq!(b.check("GET", "/api/state", &[]).unwrap_err().code, "unauthorized");
    }

    #[test]
    fn session_cookie_flags_expiry_and_logout() {
        let (a, _r) = with_admin("sess");
        let out = a.login(format!(r#"{{"username":"admin","password":"{PW}"}}"#).as_bytes(), "1.1.1.1");
        let sc = out.1.iter().find(|(k, _)| k == "Set-Cookie").unwrap().1.clone();
        assert!(sc.starts_with("cw_session=") && sc.contains("HttpOnly") && sc.contains("SameSite=Strict"));
        let c = cookie_of(&out);
        assert_eq!(a.check("GET", "/api/state", &c).unwrap().name, "admin");
        // sliding: activity at 11h keeps the session alive past 12h of wall time
        a.advance(11 * 3600);
        assert!(a.check("GET", "/api/state", &c).is_ok());
        a.advance(11 * 3600);
        assert!(a.check("GET", "/api/state", &c).is_ok());
        // idle past 12h expires
        a.advance(12 * 3600 + 1);
        assert_eq!(a.check("GET", "/api/state", &c).unwrap_err().code, "unauthorized");
        // logout invalidates
        let c2 = login_as(&a, "admin");
        assert!(a.check("GET", "/api/state", &c2).is_ok());
        let lo = a.logout(&c2);
        assert!(lo.1.iter().any(|(k, v)| k == "Set-Cookie" && v.contains("Max-Age=0")));
        assert_eq!(a.check("GET", "/api/state", &c2).unwrap_err().code, "unauthorized");
        // sessions are memory only: a fresh Auth does not know the old cookie
        let (b, _r2) = with_admin("sess2");
        assert!(b.check("GET", "/api/state", &c2).is_err());
    }

    #[test]
    fn absolute_session_lifetime() {
        let (a, _r) = with_admin("abs");
        let c = login_as(&a, "admin");
        for _ in 0..16 {
            a.advance(11 * 3600);
            let _ = a.check("GET", "/api/state", &c);
        }
        assert!(a.check("GET", "/api/state", &c).is_err());
    }

    #[test]
    fn role_matrix() {
        let (a, _r) = with_admin("roles");
        for (u, r) in [("op", Role::Operator), ("rev", Role::Reviewer), ("ro", Role::ReadOnly)] {
            a.create_user(u, PW, r).unwrap();
        }
        let ck = |u: &str| login_as(&a, u);
        let (adm, op, rev, ro) = (ck("admin"), ck("op"), ck("rev"), ck("ro"));
        let code = |m: &str, p: &str, c: &[(String, String)]| a.check(m, p, c).err().map(|e| e.code);
        // sources mutation: operator+ only
        for p in ["/api/sources", "/api/v1/sources"] {
            assert_eq!(code("POST", p, &rev), Some("forbidden".into()));
            assert_eq!(code("POST", p, &ro), Some("forbidden".into()));
            assert_eq!(code("POST", p, &op), None);
        }
        // source credentials: administrator only
        assert_eq!(code("PATCH", "/api/v1/sources/src1/credentials", &op), Some("forbidden".into()));
        assert_eq!(code("PATCH", "/api/v1/sources/src1/credentials", &adm), None);
        // footage: reviewer+, not read_only
        for p in ["/api/media/cam1", "/api/live/cam1", "/api/hit/3", "/api/v1/media/m1/stream", "/api/v1/media/m1/frame"] {
            assert_eq!(code("GET", p, &ro), Some("forbidden".into()), "{p}");
            assert_eq!(code("GET", p, &rev), None, "{p}");
        }
        // audit: operator+ (read_only and reviewer cannot read it)
        for p in ["/api/logs", "/api/v1/audit"] {
            assert_eq!(code("GET", p, &ro), Some("forbidden".into()));
            assert_eq!(code("GET", p, &rev), Some("forbidden".into()));
            assert_eq!(code("GET", p, &op), None);
        }
        // reviews/decisions: reviewer+; read_only may list
        assert_eq!(code("POST", "/api/v1/reviews", &ro), Some("forbidden".into()));
        assert_eq!(code("POST", "/api/v1/reviews", &rev), None);
        assert_eq!(code("GET", "/api/v1/reviews", &ro), None);
        assert_eq!(code("POST", "/api/confirm", &rev), None);
        assert_eq!(code("POST", "/api/v1/alerts/a1/decision", &ro), Some("forbidden".into()));
        // config mutation: operator+
        for p in ["/api/rules", "/api/modules", "/api/settings", "/api/searches"] {
            assert_eq!(code("POST", p, &rev), Some("forbidden".into()), "{p}");
            assert_eq!(code("POST", p, &op), None, "{p}");
        }
        // user management: administrator only
        assert_eq!(a.create_user_http(&op, b"{}").0, 403);
        assert_eq!(a.create_user_http(&adm, format!(r#"{{"username":"n","password":"{PW}","role":"reviewer"}}"#).as_bytes()).0, 200);
        // anonymous
        assert_eq!(code("GET", "/api/state", &[]), Some("unauthorized".into()));
        assert_eq!(code("GET", "/api/media/x", &[]), Some("unauthorized".into()));
        // public shell and health
        assert_eq!(code("GET", "/", &[]), None);
        assert_eq!(code("GET", "/ui/app.js", &[]), None);
        assert_eq!(code("GET", "/api/health", &[]), None);
        assert_eq!(code("GET", "/review", &[]), None);
    }

    #[test]
    fn join_routes_follow_the_cluster_policy() {
        let (a, _r) = with_admin("join");
        a.create_user("op", PW, Role::Operator).unwrap();
        let tok = a.create_worker_token("pc3").unwrap();
        let (adm, op) = (login_as(&a, "admin"), login_as(&a, "op"));
        let w = h(&[("Authorization", &format!("Bearer {tok}"))]);
        let code = |m: &str, p: &str, c: &[(String, String)]| a.check(m, p, c).err().map(|e| e.code);
        for p in ["/join", "/api/v1/join/info", "/api/v1/join/install.sh", "/api/v1/join/install.ps1", "/api/v1/join/bundle/macos-aarch64.tar.gz", "/api/v1/join/source.tar.gz"] {
            assert_eq!(code("GET", p, &[]), None, "{p} is public");
        }
        assert_eq!(code("POST", "/api/v1/join/enroll", &[]), None, "enroll is public; the handler wants the code");
        assert_eq!(code("POST", "/api/v1/join/codes", &[]), Some("unauthorized".into()));
        assert_eq!(code("POST", "/api/v1/join/codes", &op), Some("forbidden".into()), "only an administrator creates codes");
        assert_eq!(code("POST", "/api/v1/join/codes", &adm), None);
        assert_eq!(code("POST", "/api/v1/join/codes", &w), Some("forbidden".into()), "a worker cannot mint codes");
        for (m, p) in [("GET", "/api/v1/join/manifest"), ("GET", "/api/v1/join/file"), ("GET", "/api/v1/worker/media/m1/stream"), ("POST", "/api/v1/heartbeat")] {
            assert_eq!(code(m, p, &w), None, "{p}");
            assert_eq!(code(m, p, &[]), Some("unauthorized".into()), "{p} needs the worker token");
            assert_eq!(code(m, p, &adm), Some("forbidden".into()), "{p} is not for sessions");
        }
        assert_eq!(code("POST", "/api/v1/benchmarks", &op), None);
        assert_eq!(code("DELETE", "/api/v1/benchmarks/b1", &op), None);
        assert_eq!(code("GET", "/api/v1/cluster/realtime", &op), None);
    }

    #[test]
    fn worker_and_session_are_disjoint() {
        let (a, _r) = with_admin("disjoint");
        let tok = a.create_worker_token("pc2").unwrap();
        let w = h(&[("Authorization", &format!("Bearer {tok}"))]);
        for r in [("POST", "/api/index"), ("POST", "/api/v1/index"), ("GET", "/api/work"), ("POST", "/api/stats"), ("POST", "/api/events"), ("POST", "/api/preview/cam1")] {
            let p = a.check(r.0, r.1, &w).unwrap();
            assert!(p.worker && p.name == "worker:pc2", "{r:?}");
        }
        // a worker token can never reach media, admin, or any user route
        for (m, p) in [("GET", "/api/v1/media/m1"), ("GET", "/api/media/x"), ("POST", "/api/sources"), ("GET", "/api/v1/audit"), ("POST", "/api/v1/auth/users"), ("GET", "/api/state"), ("GET", "/api/v1/auth/me")] {
            assert_eq!(a.check(m, p, &w).unwrap_err().code, "forbidden", "{p}");
        }
        // a session cannot post an index batch or poll for work
        let adm = login_as(&a, "admin");
        assert_eq!(a.check("POST", "/api/v1/index", &adm).unwrap_err().code, "forbidden");
        assert_eq!(a.check("GET", "/api/work", &adm).unwrap_err().code, "forbidden");
        // no credential at all
        assert_eq!(a.check("POST", "/api/index", &[]).unwrap_err().code, "unauthorized");
        // wrong token
        assert_eq!(a.check("POST", "/api/index", &h(&[("Authorization", "Bearer cwt_nope")])).unwrap_err().code, "unauthorized");
        // head's own local worker token is memory-only and works the same way
        let local = a.ephemeral_worker_token();
        assert!(a.check("GET", "/api/work", &h(&[("Authorization", &format!("Bearer {local}"))])).unwrap().worker);
    }

    #[test]
    fn unknown_mutations_denied_unknown_reads_need_session() {
        let (a, _r) = with_admin("unknown");
        let adm = login_as(&a, "admin");
        assert_eq!(a.check("POST", "/api/v1/brand-new", &adm).unwrap_err().code, "forbidden");
        assert_eq!(a.check("DELETE", "/api/unknown", &adm).unwrap_err().code, "forbidden");
        assert_eq!(a.check("GET", "/api/media/../../etc/passwd", &adm).unwrap_err().code, "forbidden");
        assert_eq!(a.check("GET", "/api/brand-new", &[]).unwrap_err().code, "unauthorized");
        assert!(a.check("GET", "/api/brand-new", &adm).is_ok());
    }

    #[test]
    fn cross_origin_mutation_refused() {
        let (a, _r) = with_admin("csrf");
        let mut c = login_as(&a, "admin");
        c.push(("Host".into(), "head:7710".into()));
        c.push(("Origin".into(), "http://evil.example".into()));
        assert_eq!(a.check("POST", "/api/rules", &c).unwrap_err().code, "forbidden");
        c.pop();
        c.push(("Origin".into(), "http://head:7710".into()));
        assert!(a.check("POST", "/api/rules", &c).is_ok());
    }

    #[test]
    fn change_password_needs_current_and_takes_effect() {
        let (a, _d) = with_admin("pwchange");
        let c = login_as(&a, "admin");
        assert_eq!(a.change_password_http(&c, br#"{"current":"wrong wrong wrong","new":"another long pass 1"}"#).0, 401);
        assert_eq!(a.change_password_http(&c, format!(r#"{{"current":"{PW}","new":"another long pass 1"}}"#).as_bytes()).0, 200);
        assert_eq!(a.login(format!(r#"{{"username":"admin","password":"{PW}"}}"#).as_bytes(), "2.2.2.2").0, 401);
        assert_eq!(a.login(br#"{"username":"admin","password":"another long pass 1"}"#, "2.2.2.2").0, 200);
        assert_eq!(a.change_password_http(&[], br#"{"current":"x","new":"y"}"#).0 / 100, 4, "no session, no change");
    }

    #[test]
    fn login_rate_limit() {
        let (a, _r) = with_admin("rate");
        for _ in 0..LOGIN_MAX_FAILS {
            assert_eq!(a.login(br#"{"username":"admin","password":"nope nope nope"}"#, "9.9.9.9").0, 401);
        }
        // even the right password is refused while locked out, for the user and for the IP
        let good = format!(r#"{{"username":"admin","password":"{PW}"}}"#);
        let out = a.login(good.as_bytes(), "8.8.8.8");
        assert_eq!(out.0, 429);
        assert!(out.2.contains("rate_limited"));
        assert_eq!(a.login(br#"{"username":"other","password":"x"}"#, "9.9.9.9").0, 429);
        // window passes
        a.advance(LOGIN_WINDOW_SECS + 1);
        assert_eq!(a.login(good.as_bytes(), "8.8.8.8").0, 200);
        // unknown users are also counted
        for _ in 0..LOGIN_MAX_FAILS {
            a.login(br#"{"username":"ghost","password":"nope nope nope"}"#, "7.7.7.7");
        }
        assert_eq!(a.login(br#"{"username":"ghost","password":"x"}"#, "6.6.6.6").0, 429);
        assert_eq!(a.login(b"not json", "5.5.5.5").0, 400);
    }

    #[test]
    fn redact_cases() {
        assert_eq!(redact("rtsp://user:pass@host:554/x"), "rtsp://host:554/x");
        assert_eq!(redact("rtsp://user:p@ss@host/x"), "rtsp://host/x");
        assert_eq!(redact("http://h/a?password=hunter2&x=1"), "http://h/a?password=***&x=1");
        assert_eq!(redact("rtsp://u:p@h/s?token=abc&Passwd=z#frag"), "rtsp://h/s?token=***&Passwd=***#frag");
        assert_eq!(redact("rtsp://host/path"), "rtsp://host/path");
        assert_eq!(redact("/vision/footage/a.mp4"), "/vision/footage/a.mp4");
        let t = redact_text("open rtsp://admin:secret@cam/1 with Bearer abc123 ok");
        assert!(!t.contains("secret") && !t.contains("abc123") && t.contains("rtsp://cam/1"));
    }

    #[test]
    fn allowed_media_rejects_traversal_and_symlinks() {
        let root = tmp("media");
        let foot = root.join("vision/footage");
        std::fs::create_dir_all(&foot).unwrap();
        std::fs::write(foot.join("a.mp4"), b"x").unwrap();
        let outside = tmp("outside");
        std::fs::write(outside.join("secret.mp4"), b"x").unwrap();
        assert!(allowed_media_with(&root, &foot.join("a.mp4"), &[]));
        assert!(allowed_media_with(&root, Path::new("vision/footage/a.mp4"), &[]));
        assert!(!allowed_media_with(&root, &foot.join("../../../etc/passwd"), &[]));
        assert!(!allowed_media_with(&root, &foot.join("missing.mp4"), &[]));
        assert!(!allowed_media_with(&root, &outside.join("secret.mp4"), &[]));
        assert!(allowed_media_with(&root, &outside.join("secret.mp4"), &[outside.clone()]));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("secret.mp4"), foot.join("link.mp4")).unwrap();
            std::os::unix::fs::symlink(&outside, foot.join("dir")).unwrap();
            assert!(!allowed_media_with(&root, &foot.join("link.mp4"), &[]));
            assert!(!allowed_media_with(&root, &foot.join("dir/secret.mp4"), &[]));
        }
    }

    #[test]
    fn audit_record_is_redacted() {
        let p = Principal { name: "op".into(), role: Role::Operator, anonymous: false, mode: "enforced".into(), worker: false };
        let r = audit_record(&p, "source.add", &["rtsp://u:p@cam/1"], "ok", "added rtsp://u:p@cam/1?password=zz");
        assert_eq!(r.actor, "op");
        assert!(!r.detail.contains(":p@") && !r.detail.contains("zz") && !r.targets[0].contains("u:p"));
        let anon = Principal { name: "anonymous".into(), role: Role::Administrator, anonymous: true, mode: "open-trusted-lan".into(), worker: false };
        assert!(audit_record(&anon, "x", &[], "ok", "").actor.contains("open-trusted-lan"));
    }

    #[test]
    fn startup_notice_hides_nothing_it_should_not() {
        let root = tmp("notice");
        let a = Auth::load_with_iters(&root, 1000);
        let n = a.startup_notice();
        assert!(n.contains(&a.bootstrap_token().unwrap()) && n.contains("open-trusted-lan"));
        let t = a.bootstrap_token().unwrap();
        a.bootstrap("admin", PW, &t).unwrap();
        assert_eq!(a.startup_notice(), "auth: enforced");
    }
}
