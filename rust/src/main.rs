//! ck: a general-purpose cluster over raw TCP with a binary protocol. Roadmap: docs/CONSOLE_REQUIREMENTS.md.
//!
//!   ck head    [--name lab] [--threads N] [--no-local-worker] [--spin] [--gpu] [--standby --head PRIMARY] [--parent ROOT[:port][,STANDBY..] [--lab NAME]]
//!   ck agent   [--name lab] [--threads N] [--head IP] [--spin] [--gpu] [--virtual N] [--code JOIN-CODE] [--relay]   (alias: ck worker)
//!   ck run     [--head IP] --n 1e9 [--from 1] [--chunk 1e7] [--only IP,IP] (--sha PREFIX | --cmd "prog" | --push FILE [--with "interpreter"] | --synth NS_PER_NUMBER) [--selector SEL] [--priority P] [--policy adaptive|guided|fixed] [--reducer R] [--list FILE]
//!   ck ping    [--head IP] [--count 10000] [--size 8] [--spin]   (round-trip latency / throughput)
//!   ck install [--head IP] [--name lab] [--dry-run]               (run the agent as a system service)
//!   ck sim     SCENARIO.json [--out DIR]   (simulate a cluster: timeline.csv + summary.json; format at the top of src/sim.rs)
//!   ck job     ls | show ID | cancel ID | result ID [--head IP]   (jobs on the head, via the HTTP API)
//!   ck admin   nodes | approve ID.. | state S ID.. | labs | lab-add N W H | place N LAB X Y | unplace N | join-code [USES] [MIN]
//!   ck logs    [--role head] [--since 10m] [--kind node.] [--level info] [--limit N] [--follow]   (structured log, docs/LOGGING.md)
//!   ck run ... --detach   submit, print the job id, exit (the job keeps running on the head)
//!
//! Ports: CK_PORT (default 7700) for workers and clients, +1 UDP discovery, +2 dashboard.
//!
//! Modules (one owner each, see docs/swarm/PLAN.md):
//!   proto     wire contract: frames, kinds, HELLO, ports
//!   head      job, dispatch, requeue, beacon          dash     dashboard and /stats
//!   agent     worker loop, discovery, heartbeats      identity node id and inventory
//!   kernels   built-in work (sha, cmd, push, synth)   client   ck run / ck ping
//!   auth      credentials and frame MACs              install  system service
//!   gpu       SHA-256 on the GPU (feature "gpu")

mod advisor;
mod agent;
mod auth;
mod blobs;
mod client;
mod dash;
#[cfg(feature = "gpu")]
mod gpu;
mod head;
mod hier;
mod identity;
mod install;
mod jobcli;
mod jobs;
mod kernels;
mod lab;
mod log;
mod presence;
mod proto;
mod reduce;
mod research;
mod repl;
mod sched;
mod sim;
mod tune;
mod select;
mod store;
mod users;
mod api;

use std::sync::atomic::Ordering::Relaxed;
use std::{env, thread};

/// The head holds about 2 file descriptors per PC. The usual soft limit (1,024 on Linux and under systemd) would stop it
/// at about 500 PCs, so raise it to the hard limit (macOS refuses "unlimited": try smaller values). No-op elsewhere.
#[cfg(all(unix, target_pointer_width = "64"))]
fn raise_nofile() {
    #[repr(C)]
    struct RLimit {
        cur: u64,
        max: u64,
    }
    extern "C" {
        fn getrlimit(resource: i32, l: *mut RLimit) -> i32;
        fn setrlimit(resource: i32, l: *const RLimit) -> i32;
    }
    const NOFILE: i32 = if cfg!(target_os = "linux") { 7 } else { 8 };
    let mut l = RLimit { cur: 0, max: 0 };
    // SAFETY: plain libc calls with a #[repr(C)] struct of the platform's rlim_t (u64 on 64-bit unix).
    unsafe {
        if getrlimit(NOFILE, &mut l) != 0 {
            return;
        }
        for want in [l.max, 1 << 20, 65_536, 10_240] {
            let n = RLimit { cur: want.min(l.max), max: l.max };
            if n.cur <= l.cur || setrlimit(NOFILE, &n) == 0 {
                return;
            }
        }
    }
}
#[cfg(not(all(unix, target_pointer_width = "64")))]
fn raise_nofile() {}

pub struct Opts {
    pub name: String,
    pub head: Option<String>,
    pub threads: usize,
    pub no_local: bool,
    pub n: u64,
    pub from: u64,
    pub chunk: u64,
    pub sha: Option<String>,
    pub cmd: Option<String>,
    pub push: Option<String>,
    pub with: Option<String>,
    pub only: Option<String>,
    /// `run --synth NS`: synthetic job, NS nanoseconds per number (KIND_SYNTH).
    pub synth: Option<u64>,
    /// `agent --virtual N`: N logical agents in this process (SIM-01). 1 = a normal agent.
    pub virtual_n: usize,
    pub dry_run: bool,
    /// `run --detach`: print the job id and exit.
    pub detach: bool,
    /// `run --selector "lab=A cores>=8"`: which PCs may run the job (ORG-04).
    pub selector: Option<String>,
    /// `run --priority urgent|high|normal|low|background` (SCH-02).
    pub priority: Option<String>,
    /// `head --standby --head PRIMARY`: keep a replica and take over when the primary fails (HA-03, repl.rs).
    pub standby: bool,
    /// `head --parent ROOT[,STANDBY..]`: run as a lab head under that root (docs/HIERARCHY.md).
    pub parent: Option<String>,
    /// `head --lab NAME`: the lab this lab head serves (default: --name).
    pub lab: Option<String>,
    /// `agent --relay`: re-broadcast the head's beacon on this PC's subnet (FLT-05).
    pub relay: bool,
    /// Bare arguments after the command (`ck job show 12` -> ["show", "12"]).
    pub rest: Vec<String>,
    pub count: usize,
    pub size: usize,
    pub spin: bool,
    pub gpu: bool,
}

pub fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    crate::lg!(Error, "process.die", {"msg": msg});
    log::flush();
    std::process::exit(1)
}

fn num(s: Option<String>) -> u64 {
    let s = s.unwrap_or_else(|| die("missing number"));
    s.parse::<u64>().ok().or_else(|| s.parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64)).unwrap_or_else(|| die(&format!("bad number: {s}")))
}

fn main() {
    std::sync::LazyLock::force(&store::PROC_START);
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let mut o = Opts {
        name: "lab".into(),
        head: None,
        threads: thread::available_parallelism().map_or(1, |n| n.get()),
        no_local: false,
        n: 1_000_000,
        from: 1,
        chunk: 0,
        sha: None,
        cmd: None,
        push: None,
        with: None,
        only: None,
        synth: None,
        virtual_n: 1,
        dry_run: false,
        detach: false,
        selector: None,
        priority: None,
        relay: false,
        standby: false,
        parent: None,
        lab: None,
        rest: vec![],
        count: 10_000,
        size: 8,
        spin: false,
        gpu: false,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--name" => o.name = args.next().unwrap_or_else(|| die("--name needs a value")),
            "--head" => o.head = args.next(),
            "--threads" => o.threads = num(args.next()).max(1) as usize,
            "--no-local-worker" => o.no_local = true,
            "--spin" => o.spin = true,
            "--gpu" => o.gpu = true,
            "--n" => o.n = num(args.next()),
            "--from" => o.from = num(args.next()),
            "--chunk" => o.chunk = num(args.next()),
            "--sha" => o.sha = args.next(),
            "--cmd" => o.cmd = args.next(),
            "--push" => o.push = args.next(),
            "--with" => o.with = args.next(),
            "--only" => o.only = args.next(),
            "--synth" => o.synth = Some(num(args.next())),
            "--virtual" => o.virtual_n = num(args.next()).max(1) as usize,
            "--dry-run" => o.dry_run = true,
            "--detach" => o.detach = true,
            "--selector" => o.selector = Some(args.next().unwrap_or_else(|| die("--selector needs a value"))),
            "--priority" => o.priority = Some(args.next().unwrap_or_else(|| die("--priority needs a value"))),
            "--relay" => o.relay = true,
            "--out" => std::env::set_var("CK_SIM_OUT", args.next().unwrap_or_else(|| die("--out needs a directory"))),
            "--standby" => o.standby = true,
            "--parent" => o.parent = Some(args.next().unwrap_or_else(|| die("--parent needs ROOT[:port]"))),
            "--lab" => o.lab = Some(args.next().unwrap_or_else(|| die("--lab needs a name"))),
            // `ck logs` filters, read by log::cli
            "--role" => std::env::set_var("CK_LOGS_ROLE", args.next().unwrap_or_else(|| die("--role needs head|agent|standby|client"))),
            "--since" => std::env::set_var("CK_LOGS_SINCE", args.next().unwrap_or_else(|| die("--since needs e.g. 10m"))),
            "--kind" => std::env::set_var("CK_LOGS_KIND", args.next().unwrap_or_else(|| die("--kind needs a prefix, e.g. node."))),
            "--level" => std::env::set_var("CK_LOGS_LEVEL", args.next().unwrap_or_else(|| die("--level needs error|warn|info|debug"))),
            "--limit" => std::env::set_var("CK_LOGS_LIMIT", args.next().unwrap_or_else(|| die("--limit needs a number"))),
            "--follow" => std::env::set_var("CK_LOGS_FOLLOW", "1"),
            // read by client.rs / jobcli.rs from the environment (one place that already handles them)
            "--policy" => std::env::set_var("CK_POLICY", args.next().unwrap_or_else(|| die("--policy needs fixed|guided|adaptive"))),
            "--reducer" => std::env::set_var("CK_REDUCER", args.next().unwrap_or_else(|| die("--reducer needs a value"))),
            "--blob" => {
                let f = args.next().unwrap_or_else(|| die("--blob needs a file"));
                let all = std::env::var("CK_BLOB_FILES").map(|v| format!("{v}\n{f}")).unwrap_or(f);
                std::env::set_var("CK_BLOB_FILES", all);
            }
            "--template" => std::env::set_var("CK_TEMPLATE", args.next().unwrap_or_else(|| die("--template needs a name"))),
            "--get" => std::env::set_var("CK_GET_DIR", args.next().unwrap_or_else(|| die("--get needs a directory"))),
            "--list" => std::env::set_var("CK_LIST_FILE", args.next().unwrap_or_else(|| die("--list needs a file"))),
            // first join of a PC (FLT-04); the agent reads it from CK_JOIN_CODE
            "--code" => std::env::set_var("CK_JOIN_CODE", args.next().unwrap_or_else(|| die("--code needs a value"))),
            "--size" => o.size = num(args.next()).min(proto::MAX_FRAME as u64) as usize,
            "--count" => o.count = num(args.next()).max(1) as usize,
            _ if !a.starts_with("--") => o.rest.push(a),
            _ => die(&format!("unknown option {a}")),
        }
    }
    proto::SPIN.store(o.spin, Relaxed);
    if cmd == "head" {
        raise_nofile();
    }
    rayon::ThreadPoolBuilder::new().num_threads(o.threads).build_global().unwrap();
    match cmd.as_str() {
        "head" | "agent" | "worker" | "run" => log::init(
            match (cmd.as_str(), o.standby) { ("head", true) => "standby", ("head", _) => "head", ("run", _) => "client", _ => "agent" },
            &identity::node_id(),
        ),
        _ => {}
    }
    match cmd.as_str() {
        "logs" => log::cli(),
        "head" if o.standby => {
            repl::run_standby(&o); // returns when this PC must take over
            log::set_role("head");
            crate::say!("ha.promote", "standby: promoted, now the head");
            store::maintenance_thread();
            head::run(&o)
        }
        "head" => { store::maintenance_thread(); head::run(&o) }
        "worker" | "agent" => agent::run(&o),
        "run" => client::run(&o),
        "ping" => client::ping(&o),
        "install" => install::run(&o),
        "job" => jobcli::run(&o),
        "blob" => jobcli::blob(&o),
        "admin" => jobcli::admin(&o),
        "token" => jobcli::token(&o),
        "sim" => sim::run(&o),
        _ => die("usage: ck head|agent|run|ping|install|job|token|admin|logs [options]  (see the top of src/main.rs)"),
    }
    log::flush();
}
