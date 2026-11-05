//! Users, passwords, sessions, roles, API tokens (SEC-01). PBKDF2 record: `pbkdf2$iters$salt_hex$hash_hex`.
//! Roles, lowest to highest: viewer < researcher < operator < admin. HMAC comes from crate::auth.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::auth::{ct_eq, hmac_sha256, nonce};
use crate::jobs::now_ms;
use crate::store::Store;

pub const ITERS: u32 = 210_000;
fn idle_ms() -> i64 {
    (crate::tune::get("session_idle_h") * 3_600_000.0) as i64
}
fn max_ms() -> i64 {
    (crate::tune::get("session_max_d") * 86_400_000.0) as i64
}
fn fail_window_ms() -> i64 {
    (crate::tune::get("login_fail_window_min") * 60_000.0) as i64
}
fn fail_max() -> usize {
    crate::tune::n("login_fail_max") as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Researcher,
    Operator,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "viewer" => Role::Viewer,
            "researcher" => Role::Researcher,
            "operator" => Role::Operator,
            "admin" => Role::Admin,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Researcher => "researcher",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then_some(())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// RFC 8018 PBKDF2-HMAC-SHA256, one 32-byte block.
fn pbkdf2(password: &[u8], salt: &[u8], iters: u32) -> [u8; 32] {
    let mut s = salt.to_vec();
    s.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &s);
    let mut t = u;
    for _ in 1..iters {
        u = hmac_sha256(password, &u);
        t.iter_mut().zip(u).for_each(|(a, b)| *a ^= b);
    }
    t
}

pub fn hash_password(password: &str, iters: u32) -> String {
    let salt = &nonce()[..16];
    format!("pbkdf2${iters}${}${}", hex(salt), hex(&pbkdf2(password.as_bytes(), salt, iters)))
}

pub fn verify_password(password: &str, record: &str) -> bool {
    let p: Vec<&str> = record.split('$').collect();
    let (Some(iters), Some(salt), Some(want)) = (p.get(1).and_then(|s| s.parse::<u32>().ok()), p.get(2).and_then(|s| unhex(s)), p.get(3)) else {
        return false;
    };
    p[0] == "pbkdf2" && iters > 0 && ct_eq(hex(&pbkdf2(password.as_bytes(), &salt, iters)).as_bytes(), want.as_bytes())
}

/// Spent on unknown names so a missing user costs the same as a wrong password.
fn dummy_record() -> &'static str {
    static D: OnceLock<String> = OnceLock::new();
    D.get_or_init(|| hash_password("dummy", ITERS))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Who {
    pub name: String,
    pub role: Role,
}

pub enum LoginErr {
    Bad,
    Throttled,
}

struct Sess {
    user: String,
    created: i64,
    last: i64,
}

pub struct Auth {
    pub open: bool,
    sessions: Mutex<HashMap<String, Sess>>,
    fails: Mutex<HashMap<String, Vec<i64>>>,
    /// Serialises "no users yet" check + create in bootstrap.
    pub bootstrap_lock: Mutex<()>,
}

impl Auth {
    pub fn new(open: bool) -> Auth {
        Auth { open, sessions: Default::default(), fails: Default::default(), bootstrap_lock: Default::default() }
    }
    pub fn from_env() -> Auth {
        Auth::new(std::env::var("CK_AUTH").is_ok_and(|v| v == "off"))
    }

    /// Who is calling: open mode = anonymous admin; else a live session cookie, else a bearer token.
    pub fn who(&self, store: &Store, cookie: Option<&str>, bearer: Option<&str>) -> Option<Who> {
        self.who_at(store, cookie, bearer, now_ms())
    }

    pub fn who_at(&self, store: &Store, cookie: Option<&str>, bearer: Option<&str>, now: i64) -> Option<Who> {
        if self.open {
            return Some(Who { name: "anonymous".into(), role: Role::Admin });
        }
        let name = if let Some(sid) = cookie {
            let mut s = self.sessions.lock().unwrap();
            let live = s.get_mut(sid).filter(|x| now - x.last <= idle_ms() && now - x.created <= max_ms()).map(|x| {
                x.last = now;
                x.user.clone()
            });
            if live.is_none() {
                s.remove(sid);
            }
            live
        } else {
            None
        };
        let name = name.or_else(|| bearer.and_then(|t| store.token_user(&sha256_hex(t.as_bytes()))))?;
        let u = store.user(&name)?;
        Some(Who { name, role: Role::parse(&u.role)? })
    }

    pub fn start_session(&self, user: &str) -> String {
        let sid = hex(&nonce());
        let now = now_ms();
        self.sessions.lock().unwrap().insert(sid.clone(), Sess { user: user.into(), created: now, last: now });
        sid
    }

    pub fn end_session(&self, sid: &str) {
        self.sessions.lock().unwrap().remove(sid);
    }

    pub fn login(&self, store: &Store, name: &str, password: &str, ip: &str) -> Result<(Who, String), LoginErr> {
        self.login_at(store, name, password, ip, now_ms())
    }

    pub fn login_at(&self, store: &Store, name: &str, password: &str, ip: &str, now: i64) -> Result<(Who, String), LoginErr> {
        let key = format!("{name}|{ip}");
        {
            let mut f = self.fails.lock().unwrap();
            f.retain(|_, v| {
                v.retain(|t| now - t < fail_window_ms());
                !v.is_empty()
            });
            if f.get(&key).is_some_and(|v| v.len() >= fail_max()) {
                return Err(LoginErr::Throttled);
            }
        }
        let user = store.user(name);
        let ok = verify_password(password, user.as_ref().map_or(dummy_record(), |u| &u.pw_hash)) && user.is_some();
        let role = user.as_ref().and_then(|u| Role::parse(&u.role));
        match (ok, role) {
            (true, Some(role)) => {
                self.fails.lock().unwrap().remove(&key);
                Ok((Who { name: name.into(), role }, self.start_session(name)))
            }
            _ => {
                self.fails.lock().unwrap().entry(key).or_default().push(now);
                Err(LoginErr::Bad)
            }
        }
    }

    /// New API token for `user`: the raw token is returned once, only its sha256 is stored.
    pub fn new_token(&self, store: &Store, user: &str) -> Result<String, String> {
        let t = format!("ckt_{}", hex(&nonce()));
        store.add_token(&sha256_hex(t.as_bytes()), user, now_ms())?;
        Ok(t)
    }
}

pub fn valid_name(n: &str) -> bool {
    (1..=64).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Minimum role for a route; None = public. Everything under /api/ not listed needs a viewer (404 follows).
pub fn min_role(method: &str, path: &str) -> Option<Role> {
    let p = path.trim_end_matches('/');
    Some(match (method, p) {
        (_, "/api/v1/me" | "/api/v1/bootstrap" | "/api/v1/login" | "/api/v1/logout") => return None,
        (_, "/api/v1/users" | "/api/v1/config") => Role::Admin,
        ("POST", "/api/v1/jobs" | "/api/v1/experiments") => Role::Researcher,
        ("POST", _) if p.starts_with("/api/v1/alerts/") && p.ends_with("/ack") => Role::Operator,
        ("POST", "/api/v1/ha/switchover") | ("PUT", "/api/v1/ha/settings" | "/api/v1/tunables") => Role::Admin,
        ("GET", "/api/v1/logs") => Role::Operator,
        ("POST", "/api/v1/nodes/power") | ("PUT", "/api/v1/power") => Role::Operator,
        ("POST", "/api/v1/nodes/state" | "/api/v1/labs" | "/api/v1/join-codes") => Role::Operator,
        ("PUT", _) if p.starts_with("/api/v1/nodes/") && p.ends_with("/place") => Role::Operator,
        ("PATCH" | "DELETE", _) if p.starts_with("/api/v1/labs/") => Role::Operator,
        ("PUT", _) if p.starts_with("/api/v1/nodes/") && p.ends_with("/labels") => Role::Operator,
        ("PUT" | "DELETE", _) if p.starts_with("/api/v1/pools/") => Role::Operator,
        ("POST", "/api/v1/caps" | "/api/v1/reserves" | "/api/v1/windows" | "/api/v1/reservations") => Role::Operator,
        ("DELETE", _) if ["caps", "reserves", "windows", "reservations"].iter().any(|k| p.starts_with(&format!("/api/v1/{k}/"))) => Role::Operator,
        ("POST", _) if p.starts_with("/api/v1/jobs/") && p.ends_with("/cancel") => Role::Researcher,
        _ if p.starts_with("/api/") => Role::Viewer,
        _ => return None,
    })
}

/// Every API route (method, pattern). api::tests checks each has a CLI command in jobcli::CLI_MAP (API-02).
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/me"), ("GET", "/ha"), ("POST", "/ha/switchover"), ("POST", "/bootstrap"), ("POST", "/login"), ("POST", "/logout"), ("POST", "/tokens"),
    ("GET", "/users"), ("POST", "/users"), ("GET", "/cluster"),
    ("GET", "/nodes"), ("GET", "/nodes/{id}"), ("POST", "/nodes/state"), ("PUT", "/nodes/{id}/place"),
    ("GET", "/labs"), ("POST", "/labs"), ("PATCH", "/labs/{id}"), ("DELETE", "/labs/{id}"), ("POST", "/join-codes"),
    ("GET", "/jobs"), ("POST", "/jobs"), ("GET", "/jobs/{id}"), ("GET", "/jobs/{id}/result"), ("POST", "/jobs/{id}/cancel"),
    ("GET", "/events"), ("GET", "/stream"),
    ("GET", "/select"), ("PUT", "/nodes/{id}/labels"), ("GET", "/pools"), ("PUT", "/pools/{name}"), ("DELETE", "/pools/{name}"),
    ("GET", "/caps"), ("POST", "/caps"), ("DELETE", "/caps/{id}"), ("GET", "/reserves"), ("POST", "/reserves"), ("DELETE", "/reserves/{id}"),
    ("GET", "/config"), ("POST", "/config"),
    ("GET", "/windows"), ("POST", "/windows"), ("DELETE", "/windows/{id}"),
    ("GET", "/reservations"), ("POST", "/reservations"), ("DELETE", "/reservations/{id}"), ("GET", "/estimate"),
    ("POST", "/nodes/power"), ("GET", "/power"), ("PUT", "/power"), ("GET", "/ha/settings"), ("PUT", "/ha/settings"),
    ("GET", "/tunables"), ("PUT", "/tunables"), ("GET", "/ha/candidates"),
    ("GET", "/logs"),
    ("GET", "/jobs/{id}/breakdown"), ("GET", "/alerts"), ("POST", "/alerts/{id}/ack"),
    ("POST", "/experiments"), ("GET", "/experiments"), ("GET", "/experiments/{id}"), ("GET", "/report"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let r = hash_password("correct horse battery", 1000);
        assert!(r.starts_with("pbkdf2$1000$"));
        assert!(verify_password("correct horse battery", &r));
        assert!(!verify_password("wrong", &r));
        assert!(!verify_password("x", "garbage"));
        assert_ne!(r, hash_password("correct horse battery", 1000), "random salt");
        // RFC 7914 test vector: PBKDF2-HMAC-SHA256("passwd", "salt", 1)
        assert_eq!(hex(&pbkdf2(b"passwd", b"salt", 1))[..16], *"55ac046e56e3089f");
    }

    #[test]
    fn route_roles() {
        let table = [
            ("GET", "/api/v1/me", None),
            ("POST", "/api/v1/login", None),
            ("POST", "/api/v1/bootstrap", None),
            ("GET", "/stats", None),
            ("GET", "/_app/x.js", None),
            ("POST", "/api/v1/tokens", Some(Role::Viewer)),
            ("GET", "/api/v1/cluster", Some(Role::Viewer)),
            ("GET", "/api/v1/jobs", Some(Role::Viewer)),
            ("GET", "/api/v1/jobs/3/result", Some(Role::Viewer)),
            ("GET", "/api/v1/stream", Some(Role::Viewer)),
            ("POST", "/api/v1/jobs", Some(Role::Researcher)),
            ("POST", "/api/v1/jobs/3/cancel", Some(Role::Researcher)),
            ("GET", "/api/v1/nodes/n1", Some(Role::Viewer)),
            ("GET", "/api/v1/labs", Some(Role::Viewer)),
            ("POST", "/api/v1/nodes/state", Some(Role::Operator)),
            ("PUT", "/api/v1/nodes/n1/place", Some(Role::Operator)),
            ("POST", "/api/v1/labs", Some(Role::Operator)),
            ("PATCH", "/api/v1/labs/2", Some(Role::Operator)),
            ("DELETE", "/api/v1/labs/2", Some(Role::Operator)),
            ("POST", "/api/v1/join-codes", Some(Role::Operator)),
            ("GET", "/api/v1/select", Some(Role::Viewer)),
            ("GET", "/api/v1/pools", Some(Role::Viewer)),
            ("GET", "/api/v1/caps", Some(Role::Viewer)),
            ("GET", "/api/v1/reserves", Some(Role::Viewer)),
            ("PUT", "/api/v1/nodes/n1/labels", Some(Role::Operator)),
            ("PUT", "/api/v1/pools/night", Some(Role::Operator)),
            ("DELETE", "/api/v1/pools/night", Some(Role::Operator)),
            ("POST", "/api/v1/caps", Some(Role::Operator)),
            ("DELETE", "/api/v1/caps/2", Some(Role::Operator)),
            ("POST", "/api/v1/reserves", Some(Role::Operator)),
            ("DELETE", "/api/v1/reserves/2", Some(Role::Operator)),
            ("GET", "/api/v1/config", Some(Role::Admin)),
            ("POST", "/api/v1/config", Some(Role::Admin)),
            ("GET", "/api/v1/tunables", Some(Role::Viewer)),
            ("PUT", "/api/v1/tunables", Some(Role::Admin)),
            ("GET", "/api/v1/ha/candidates", Some(Role::Viewer)),
            ("GET", "/api/v1/logs", Some(Role::Operator)),
            ("GET", "/api/v1/alerts", Some(Role::Viewer)),
            ("POST", "/api/v1/alerts/3/ack", Some(Role::Operator)),
            ("POST", "/api/v1/experiments", Some(Role::Researcher)),
            ("GET", "/api/v1/experiments/2", Some(Role::Viewer)),
            ("GET", "/api/v1/jobs/4/breakdown", Some(Role::Viewer)),
            ("GET", "/api/v1/report", Some(Role::Viewer)),
            ("GET", "/api/v1/users", Some(Role::Admin)),
            ("POST", "/api/v1/users", Some(Role::Admin)),
        ];
        for (m, p, want) in table {
            assert_eq!(min_role(m, p), want, "{m} {p}");
        }
    }

    #[test]
    fn throttle_and_sessions() {
        let st = Store::memory();
        st.create_user("ann", &hash_password("longenoughpw1", 100), "viewer", 1).unwrap();
        let a = Auth::new(false);
        // iterations in the stored record are tiny, so the wrong-password path is fast
        for i in 0..5 {
            assert!(matches!(a.login_at(&st, "ann", "bad", "1.1.1.1", 1000 + i), Err(LoginErr::Bad)));
        }
        assert!(matches!(a.login_at(&st, "ann", "longenoughpw1", "1.1.1.1", 2000), Err(LoginErr::Throttled)));
        assert!(matches!(a.login_at(&st, "ann", "longenoughpw1", "2.2.2.2", 2000), Ok(_)), "other ip unaffected");
        let (w, sid) = a.login_at(&st, "ann", "longenoughpw1", "1.1.1.1", 1000 + fail_window_ms() + 10).ok().unwrap();
        assert_eq!(w.role, Role::Viewer);
        assert_eq!(a.who(&st, Some(&sid), None).map(|w| w.name), Some("ann".into()));
        assert!(a.who_at(&st, Some(&sid), None, now_ms() + idle_ms() + 1000).is_none(), "idle expiry");
        let t = a.new_token(&st, "ann").unwrap();
        assert!(t.starts_with("ckt_"));
        assert_eq!(a.who(&st, None, Some(&t)).map(|w| w.name), Some("ann".into()));
        assert!(a.who(&st, None, Some("ckt_nope")).is_none());
        assert_eq!(Auth::new(true).who(&st, None, None).unwrap().role, Role::Admin);
    }
}
