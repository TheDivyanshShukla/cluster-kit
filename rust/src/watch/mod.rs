//! Campus cameras on the Rust engine.
//!
//! Every decoded frame is gated. A blurry or blank frame does not reach a detector.
//! Rates on the panel are a smoothed once-a-second sample: counting inside the model
//! would spend the cores the cameras need.

#[allow(dead_code)]
mod alerts;
#[allow(dead_code)]
mod api;
mod approx;
#[allow(dead_code)]
mod auth;
mod config;
mod decode;
#[allow(dead_code)]
mod domain;
mod fetch;
mod fight;
mod gate;
mod head;
#[allow(dead_code)]
mod ingest;
#[allow(dead_code)]
mod matches;
#[allow(dead_code)]
mod media_catalog;
#[allow(dead_code)]
mod models;
mod net;
mod query;
mod rules;
#[allow(dead_code)]
mod shard;
#[allow(dead_code)]
mod scheduler;
mod split;
#[allow(dead_code)]
mod storage;
mod track;
mod worker;
mod yolo;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PORT: u16 = 7710;
pub const DISC: u16 = 7711;
pub const MAGIC: &str = "pc-cluster-vi/1";

pub fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let mut o = Opts {
        name: "lab".into(),
        head: None,
        port: PORT,
        no_local: false,
        local: false,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--name" => o.name = args.next().ok_or("--name needs a value")?,
            "--head" => o.head = args.next(),
            "--port" => {
                o.port = args
                    .next()
                    .ok_or("--port needs a value")?
                    .parse()
                    .map_err(|_| "bad --port".to_string())?;
            }
            "--no-local-worker" => o.no_local = true,
            "--local" => o.local = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    match cmd.as_str() {
        "head" => head::run(o),
        "worker" => worker::run(o),
        "fetch" => fetch::run(),
        _ => Err("usage: cw head | cw worker [--head IP] | cw fetch".into()),
    }
}

pub struct Opts {
    pub name: String,
    pub head: Option<String>,
    pub port: u16,
    pub no_local: bool,
    pub local: bool,
}

pub fn log(msg: &str) {
    eprintln!("cw  {msg}");
}

pub fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn repo_root() -> PathBuf {
    if let Some(root) = std::env::var_os("CW_ROOT") { return PathBuf::from(root); }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("kit").exists() || cwd.join("kit.bat").exists() {
            return cwd;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.clone();
        for _ in 0..4 {
            if !dir.pop() {
                break;
            }
            if dir.join("vision").is_dir() {
                return dir;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Physical cores. Hyperthreads share the same vector units, so the detector uses these.
pub fn detector_threads() -> usize {
    if let Ok(s) = std::env::var("CW_THREADS") {
        if let Ok(n) = s.parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    sysinfo::System::physical_core_count().filter(|n| *n > 0).unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).div_ceil(2)
    })
}

pub fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "pc".into())
}

pub fn primary_ip() -> String {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok();
    if let Some(s) = s {
        if s.connect("8.8.8.8:9").is_ok() {
            if let Ok(a) = s.local_addr() {
                return a.ip().to_string();
            }
        }
    }
    "127.0.0.1".into()
}
