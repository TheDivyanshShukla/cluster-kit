//! `ck sim SCENARIO.json [--out DIR]`: deterministic discrete-event simulation of a cluster (SIM-02, SIM-03).
//! It calls sched::chunk_len, so the simulated head chunks work exactly like the real one (decision D5).
//! Writes DIR/timeline.csv (t_s, done, rate_per_s, busy_nodes, connected_nodes, head_busy_frac) and DIR/summary.json.
//!
//! Scenario (every field optional, defaults in brackets):
//! {
//!   "nodes": [{"count": 1, "rate": 1e6, "jitter": 0.0, "mtbf_s": 0, "down_s": 60, "lab": "A",
//!              "speed_cv": 0, "run_cv": 0, "chunk_overhead_ms": 0, "fail_at_s": 0}],
//!                 speed_cv = static spread of PC speed (normal, drawn once per PC); run_cv = run-to-run noise (drawn once per run);
//!                 chunk_overhead_ms = fixed node time per chunk; fail_at_s = the whole group fails once then (E3)
//!                 rate = numbers/s per node; jitter = per-chunk uniform spread (+-fraction of the time);
//!                 mtbf_s = mean time between failures (exponential), 0 = never; down_s = time away after a failure
//!   "job": {"n": 1e9, "policy": "adaptive", "chunk": 0},          policy adaptive|guided|fixed; chunk only for fixed
//!   "network": {"latency_ms": 0.3},                                one way
//!   "head": {"service_us": 379, "service_us_per_agent": 0.199, "inflight": 2},
//!                 head time per task = service_us + service_us_per_agent x connected agents (calibrated on the measured
//!                 200- and 1,000-agent points, docs/sim/VALIDATION.md)
//!                 chunks in flight per node
//!   "architecture": "single",                                      single | standby | relays | relays+standby
//!   "relays": {"per_lab": true, "batch": 20, "extra_latency_ms": 0.3},   batch = tasks per head message
//!   "head_failure": {"at_s": 0, "recovery_s": 5.7, "manual_restart_s": 0},
//!                 at_s 0 = none; standby: dispatch resumes recovery_s later (measured 5.7 s); without a standby the job
//!                 stops, unless manual_restart_s > 0 (someone restarts the head after that long)
//!   "seed": 1, "sample_s": 1, "max_s": 86400                       max_s = stop an unfinished run
//! }
//! A scenario with a "day" key runs the job-level day simulator instead (see `day()` below).
//! A scenario with a "hier" key runs one two-level (root + lab heads) run; {"hier_study": 1} runs the four hierarchy
//! studies and writes hier_*.csv (see the hierarchy section at the end of this file and docs/sim/HIERARCHY.md).
use crate::sched::{self, ChunkPolicy};
use crate::{die, Opts};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};

struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// standard normal (Box-Muller)
    fn n(&mut self) -> f64 {
        (-2.0 * (1.0 - self.u()).ln()).sqrt() * (std::f64::consts::TAU * self.u()).cos()
    }
    /// 1 + cv x N(0,1), floored so a rate never goes near zero
    fn mult(&mut self, cv: f64) -> f64 {
        if cv > 0.0 { (1.0 + cv * self.n()).max(0.2) } else { 1.0 }
    }
}

#[derive(Clone)]
struct Group {
    count: usize,
    rate: f64,
    jitter: f64,
    mtbf: f64,
    down: f64,
    lab: usize,
    speed_cv: f64,    // static spread of PC speed inside the group (drawn once per PC)
    run_cv: f64,      // run-to-run noise of a PC's speed (drawn once per run)
    fail_at: f64,     // 0 = never; else the whole group fails once at this time (E3: lose k PCs mid-job)
    overhead_s: f64,  // fixed cost of every chunk on the node (spawn, copy, result)
}

pub struct Scn {
    groups: Vec<Group>,
    n: u64,
    policy: ChunkPolicy,
    chunk: u64,
    lat: f64,
    svc: f64,
    svc_agent: f64, // extra head time per task and per connected agent (seconds)
    inflight: u32,
    relays: bool,
    per_lab: bool,
    batch: f64,
    extra: f64,
    head_fail: Option<(f64, f64)>, // (at, outage)
    head_stops: bool,              // outage never ends (no standby, no manual restart)
    seed: u64,
    sample: f64,
    max_s: f64,
}

fn f(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(d)
}

impl Scn {
    pub fn parse(text: &str) -> Result<Scn, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("bad scenario JSON: {e}"))?;
        let mut labs: Vec<String> = vec![];
        let mut groups: Vec<Group> = vec![];
        for g in v["nodes"].as_array().cloned().unwrap_or_else(|| vec![json!({})]) {
            let lab = g["lab"].as_str().unwrap_or("A").to_string();
            let lab = labs.iter().position(|l| *l == lab).unwrap_or_else(|| {
                labs.push(lab);
                labs.len() - 1
            });
            groups.push(Group { count: f(&g, "count", 1.0) as usize, rate: f(&g, "rate", 1e6), jitter: f(&g, "jitter", 0.0), mtbf: f(&g, "mtbf_s", 0.0), down: f(&g, "down_s", 60.0), lab, speed_cv: f(&g, "speed_cv", 0.0), run_cv: f(&g, "run_cv", 0.0), overhead_s: f(&g, "chunk_overhead_ms", 0.0) / 1e3, fail_at: f(&g, "fail_at_s", 0.0) });
        }
        let pol = v["job"]["policy"].as_str().unwrap_or("adaptive");
        let policy = sched::parse_policy(pol).ok_or(format!("bad policy {pol}"))?;
        let arch = v["architecture"].as_str().unwrap_or("single");
        if !["single", "standby", "relays", "relays+standby"].contains(&arch) {
            return Err(format!("bad architecture {arch}"));
        }
        let (r, hf) = (&v["relays"], &v["head_failure"]);
        let at = f(hf, "at_s", 0.0);
        let standby = arch.ends_with("standby");
        let restart = f(hf, "manual_restart_s", 0.0);
        let outage = if standby { f(hf, "recovery_s", 5.7) } else { restart };
        Ok(Scn {
            groups,
            n: f(&v["job"], "n", 1e9) as u64,
            policy,
            chunk: f(&v["job"], "chunk", 0.0) as u64,
            lat: f(&v["network"], "latency_ms", 0.3) / 1e3,
            svc: f(&v["head"], "service_us", 379.0) / 1e6,
            svc_agent: f(&v["head"], "service_us_per_agent", 0.199) / 1e6,
            inflight: f(&v["head"], "inflight", 2.0).max(1.0) as u32,
            relays: arch.starts_with("relays"),
            per_lab: r["per_lab"].as_bool().unwrap_or(true),
            batch: f(r, "batch", 20.0).max(1.0),
            extra: f(r, "extra_latency_ms", 0.3) / 1e3,
            head_fail: (at > 0.0).then_some((at, outage)),
            head_stops: at > 0.0 && !standby && restart <= 0.0,
            seed: f(&v, "seed", 1.0) as u64,
            sample: f(&v, "sample_s", 1.0).max(1e-3),
            max_s: f(&v, "max_s", 86400.0),
        })
    }
}

#[derive(Clone, Copy)]
enum K {
    Head { n: usize, g: u32, c: u64 }, // a request (c = 0) or a result reaches the head queue
    Fin { n: usize, g: u32, c: u64 },  // a node finished chunk c
    Fail { n: usize },
    Up { n: usize },
}
struct Ev<T = K> {
    t: f64,
    seq: u64,
    k: T,
}
impl<T> PartialEq for Ev<T> {
    fn eq(&self, o: &Self) -> bool {
        self.seq == o.seq
    }
}
impl<T> Eq for Ev<T> {}
impl<T> PartialOrd for Ev<T> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl<T> Ord for Ev<T> {
    fn cmp(&self, o: &Self) -> Ordering {
        o.t.total_cmp(&self.t).then(o.seq.cmp(&self.seq)) // min-heap
    }
}

struct Nd {
    g: Group,
    up: bool,
    gen: u32,
    used: u32, // slots with a request or a chunk outstanding
    in_idle: bool,
    busy_until: f64,
    chunks: Vec<u64>,
}

pub struct Out {
    pub finished: bool,
    pub end_s: f64,
    pub ideal_s: f64,
    pub n: u64,
    pub done: u64,
    #[allow(dead_code)] // read by the sim tests (nothing left behind when finished)
    pub pending: u64,
    #[allow(dead_code)]
    pub inflight: u64,
    pub tasks: u64,
    pub failures: u64,
    pub recovered: u64,
    pub head_util: f64,
    pub timeline: Vec<[f64; 6]>,
}

impl Out {
    pub fn summary(&self) -> Value {
        json!({"finished": self.finished, "makespan_s": self.finished.then_some(self.end_s), "end_s": self.end_s, "ideal_s": self.ideal_s,
            "efficiency_pct": if self.finished { 100.0 * self.ideal_s / self.end_s } else { 0.0 }, "n": self.n, "done": self.done,
            "tasks": self.tasks, "head_utilisation_pct": 100.0 * self.head_util, "node_failures": self.failures,
            "recovered_work": self.recovered, "duplicate_work": 0, "throughput_per_s": if self.finished { self.n as f64 / self.end_s } else { 0.0 },
            "tasks_per_s": self.tasks as f64 / self.end_s.max(1e-9)})
    }
}

struct Sim<'a> {
    s: &'a Scn,
    nodes: Vec<Nd>,
    heap: BinaryHeap<Ev>,
    seq: u64,
    rng: Rng,
    pending: u64,
    inflight: u64,
    done: u64,
    next_chunk: u64,
    chunks: HashMap<u64, (usize, u64)>,
    head_free: f64,
    head_busy: f64,
    relay_free: Vec<f64>,
    idle: Vec<usize>,
    connected: usize,
    fleet_rate: f64,
    busy_nodes: usize,
    tasks: u64,
    failures: u64,
    recovered: u64,
    finish: Option<f64>,
}

impl Sim<'_> {
    fn push(&mut self, t: f64, k: K) {
        self.seq += 1;
        self.heap.push(Ev { t, seq: self.seq, k });
    }
    /// when a message sent by node i at t is at the head's queue (through its lab relay, if any)
    fn route(&mut self, t: f64, i: usize) -> f64 {
        let a = t + self.s.lat;
        if !self.s.relays {
            return a;
        }
        let r = if self.s.per_lab { self.nodes[i].g.lab } else { 0 };
        let st = a.max(self.relay_free[r]);
        self.relay_free[r] = st + self.s.svc;
        st + self.s.svc + self.s.extra
    }
    fn request(&mut self, i: usize, t: f64) {
        self.nodes[i].used += 1;
        let (g, a) = (self.nodes[i].gen, self.route(t, i));
        self.push(a, K::Head { n: i, g, c: 0 });
    }
    /// fill every free slot of node i: a request if there is work, else park the node as idle
    fn fill(&mut self, i: usize, t: f64) {
        while self.nodes[i].used < self.s.inflight && self.pending > 0 {
            self.request(i, t);
        }
        if self.nodes[i].used < self.s.inflight && !self.nodes[i].in_idle {
            self.nodes[i].in_idle = true;
            self.idle.push(i);
        }
    }
    /// work came back to the pool: ask up to k idle slots for it
    fn wake(&mut self, t: f64, mut k: usize) {
        while k > 0 && self.pending > 0 {
            let Some(i) = self.idle.pop() else { break };
            self.nodes[i].in_idle = false;
            while k > 0 && self.nodes[i].up && self.nodes[i].used < self.s.inflight {
                self.request(i, t);
                k -= 1;
            }
            if self.nodes[i].up && self.nodes[i].used < self.s.inflight {
                self.nodes[i].in_idle = true;
                self.idle.push(i);
            }
        }
    }
    fn schedule_fail(&mut self, i: usize, t: f64) {
        let m = self.nodes[i].g.mtbf;
        if m > 0.0 {
            let d = -m * (1.0 - self.rng.u()).ln();
            self.push(t + d, K::Fail { n: i });
        }
    }
    fn head(&mut self, t: f64, i: usize, g: u32, c: u64) {
        if self.nodes[i].gen != g {
            return; // the node failed since it sent this
        }
        let s = self.s;
        let mut st = t.max(self.head_free);
        if let Some((at, rec)) = s.head_fail.filter(|_| !s.head_stops) {
            if st >= at && st < at + rec {
                st = at + rec; // the standby is promoting (or the head restarts); the queue waits
            }
        }
        let cost = if s.relays { s.svc / s.batch } else { s.svc + s.svc_agent * self.connected as f64 };
        let fin = st + cost;
        self.head_free = fin;
        self.head_busy += cost;
        self.nodes[i].used -= 1;
        if c != 0 {
            let (_, len) = self.chunks.remove(&c).unwrap();
            let nd = &mut self.nodes[i];
            nd.chunks.retain(|x| *x != c);
            if nd.chunks.is_empty() {
                self.busy_nodes -= 1;
            }
            self.done += len;
            self.inflight -= len;
            self.tasks += 1;
            if self.done == s.n {
                self.finish = Some(fin);
                return;
            }
        }
        if self.pending == 0 {
            return self.fill(i, t);
        }
        let nd = &self.nodes[i];
        let len = sched::chunk_len(s.policy, s.chunk, s.n, self.pending, nd.g.rate, self.fleet_rate, self.connected, 1_000, 50_000_000);
        self.pending -= len;
        self.inflight += len;
        self.next_chunk += 1;
        let id = self.next_chunk;
        self.chunks.insert(id, (i, len));
        let jit = nd_jitter(&mut self.rng, nd.g.jitter);
        let nd = &mut self.nodes[i];
        if nd.chunks.is_empty() {
            self.busy_nodes += 1;
        }
        nd.chunks.push(id);
        nd.used += 1;
        let arrive = fin + s.lat + if s.relays { s.extra } else { 0.0 };
        let end = arrive.max(nd.busy_until) + len as f64 / nd.g.rate * jit + nd.g.overhead_s;
        nd.busy_until = end;
        let g = nd.gen;
        self.push(end, K::Fin { n: i, g, c: id });
    }
    fn fail(&mut self, t: f64, i: usize) {
        let nd = &mut self.nodes[i];
        nd.up = false;
        nd.gen += 1;
        nd.used = 0;
        nd.busy_until = 0.0;
        self.connected -= 1;
        self.fleet_rate -= nd.g.rate;
        let lost: Vec<u64> = nd.chunks.drain(..).collect();
        if !lost.is_empty() {
            self.busy_nodes -= 1;
        }
        for c in &lost {
            let (_, len) = self.chunks.remove(c).unwrap();
            self.pending += len;
            self.inflight -= len;
            self.recovered += len;
        }
        self.failures += 1;
        let down = self.nodes[i].g.down;
        self.push(t + down, K::Up { n: i });
        self.wake(t, lost.len());
    }
}

fn nd_jitter(r: &mut Rng, j: f64) -> f64 {
    (1.0 + j * (2.0 * r.u() - 1.0)).max(0.05)
}

pub fn simulate(s: &Scn) -> Out {
    let mut nodes = vec![];
    let mut dr = Rng(s.seed ^ 0xA5A5_5A5A_1234_4321); // speeds draw from their own stream: failures and jitter stay comparable
    for g in &s.groups {
        for _ in 0..g.count {
            let mut g = g.clone();
            g.rate *= dr.mult(g.speed_cv) * dr.mult(g.run_cv);
            nodes.push(Nd { g, up: true, gen: 0, used: 0, in_idle: false, busy_until: 0.0, chunks: vec![] });
        }
    }
    let ideal_s = s.n as f64 / nodes.iter().map(|n| n.g.rate).sum::<f64>();
    let labs = s.groups.iter().map(|g| g.lab + 1).max().unwrap_or(1);
    let mut m = Sim {
        s,
        connected: nodes.len(),
        fleet_rate: nodes.iter().map(|n| n.g.rate).sum(),
        nodes,
        heap: BinaryHeap::new(),
        seq: 0,
        rng: Rng(s.seed),
        pending: s.n,
        inflight: 0,
        done: 0,
        next_chunk: 0,
        chunks: HashMap::new(),
        head_free: 0.0,
        head_busy: 0.0,
        relay_free: vec![0.0; labs],
        idle: vec![],
        busy_nodes: 0,
        tasks: 0,
        failures: 0,
        recovered: 0,
        finish: None,
    };
    for i in 0..m.nodes.len() {
        m.fill(i, 0.0);
        m.schedule_fail(i, 0.0);
        if m.nodes[i].g.fail_at > 0.0 {
            let t = m.nodes[i].g.fail_at;
            m.push(t, K::Fail { n: i });
        }
    }
    // no standby: the job stops when the head dies
    let stop = match s.head_fail {
        Some((at, _)) if s.head_stops => at.min(s.max_s),
        _ => s.max_s,
    };
    let (mut tl, mut next, mut last_done, mut last_busy, mut now) = (vec![], s.sample, 0u64, 0.0, 0.0);
    let mut row = |m: &Sim, t: f64, last_done: &mut u64, last_busy: &mut f64, dt: f64| {
        tl.push([t, m.done as f64, (m.done - *last_done) as f64 / dt, m.busy_nodes as f64, m.connected as f64, ((m.head_busy - *last_busy) / dt).min(1.0)]);
        (*last_done, *last_busy) = (m.done, m.head_busy);
    };
    while let Some(ev) = m.heap.pop() {
        if ev.t >= stop {
            now = stop;
            break;
        }
        while next <= ev.t {
            row(&m, next, &mut last_done, &mut last_busy, s.sample);
            next += s.sample;
        }
        now = ev.t;
        match ev.k {
            K::Head { n, g, c } => m.head(ev.t, n, g, c),
            K::Fin { n, g, c } => {
                if m.nodes[n].gen == g {
                    let a = m.route(ev.t, n);
                    m.push(a, K::Head { n, g, c });
                }
            }
            K::Fail { n } => m.fail(ev.t, n),
            K::Up { n } => {
                let nd = &mut m.nodes[n];
                (nd.up, nd.busy_until) = (true, ev.t);
                m.connected += 1;
                m.fleet_rate += m.nodes[n].g.rate;
                m.fill(n, ev.t);
                m.schedule_fail(n, ev.t);
            }
        }
        if m.finish.is_some() {
            break;
        }
    }
    let end_s = m.finish.unwrap_or(now);
    row(&m, end_s, &mut last_done, &mut last_busy, (end_s - (next - s.sample)).max(1e-9));
    Out { finished: m.finish.is_some(), end_s, ideal_s, n: s.n, done: m.done, pending: m.pending, inflight: m.inflight, tasks: m.tasks, failures: m.failures, recovered: m.recovered, head_util: (m.head_busy / end_s.max(1e-9)).min(1.0), timeline: tl }
}

/// The power model (watts per PC) shared by day mode defaults and GET /api/v1/power `labs`.
pub const W_IDLE: f64 = 25.0;
pub const W_BUSY: f64 = 65.0;
pub const W_SLEEP: f64 = 2.0;
pub const W_BOOT: f64 = 65.0;
pub const W_INTERACTIVE: f64 = 45.0;

pub const API_MAX_NODES: f64 = 20_000.0;
pub const API_MAX_HOURS: f64 = 48.0;

/// POST /api/v1/sim: run a `ck sim` scenario and return what `ck sim` writes: {"summary", "series"} (series = the
/// timeline.csv text). Err = the reason (400). Caps: nodes <= 20,000, simulated hours <= 48; a day run is also bounded
/// to 3e8 PC-ticks so one request cannot spin for minutes. ponytail: the job-level run is bounded by max_s only.
pub fn run_api(v: &Value) -> Result<Value, String> {
    if !v.is_object() {
        return Err("the body must be a scenario object".into());
    }
    let day = v.get("day").is_some();
    let count = |k: &str| v[k].as_array().map_or(1.0, |a| a.iter().map(|g| f(g, "count", 1.0).max(0.0)).sum::<f64>());
    let nodes = count(if day { "pcs" } else { "nodes" });
    if nodes > API_MAX_NODES {
        return Err(format!("at most {} nodes per run (this has {nodes})", API_MAX_NODES as u64));
    }
    if day {
        let (h, dt) = (f(&v["day"], "hours", 24.0), f(&v["day"], "tick_s", 10.0).max(0.5));
        if h > API_MAX_HOURS {
            return Err(format!("at most {} simulated hours per run (day.hours is {h})", API_MAX_HOURS as u64));
        }
        if nodes * h * 3600.0 / dt > 3e8 {
            return Err("too many PC-ticks: raise day.tick_s or lower the PCs or hours".into());
        }
        let mut r = day_fn(v);
        let series = r["_timeline"].take();
        return Ok(json!({"summary": r, "series": series}));
    }
    let s = Scn::parse(&v.to_string())?;
    if s.max_s > API_MAX_HOURS * 3600.0 {
        return Err(format!("max_s is over {} hours", API_MAX_HOURS as u64));
    }
    let r = simulate(&s);
    let mut csv = String::from("t_s,done,rate_per_s,busy_nodes,connected_nodes,head_busy_frac\n");
    for r in &r.timeline {
        csv += &format!("{:.3},{},{:.1},{},{},{:.4}\n", r[0], r[1], r[2], r[3], r[4], r[5]);
    }
    Ok(json!({"summary": r.summary(), "series": csv}))
}

fn day_fn(v: &Value) -> Value {
    day(v)
}

pub fn run(o: &Opts) {
    let path = o.rest.first().unwrap_or_else(|| die("usage: ck sim SCENARIO.json [--out DIR]"));
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let dir = std::env::var("CK_SIM_OUT").unwrap_or_else(|_| "sim-out".into());
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(&format!("{dir}: {e}")));
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if v.get("hier_study").is_some() {
            return hier_study(&dir);
        }
        if let Some(h) = v.get("hier") {
            let o = hsim(&HScn::from_json(h));
            let sm = o.summary();
            std::fs::write(format!("{dir}/summary.json"), serde_json::to_string_pretty(&sm).unwrap()).unwrap_or_else(|e| die(&e.to_string()));
            return println!("{sm}");
        }
        if v.get("day").is_some() {
            let t0 = std::time::Instant::now();
            let mut r = day(&v);
            let tl = r["_timeline"].take();
            std::fs::write(format!("{dir}/timeline.csv"), tl.as_str().unwrap_or("")).unwrap_or_else(|e| die(&e.to_string()));
            std::fs::write(format!("{dir}/summary.json"), serde_json::to_string_pretty(&r).unwrap()).unwrap_or_else(|e| die(&e.to_string()));
            println!("day: {:.2} kWh, {} PC failures, {} head failures, {} wakes; simulated in {:.2} s wall; files in {dir}/", r["kwh"].as_f64().unwrap_or(0.0), r["node_failures"], r["head_failures"], r["wakes"], t0.elapsed().as_secs_f64());
            return;
        }
    }
    let s = Scn::parse(&text).unwrap_or_else(|e| die(&e));
    let t0 = std::time::Instant::now();
    let r = simulate(&s);
    let mut csv = String::from("t_s,done,rate_per_s,busy_nodes,connected_nodes,head_busy_frac\n");
    for r in &r.timeline {
        csv += &format!("{:.3},{},{:.1},{},{},{:.4}\n", r[0], r[1], r[2], r[3], r[4], r[5]);
    }
    std::fs::write(format!("{dir}/timeline.csv"), csv).unwrap_or_else(|e| die(&e.to_string()));
    let sm = r.summary();
    std::fs::write(format!("{dir}/summary.json"), serde_json::to_string_pretty(&sm).unwrap()).unwrap_or_else(|e| die(&e.to_string()));
    let outcome = if r.finished {
        format!("makespan {:.1} s (ideal {:.1} s, efficiency {:.1} %)", r.end_s, r.ideal_s, 100.0 * r.ideal_s / r.end_s)
    } else {
        format!("UNFINISHED at {:.1} s: {} of {} done (ideal {:.1} s)", r.end_s, r.done, r.n, r.ideal_s)
    };
    println!(
        "{outcome}; {} tasks; head utilisation {:.1} %; {} node failures; recovered work {}; duplicate work 0. Simulated in {:.2} s wall; files in {dir}/",
        r.tasks, 100.0 * r.head_util, r.failures, r.recovered, t0.elapsed().as_secs_f64()
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Day mode: a scenario with a "day" key simulates hours of a whole lab at job level (tick-based, not per chunk):
// several jobs with priority / max_pcs / placement (sched::place), class reservations, eco power (sleep, wake
// latency, prewake), per-PC power draw and energy, PC failures (MTBF) and head failures. The per-chunk costs measured
// with the chunk-level simulator enter as `efficiency` and `chunk_s`.
//
// {
//   "day": {"hours": 24, "tick_s": 10, "sample_s": 300, "seed": 1, "efficiency": 0.97, "chunk_s": 1},
//   "pcs": [{"count": 200, "rate": 23.3e6, "speed_cv": 0.03, "run_cv": 0.005, "mtbf_s": 0, "down_s": 180, "threads": 12}],
//   "power": {"idle_w": 25, "busy_w": 65, "sleep_w": 2, "boot_w": 65, "interactive_w": 45, "wake_s": 45},   (assumptions)
//   "eco": {"enabled": false, "idle_sleep_s": 600, "prewake_s": 120, "keep_awake": 0},
//   "reservations": [{"start_h": 9, "end_h": 11, "first_pc": 0, "pcs": 60}],            class: PCs blocked for jobs
//   "jobs": [{"name": "pow", "arrive_h": 0, "work": 0, "priority": 4, "max_pcs": 0, "placement": "spread"}],   work 0 = endless
//   "head": {"mtbf_s": 0, "standby": true, "recovery_s": 5.7, "restart_s": 600}
// }
// Output: summary.json (energy by state, per-job rows, class delay) and timeline.csv.
struct Pc {
    rate: f64,
    threads: u32,
    mtbf: f64,
    down: f64,
    // 0 asleep, 1 booting, 2 awake
    st: u8,
    boot_end: f64,
    down_until: f64,
    idle_since: f64,
    job: Option<usize>,
}
struct DJob {
    name: String,
    arrive: f64,
    work: f64, // 0 = endless
    left: f64,
    pri: u32,
    max_pcs: u32,
    place: String,
    started: Option<f64>,
    finished: Option<f64>,
    pc_s: f64,
    busy_wh: f64,
    done: f64,
    peak: usize,
    holders: Vec<bool>,
}

pub fn day(v: &Value) -> Value {
    let d = &v["day"];
    let (hours, dt, sample, eff, chunk_s) = (f(d, "hours", 24.0), f(d, "tick_s", 10.0).max(0.5), f(d, "sample_s", 300.0), f(d, "efficiency", 0.97), f(d, "chunk_s", 1.0));
    let mut rng = Rng(f(d, "seed", 1.0) as u64);
    let p = &v["power"];
    let (idle_w, busy_w, sleep_w, boot_w, int_w, wake_s) = (f(p, "idle_w", W_IDLE), f(p, "busy_w", W_BUSY), f(p, "sleep_w", W_SLEEP), f(p, "boot_w", W_BOOT), f(p, "interactive_w", W_INTERACTIVE), f(p, "wake_s", 45.0));
    let e = &v["eco"];
    let (eco, idle_sleep, prewake, keep) = (e["enabled"].as_bool().unwrap_or(false), f(e, "idle_sleep_s", 600.0), f(e, "prewake_s", 120.0), f(e, "keep_awake", 0.0) as usize);
    let h = &v["head"];
    let (h_mtbf, h_rec, h_restart, h_sb) = (f(h, "mtbf_s", 0.0), f(h, "recovery_s", 5.7), f(h, "restart_s", 600.0), h["standby"].as_bool().unwrap_or(true));
    let mut pcs: Vec<Pc> = vec![];
    for g in v["pcs"].as_array().cloned().unwrap_or_else(|| vec![json!({})]) {
        for _ in 0..f(&g, "count", 1.0) as usize {
            let r = f(&g, "rate", 1e6) * rng.mult(f(&g, "speed_cv", 0.0)) * rng.mult(f(&g, "run_cv", 0.0));
            pcs.push(Pc { rate: r, threads: f(&g, "threads", 12.0) as u32, mtbf: f(&g, "mtbf_s", 0.0), down: f(&g, "down_s", 180.0), st: 2, boot_end: 0.0, down_until: 0.0, idle_since: 0.0, job: None });
        }
    }
    let n = pcs.len();
    let mut jobs: Vec<DJob> = v["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|j| DJob {
            name: j["name"].as_str().unwrap_or("job").to_string(),
            arrive: if j.get("arrive_s").is_some() { f(j, "arrive_s", 0.0) } else { f(j, "arrive_h", 0.0) * 3600.0 },
            work: f(j, "work", 0.0),
            left: f(j, "work", 0.0),
            pri: f(j, "priority", 2.0) as u32,
            max_pcs: f(j, "max_pcs", 0.0) as u32,
            place: j["placement"].as_str().unwrap_or("spread").to_string(),
            started: None,
            finished: None,
            pc_s: 0.0,
            busy_wh: 0.0,
            done: 0.0,
            peak: 0,
            holders: vec![false; n],
        })
        .collect();
    let res: Vec<(f64, f64, usize, usize)> = v["reservations"].as_array().cloned().unwrap_or_default().iter().map(|r| (f(r, "start_h", 0.0) * 3600.0, f(r, "end_h", 0.0) * 3600.0, f(r, "first_pc", 0.0) as usize, f(r, "pcs", 0.0) as usize)).collect();
    let end = hours * 3600.0;
    let ids: Vec<String> = (0..n).map(|i| format!("{i:05}")).collect();
    let (mut t, mut next_s) = (0.0, 0.0);
    let (mut e_busy, mut e_idle, mut e_sleep, mut e_boot, mut e_int) = (0.0, 0.0, 0.0, 0.0, 0.0); // Wh
    let (mut node_fail, mut head_fail, mut lost_work, mut wakes, mut class_delay_pc_s, mut class_delay_max) = (0u64, 0u64, 0.0f64, 0u64, 0.0f64, 0.0f64);
    let mut head_down_until = 0.0f64;
    let mut tl = String::from("t_s,asleep,booting,awake_idle,busy,class,power_w\n");
    let (mut acc_n, mut acc) = (0.0, [0.0f64; 6]);
    let mut blocked = vec![false; n];
    let mut want_awake = vec![false; n];
    while t < end {
        // failures
        for i in 0..n {
            if pcs[i].st == 2 && pcs[i].mtbf > 0.0 && t >= pcs[i].down_until && rng.u() < 1.0 - (-dt / pcs[i].mtbf).exp() {
                node_fail += 1;
                pcs[i].down_until = t + pcs[i].down;
                if let Some(j) = pcs[i].job.take() {
                    let lost = pcs[i].rate * eff * chunk_s * 0.5; // the chunk it held is redone
                    if jobs[j].work > 0.0 {
                        jobs[j].left += lost;
                        jobs[j].done -= lost;
                    }
                    lost_work += lost;
                }
            }
        }
        if h_mtbf > 0.0 && t >= head_down_until && rng.u() < 1.0 - (-dt / h_mtbf).exp() {
            head_fail += 1;
            head_down_until = t + if h_sb { h_rec } else { h_restart };
        }
        // who is blocked by a class, who must be awake
        blocked.iter_mut().for_each(|b| *b = false);
        want_awake.iter_mut().for_each(|b| *b = false);
        for &(s0, s1, f0, c) in &res {
            for i in f0..(f0 + c).min(n) {
                if t >= s0 && t < s1 {
                    blocked[i] = true;
                }
                if t >= s0 - prewake && t < s1 {
                    want_awake[i] = true;
                }
            }
        }
        // power state transitions
        for i in 0..n {
            let pc = &mut pcs[i];
            if pc.st == 1 && t >= pc.boot_end {
                pc.st = 2;
                pc.idle_since = t;
            }
            if pc.st == 0 && want_awake[i] {
                pc.st = 1;
                pc.boot_end = t + wake_s;
                wakes += 1;
            }
            if blocked[i] || pc.job.is_some() || pc.st != 2 {
                pc.idle_since = t;
            }
            if blocked[i] {
                pc.job = None;
            }
        }
        // assignment, jobs in priority then arrival order
        let mut order: Vec<usize> = (0..jobs.len()).filter(|&j| jobs[j].arrive <= t && jobs[j].finished.is_none()).collect();
        order.sort_by(|&a, &b| jobs[a].pri.cmp(&jobs[b].pri).then(jobs[a].arrive.total_cmp(&jobs[b].arrive)));
        let hf = 1.0 - (head_down_until - t).clamp(0.0, dt) / dt; // share of this tick the head is up
        let head_ok = hf > 0.0;
        let mut taken = vec![false; n];
        let mut assigned: Vec<Vec<usize>> = vec![vec![]; jobs.len()];
        for &j in &order {
            let cands: Vec<sched::PlaceNode> = (0..n).filter(|&i| pcs[i].st == 2 && t >= pcs[i].down_until && !blocked[i] && !taken[i]).map(|i| sched::PlaceNode { node_id: ids[i].clone(), rate: pcs[i].rate, threads: pcs[i].threads, holds: jobs[j].holders[i], cached: false }).collect();
            for id in sched::place(&jobs[j].place, jobs[j].max_pcs, &cands) {
                let i: usize = id.parse().unwrap();
                taken[i] = true;
                assigned[j].push(i);
            }
            // eco: wake sleepers this job would use (place over its awake-and-asleep candidates, the real function)
            if eco {
                let hold = assigned[j].len();
                let wanted = if jobs[j].max_pcs == 0 { n } else { jobs[j].max_pcs as usize };
                let booting = (0..n).filter(|&i| pcs[i].st == 1 && !blocked[i]).count();
                if hold + booting < wanted {
                    let all: Vec<sched::PlaceNode> = (0..n).filter(|&i| !blocked[i] && !taken[i] && pcs[i].st == 0).map(|i| sched::PlaceNode { node_id: ids[i].clone(), rate: pcs[i].rate, threads: pcs[i].threads, holds: false, cached: false }).collect();
                    let need = (wanted - hold - booting) as u32;
                    for id in sched::place(&jobs[j].place, if jobs[j].max_pcs == 0 { 0 } else { need }, &all).into_iter().take(need as usize) {
                        let i: usize = id.parse().unwrap();
                        pcs[i].st = 1;
                        pcs[i].boot_end = t + wake_s;
                        wakes += 1;
                    }
                }
            }
        }
        // progress, energy
        let (mut busy, mut cls, mut idle, mut boot, mut asleep) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut busy_frac = vec![0.0f64; n];
        for &j in &order {
            let jb = &mut jobs[j];
            for h in jb.holders.iter_mut() {
                *h = false;
            }
            let rate: f64 = assigned[j].iter().map(|&i| pcs[i].rate).sum::<f64>() * eff * hf;
            for &i in &assigned[j] {
                jb.holders[i] = true;
                pcs[i].job = Some(j);
            }
            if assigned[j].is_empty() || !head_ok {
                continue;
            }
            jb.started.get_or_insert(t);
            jb.peak = jb.peak.max(assigned[j].len());
            let used = if jb.work > 0.0 && rate * dt > jb.left { jb.left / rate } else { dt };
            jb.left -= rate * used;
            jb.done += rate * used;
            jb.pc_s += assigned[j].len() as f64 * used;
            if jb.work > 0.0 && jb.left <= 1e-9 {
                jb.finished = Some(t + used + chunk_s); // + the tail: the last chunk finishing
            }
            for &i in &assigned[j] {
                busy_frac[i] = used / dt;
                jb.busy_wh += (busy_w - idle_w) * used / 3600.0;
            }
        }
        let mut awake_cnt = pcs.iter().filter(|p| p.st != 0).count();
        for i in 0..n {
            let pc = &mut pcs[i];
            if !taken[i] || !head_ok {
                if pc.job.is_some() && !taken[i] {
                    pc.job = None;
                }
            }
            let hrs = dt / 3600.0;
            match pc.st {
                0 => {
                    asleep += 1;
                    e_sleep += sleep_w * hrs;
                }
                1 => {
                    boot += 1;
                    e_boot += boot_w * hrs;
                    if blocked[i] {
                        class_delay_pc_s += dt;
                        class_delay_max = class_delay_max.max(pc.boot_end - t);
                    }
                }
                _ => {
                    if blocked[i] {
                        cls += 1;
                        e_int += int_w * hrs;
                    } else if taken[i] && head_ok {
                        busy += 1;
                        e_busy += (busy_w * busy_frac[i] + idle_w * (1.0 - busy_frac[i])) * hrs;
                    } else {
                        idle += 1;
                        e_idle += idle_w * hrs;
                        // eco: sleep a PC that has been idle long enough (never one a class needs soon)
                        if eco && !want_awake[i] && t - pc.idle_since >= idle_sleep && t >= pc.down_until && awake_cnt > keep {
                            pc.st = 0;
                            awake_cnt -= 1;
                        }
                    }
                }
            }
        }
        // a class PC that is asleep when the class starts is a delay
        for i in 0..n {
            if blocked[i] && pcs[i].st == 0 {
                class_delay_pc_s += dt;
            }
        }
        let pw = busy as f64 * busy_w + idle as f64 * idle_w + asleep as f64 * sleep_w + boot as f64 * boot_w + cls as f64 * int_w;
        acc_n += 1.0;
        for (a, x) in acc.iter_mut().zip([asleep, boot, idle, busy, cls].iter().map(|&x| x as f64).chain([pw])) {
            *a += x;
        }
        next_s += dt;
        if next_s >= sample {
            tl += &format!("{:.0},{:.1},{:.1},{:.1},{:.1},{:.1},{:.0}\n", t, acc[0] / acc_n, acc[1] / acc_n, acc[2] / acc_n, acc[3] / acc_n, acc[4] / acc_n, acc[5] / acc_n);
            (next_s, acc_n, acc) = (0.0, 0.0, [0.0; 6]);
        }
        t += dt;
    }
    let kwh = (e_busy + e_idle + e_sleep + e_boot + e_int) / 1e3;
    let rows: Vec<Value> = jobs.iter().map(|j| json!({"name": j.name, "arrive_s": j.arrive, "started_s": j.started, "finished_s": j.finished, "wait_s": j.started.map(|s| s - j.arrive),
        "makespan_s": j.finished.map(|f| f - j.arrive), "run_s": j.finished.zip(j.started).map(|(f, s)| f - s), "pc_seconds": j.pc_s, "peak_pcs": j.peak, "done": j.done, "left": j.left, "marginal_busy_kwh": j.busy_wh / 1e3})).collect();
    json!({"kwh": kwh, "kwh_busy": e_busy / 1e3, "kwh_idle": e_idle / 1e3, "kwh_sleep": e_sleep / 1e3, "kwh_boot": e_boot / 1e3, "kwh_class": e_int / 1e3,
        "node_failures": node_fail, "head_failures": head_fail, "lost_work": lost_work, "wakes": wakes, "class_delay_pc_s": class_delay_pc_s, "class_delay_max_s": class_delay_max,
        "hours": hours, "jobs": rows, "_timeline": tl})
}

// ---------------------------------------------------------------------------------------------------------------
// Hierarchy mode (docs/sim/HIERARCHY.md): root head + `lab` sub-heads. A lab head asks the root for a LEASE (a big range,
// `lease_s` seconds of the lab's work), chunks it for its own PCs with the same head cost model, merges the results and
// reports the whole lease back in one message. The root sees each lab head as one worker. Work is counted in PC-seconds at
// nominal speed (1 unit = 1 s of one average PC); a chunk is `chunk_s` units. Every constant marked ASSUMPTION in the doc
// is a field here. Not modelled: PC failures, jitter, relay batching, lab-head restart (PCs stay on the root after a kill).
#[derive(Clone)]
pub struct HScn {
    pub pcs: usize,
    pub lab_pcs: usize,       // PCs per lab (also the size of the partitioned group in 1-level runs)
    pub two_level: bool,
    pub speed_cv: f64,
    pub chunk_s: f64,
    pub n: f64,               // job size in PC-seconds
    pub lease_s: f64,         // L: seconds of lab work per lease
    pub refill: f64,          // ASSUMPTION: a lab asks for the next lease when its pool drops under refill x L of work
    pub ttl_s: f64,           // ASSUMPTION: root fences all leases of a lab head it has not heard from for ttl_s (lab heartbeats every hb_s)
    pub hb_s: f64,            // ASSUMPTION: lab head heartbeat period; its root cost is added analytically (the heartbeats are not events)
    pub lat: f64,             // PC <-> its head, one way
    pub wan: f64,             // lab head <-> root, one way
    pub svc: f64,             // head time per message (s), the calibrated 379 us ...
    pub svc_agent: f64,       // ... + 0.199 us per connected agent
    pub merge: f64,           // ASSUMPTION: lab head time to merge one chunk result
    pub root_merge: f64,      // ASSUMPTION: root time per chunk in a lease report
    pub inflight: u32,
    pub reconnect: f64,       // ASSUMPTION: PCs of a dead lab head reach the root after reconnect x (1 .. 1.5)
    pub kill_lab: Option<(usize, f64)>,        // (lab, at)
    pub root_kill: Option<(f64, f64)>,         // (at, outage): standby 5.7 s, or a manual restart
    pub part: Option<(usize, f64, f64)>,       // (lab, at, length): lab cut off from the root
    pub seed: u64,
    pub max_s: f64,
    pub warm_s: f64,          // rates are measured after this time
}

impl Default for HScn {
    fn default() -> Self {
        HScn { pcs: 200, lab_pcs: 50, two_level: false, speed_cv: 0.03, chunk_s: 1.0, n: 1e12, lease_s: 30.0, refill: 0.5, ttl_s: 90.0, hb_s: 2.0, lat: 3e-4, wan: 5e-4, svc: 379e-6, svc_agent: 0.199e-6, merge: 20e-6, root_merge: 2e-6, inflight: 2, reconnect: 1.5, kill_lab: None, root_kill: None, part: None, seed: 1, max_s: 86400.0, warm_s: 0.0 }
    }
}

impl HScn {
    pub fn from_json(h: &Value) -> HScn {
        let d = HScn::default();
        let opt = |k: &str| h.get(k).and_then(Value::as_array).map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect::<Vec<_>>());
        HScn {
            pcs: f(h, "pcs", d.pcs as f64) as usize,
            lab_pcs: f(h, "lab_pcs", d.lab_pcs as f64).max(1.0) as usize,
            two_level: h["two_level"].as_bool().unwrap_or(false),
            speed_cv: f(h, "speed_cv", d.speed_cv),
            chunk_s: f(h, "chunk_s", d.chunk_s).max(1e-3),
            n: f(h, "n", d.n),
            lease_s: f(h, "lease_s", d.lease_s),
            refill: f(h, "refill", d.refill),
            ttl_s: f(h, "ttl_s", d.ttl_s),
            hb_s: f(h, "hb_s", d.hb_s).max(1e-3),
            lat: f(h, "latency_ms", d.lat * 1e3) / 1e3,
            wan: f(h, "wan_ms", d.wan * 1e3) / 1e3,
            svc: f(h, "service_us", d.svc * 1e6) / 1e6,
            svc_agent: f(h, "service_us_per_agent", d.svc_agent * 1e6) / 1e6,
            merge: f(h, "merge_us", d.merge * 1e6) / 1e6,
            root_merge: f(h, "root_merge_us", d.root_merge * 1e6) / 1e6,
            inflight: f(h, "inflight", 2.0).max(1.0) as u32,
            reconnect: f(h, "reconnect_s", d.reconnect),
            kill_lab: opt("kill_lab").filter(|a| a.len() == 2).map(|a| (a[0] as usize, a[1])),
            root_kill: opt("root_kill").filter(|a| a.len() == 2).map(|a| (a[0], a[1])),
            part: opt("partition").filter(|a| a.len() == 3).map(|a| (a[0] as usize, a[1], a[2])),
            seed: f(h, "seed", 1.0) as u64,
            max_s: f(h, "max_s", d.max_s),
            warm_s: f(h, "warm_s", 0.0),
        }
    }
}

pub struct HOut {
    pub finished: bool,
    pub end_s: f64,
    pub ideal_s: f64,
    pub done: f64,
    pub tasks_s: f64,        // chunk results processed per second by all heads, after warm_s
    pub root_cpu: f64,       // busy fraction of the root's one service thread, after warm_s
    pub root_msgs_s: f64,
    pub lab_cpu_max: f64,
    pub redone: f64,         // units the root took back from an expired lease and gave out again
    pub wasted: f64,         // units computed for a fenced lease or lost with a dead lab head (computed twice)
    pub lost_with_lab: f64,
    pub part_units: f64,     // units finished by the PCs of the partitioned lab inside the partition window
}

impl HOut {
    pub fn summary(&self) -> Value {
        json!({"finished": self.finished, "makespan_s": self.finished.then_some(self.end_s), "end_s": self.end_s, "ideal_s": self.ideal_s, "done": self.done,
            "tasks_per_s": self.tasks_s, "root_cpu_pct": 100.0 * self.root_cpu, "root_msgs_per_s": self.root_msgs_s, "lab_cpu_max_pct": 100.0 * self.lab_cpu_max,
            "redone_units": self.redone, "wasted_units": self.wasted, "lost_with_lab_units": self.lost_with_lab, "partition_window_units": self.part_units})
    }
}

#[derive(Clone, Copy)]
enum HK {
    Arr { pc: usize, ep: u32, c: u64 }, // a request (c = 0) or a chunk result reaches the PC's head
    Fin { pc: usize, ep: u32, c: u64 },
    Req { lab: usize },                 // lease request reaches the root
    Rep { l: usize },                   // lease report reaches the root
    Grant { lab: usize, l: usize },     // lease (or NONE) reaches the lab head
    Fence { lab: usize },               // root gives up on a silent lab head: its unreported leases go back to the pool
    Kill { lab: usize },
    Back { pc: usize, ep: u32 },        // a PC of a dead lab head reaches the root
    Poll { lab: usize },
}
const NONE: usize = usize::MAX;

struct HPc {
    speed: f64,
    lab: usize,
    srv: usize, // 0 = root, 1 + lab = its lab head
    ep: u32,
    used: u32,
    busy_until: f64,
    idle: bool,
}
struct HLab {
    alive: bool,
    pcs: Vec<usize>,
    pool: VecDeque<usize>,
    pool_u: f64,
    idle: Vec<usize>,
    req_out: bool,
    hold: f64,
    cap: f64,
}
struct Lease {
    lab: usize,
    units: f64,
    disp: f64,
    done: f64,
    nch: u32,
    closed: bool,
    fenced: bool,
}

struct HSim<'a> {
    s: &'a HScn,
    pcs: Vec<HPc>,
    labs: Vec<HLab>,
    leases: Vec<Lease>,
    heap: BinaryHeap<Ev<HK>>,
    seq: u64,
    free: Vec<f64>,
    busy: Vec<f64>,
    pending: f64,
    done: f64,
    chunks: HashMap<u64, (usize, f64, usize)>, // id -> (pc, len, lease)
    next: u64,
    root_conn: usize,
    alive_labs: usize,
    root_idle: Vec<usize>,
    tasks: u64,
    root_msgs: u64,
    redone: f64,
    wasted: f64,
    lost: f64,
    part_u: f64,
    finish: Option<f64>,
}

impl HSim<'_> {
    fn push(&mut self, t: f64, k: HK) {
        self.seq += 1;
        self.heap.push(Ev { t, seq: self.seq, k });
    }
    /// a message between lab `lab` and the root, sent at t, waits for the end of the partition
    fn cutd(&self, lab: usize, t: f64) -> f64 {
        match self.s.part {
            Some((l, p0, len)) if l == lab && t >= p0 && t < p0 + len => p0 + len,
            _ => t,
        }
    }
    fn serve(&mut self, srv: usize, t: f64, cost: f64) -> f64 {
        let mut st = t.max(self.free[srv]);
        if let (0, Some((at, rec))) = (srv, self.s.root_kill) {
            if st >= at && st < at + rec {
                st = at + rec; // the standby is promoting (or the root restarts); the queue waits
            }
        }
        let fin = st + cost;
        self.free[srv] = fin;
        self.busy[srv] += cost;
        fin
    }
    fn send(&mut self, pc: usize, t: f64, c: u64) {
        let (ep, srv, lab) = (self.pcs[pc].ep, self.pcs[pc].srv, self.pcs[pc].lab);
        let t = if srv == 0 && !self.s.two_level { self.cutd(lab, t) } else { t }; // 1-level: the cut lab's PCs talk to the root
        self.push(t + self.s.lat, HK::Arr { pc, ep, c });
    }
    fn request(&mut self, pc: usize, t: f64) {
        self.pcs[pc].used += 1;
        self.send(pc, t, 0);
    }
    fn assign(&mut self, pc: usize, fin: f64, len: f64, l: usize) {
        self.next += 1;
        let id = self.next;
        self.chunks.insert(id, (pc, len, l));
        let p = &mut self.pcs[pc];
        p.used += 1;
        let end = (fin + self.s.lat).max(p.busy_until) + len / p.speed;
        p.busy_until = end;
        let ep = p.ep;
        self.push(end, HK::Fin { pc, ep, c: id });
    }
    /// work is available at lab `lab` (or the root): ask idle slots for it
    fn wake(&mut self, t: f64, lab: Option<usize>) {
        let avail = lab.map_or(self.pending, |l| self.labs[l].pool_u);
        let mut k = (avail / self.s.chunk_s).ceil() as usize;
        let list = match lab {
            Some(l) => std::mem::take(&mut self.labs[l].idle),
            None => std::mem::take(&mut self.root_idle),
        };
        let mut keep = vec![];
        for pc in list {
            if k == 0 {
                keep.push(pc);
                continue;
            }
            self.pcs[pc].idle = false;
            while k > 0 && self.pcs[pc].used < self.s.inflight {
                self.request(pc, t);
                k -= 1;
            }
            if self.pcs[pc].used < self.s.inflight {
                self.pcs[pc].idle = true;
                keep.push(pc);
            }
        }
        match lab {
            Some(l) => self.labs[l].idle = keep,
            None => self.root_idle = keep,
        }
    }
    fn fill_root(&mut self, pc: usize, t: f64) {
        while self.pcs[pc].used < self.s.inflight && self.pending > 1e-9 {
            self.request(pc, t);
        }
        if self.pcs[pc].used < self.s.inflight && !self.pcs[pc].idle {
            self.pcs[pc].idle = true;
            self.root_idle.push(pc);
        }
    }
    /// lab head asks the root for the next lease when its pool runs low
    fn refill(&mut self, lab: usize, t: f64) {
        let lb = &self.labs[lab];
        if lb.alive && !lb.req_out && t >= lb.hold && self.finish.is_none() && lb.pool_u < self.s.refill * self.s.lease_s * lb.cap {
            self.labs[lab].req_out = true;
            let at = self.cutd(lab, t + self.s.wan);
            self.push(at, HK::Req { lab });
        }
    }
    fn arr(&mut self, t: f64, pc: usize, ep: u32, c: u64) {
        if self.pcs[pc].ep != ep {
            return; // the PC lost its lab head since it sent this
        }
        let s = self.s;
        let srv = self.pcs[pc].srv;
        self.pcs[pc].used -= 1;
        let (len, lease) = if c != 0 { let (_, l, ls) = self.chunks.remove(&c).unwrap(); (l, ls) } else { (0.0, NONE) };
        if srv == 0 {
            let fin = self.serve(0, t, s.svc + s.svc_agent * self.root_conn as f64);
            self.root_msgs += 1;
            if c != 0 {
                self.done += len;
                self.tasks += 1;
                if self.done >= s.n - 1e-6 {
                    self.finish = Some(fin);
                    return;
                }
            }
            if self.pending > 1e-9 {
                let l = self.pending.min(s.chunk_s);
                self.pending -= l;
                self.assign(pc, fin, l, NONE);
            } else if !self.pcs[pc].idle {
                self.pcs[pc].idle = true;
                self.root_idle.push(pc);
            }
            return;
        }
        let lab = srv - 1;
        let fin = self.serve(srv, t, s.svc + s.svc_agent * self.labs[lab].pcs.len() as f64 + if c != 0 { s.merge } else { 0.0 });
        if c != 0 {
            self.tasks += 1;
            let ls = &mut self.leases[lease];
            ls.done += len;
            ls.nch += 1;
            if ls.done >= ls.units - 1e-6 {
                let at = self.cutd(lab, fin + s.wan); // one merged report for the whole lease
                self.push(at, HK::Rep { l: lease });
            }
        }
        while let Some(&l) = self.labs[lab].pool.front() {
            if self.leases[l].disp >= self.leases[l].units - 1e-9 {
                self.labs[lab].pool.pop_front();
            } else {
                break;
            }
        }
        if let Some(&l) = self.labs[lab].pool.front() {
            let ls = &mut self.leases[l];
            let len = (ls.units - ls.disp).min(s.chunk_s);
            ls.disp += len;
            self.labs[lab].pool_u -= len;
            self.assign(pc, fin, len, l);
        } else if !self.pcs[pc].idle {
            self.pcs[pc].idle = true;
            self.labs[lab].idle.push(pc);
        }
        self.refill(lab, fin);
    }
    fn root_free_cost(&self, extra: f64) -> f64 {
        self.s.svc + self.s.svc_agent * self.root_conn as f64 + extra
    }
    fn on(&mut self, t: f64, k: HK) {
        let s = self.s;
        match k {
            HK::Arr { pc, ep, c } => self.arr(t, pc, ep, c),
            HK::Fin { pc, ep, c } => {
                if self.pcs[pc].ep == ep {
                    if let Some((l, p0, len)) = s.part {
                        if self.pcs[pc].lab == l && t >= p0 && t < p0 + len {
                            self.part_u += self.chunks[&c].1;
                        }
                    }
                    self.send(pc, t, c);
                }
            }
            HK::Req { lab } => {
                if !self.labs[lab].alive {
                    return;
                }
                let fin = self.serve(0, t, self.root_free_cost(0.0));
                self.root_msgs += 1;
                let reply = self.cutd(lab, fin + s.wan);
                if self.pending > 1e-9 {
                    // ASSUMPTION: guided lease size, never more than half the unleased work per live lab (keeps the tail short)
                    let want = (s.lease_s * self.labs[lab].cap).min(self.pending / (2.0 * self.alive_labs.max(1) as f64)).max(s.chunk_s.min(self.pending));
                    let units = want.min(self.pending);
                    self.pending -= units;
                    let l = self.leases.len();
                    self.leases.push(Lease { lab, units, disp: 0.0, done: 0.0, nch: 0, closed: false, fenced: false });
                    self.push(reply, HK::Grant { lab, l });
                } else {
                    self.push(reply, HK::Grant { lab, l: NONE });
                }
            }
            HK::Grant { lab, l } => {
                if !self.labs[lab].alive {
                    return;
                }
                self.labs[lab].req_out = false;
                if l == NONE {
                    self.labs[lab].hold = t + 0.5;
                    return self.push(t + 0.5, HK::Poll { lab });
                }
                let u = self.leases[l].units;
                self.labs[lab].pool.push_back(l);
                self.labs[lab].pool_u += u;
                self.wake(t, Some(lab));
                self.refill(lab, t);
            }
            HK::Poll { lab } => self.refill(lab, t),
            HK::Rep { l, .. } => {
                let fenced = self.leases[l].fenced;
                self.leases[l].closed |= !fenced;
                let fin = self.serve(0, t, self.root_free_cost(s.root_merge * self.leases[l].nch as f64));
                self.root_msgs += 1;
                let u = self.leases[l].units;
                if fenced {
                    self.wasted += u; // computed, but the root had already given the range away
                } else {
                    self.done += u;
                    if self.done >= s.n - 1e-6 {
                        self.finish = Some(fin);
                    }
                }
            }
            HK::Fence { lab } => {
                if let Some((at, rec)) = s.root_kill.filter(|&(at, rec)| t >= at && t < at + rec) {
                    return self.push(at + rec + 5.0, HK::Fence { lab }); // timers do not run while the root is down
                }
                for l in 0..self.leases.len() {
                    let ls = &mut self.leases[l];
                    if ls.lab == lab && !ls.closed && !ls.fenced {
                        ls.fenced = true;
                        self.pending += ls.units;
                        self.redone += ls.units;
                    }
                }
                self.wake(t, None);
            }
            HK::Kill { lab } => {
                if !self.labs[lab].alive {
                    return;
                }
                self.labs[lab].alive = false;
                self.alive_labs -= 1;
                self.root_conn -= 1;
                for ls in self.leases.iter().filter(|ls| ls.lab == lab && ls.done < ls.units - 1e-6) {
                    self.lost += ls.disp; // dispatched work of an unreported lease is gone with the lab head
                }
                self.push(t + s.ttl_s, HK::Fence { lab });
                let members = self.labs[lab].pcs.clone();
                for &pc in &members {
                    let p = &mut self.pcs[pc];
                    (p.ep, p.used, p.idle) = (p.ep + 1, 0, false);
                    let ep = p.ep;
                    let d = s.reconnect * (1.0 + 0.5 * self.back_u(pc));
                    self.push(t + d, HK::Back { pc, ep });
                }
                let gone: Vec<u64> = self.chunks.iter().filter(|(_, v)| members.binary_search(&v.0).is_ok()).map(|(k, _)| *k).collect();
                for c in gone {
                    self.chunks.remove(&c); // chunks in flight at the dead head (counted in `lost`)
                }
            }
            HK::Back { pc, ep } => {
                if self.pcs[pc].ep == ep {
                    self.pcs[pc].srv = 0;
                    self.pcs[pc].busy_until = t;
                    self.root_conn += 1;
                    self.fill_root(pc, t);
                }
            }
        }
    }
    /// deterministic per-PC value in [0,1) for reconnect jitter
    fn back_u(&self, pc: usize) -> f64 {
        Rng(self.s.seed ^ (pc as u64).wrapping_mul(0x9E37_79B9)).u()
    }
}

/// Closed-form busy share of one core (docs/sim/HIERARCHY.md section 1) for the advisor: (1-level root, 2-level root, one lab head).
/// Messages cost `svc + svc_agent x connected agents`; the root of a 2-level fleet sees one agent per lab, 2 lease messages per lease,
/// one heartbeat every `hb_s` and one batch of finished pieces (`lab_part_s`, 0.5 s) per lab; a lab head also pays `merge` per chunk.
/// (The built lab head batches, so the root does not pay `root_merge` per chunk as the study assumed.)
pub fn hier_model(pcs: usize, labs: usize, chunk_s: f64, lease_s: f64) -> (f64, f64, f64) {
    let s = HScn::default();
    let (p, l) = (pcs as f64, labs.max(1) as f64);
    let tasks = p / chunk_s.max(1e-3);
    let one = tasks * (s.svc + s.svc_agent * p);
    let root = l * (2.0 / lease_s.max(1e-3) + 1.0 / s.hb_s + 2.0 / crate::tune::get("lab_part_s").max(0.05)) * (s.svc + s.svc_agent * l);
    let lab = tasks / l * (s.svc + s.svc_agent * p / l + s.merge);
    (one.min(1.0), root.min(1.0), lab.min(1.0))
}

pub fn hsim(s: &HScn) -> HOut {
    let mut dr = Rng(s.seed ^ 0xA5A5_5A5A_1234_4321);
    let lp = s.lab_pcs.max(1);
    let nlabs = if s.two_level { s.pcs.div_ceil(lp) } else { 0 };
    let pcs: Vec<HPc> = (0..s.pcs).map(|i| HPc { speed: dr.mult(s.speed_cv), lab: i / lp, srv: if s.two_level { 1 + i / lp } else { 0 }, ep: 0, used: 0, busy_until: 0.0, idle: false }).collect();
    let fleet: f64 = pcs.iter().map(|p| p.speed).sum();
    let labs: Vec<HLab> = (0..nlabs)
        .map(|l| {
            let m: Vec<usize> = (l * lp..((l + 1) * lp).min(s.pcs)).collect();
            HLab { alive: true, cap: m.iter().map(|&i| pcs[i].speed).sum(), pcs: m, pool: VecDeque::new(), pool_u: 0.0, idle: vec![], req_out: false, hold: 0.0 }
        })
        .collect();
    let mut m = HSim { s, root_conn: if s.two_level { nlabs } else { s.pcs }, alive_labs: nlabs, pcs, labs, leases: vec![], heap: BinaryHeap::new(), seq: 0, free: vec![0.0; 1 + nlabs], busy: vec![0.0; 1 + nlabs], pending: s.n, done: 0.0, chunks: HashMap::new(), next: 0, root_idle: vec![], tasks: 0, root_msgs: 0, redone: 0.0, wasted: 0.0, lost: 0.0, part_u: 0.0, finish: None };
    if s.two_level {
        for l in 0..nlabs {
            for pc in m.labs[l].pcs.clone() {
                m.pcs[pc].idle = true;
                m.labs[l].idle.push(pc);
            }
            m.refill(l, 0.0);
        }
        if let Some((l, at, _)) = s.part.filter(|&(l, _, len)| l < nlabs && len > s.ttl_s) {
            m.push(at + s.ttl_s, HK::Fence { lab: l }); // silent longer than the timeout: the root takes the leases back
        }
        if let Some((l, at)) = s.kill_lab.filter(|&(l, _)| l < nlabs) {
            m.push(at, HK::Kill { lab: l });
        }
    } else {
        for pc in 0..s.pcs {
            m.fill_root(pc, 0.0);
        }
    }
    let (mut snap, mut now) = (None, 0.0);
    while let Some(ev) = m.heap.pop() {
        if ev.t >= s.max_s {
            now = s.max_s;
            break;
        }
        if snap.is_none() && ev.t >= s.warm_s {
            snap = Some((m.tasks, m.root_msgs, m.busy.clone()));
        }
        now = ev.t;
        m.on(ev.t, ev.k);
        if m.finish.is_some() {
            break;
        }
    }
    let end_s = m.finish.unwrap_or(now);
    let (t0, r0, b0) = snap.unwrap_or((0, 0, vec![0.0; m.busy.len()]));
    let w = (end_s - s.warm_s).max(1e-9);
    let lab_cpu_max = (1..m.busy.len()).map(|i| (m.busy[i] - b0[i]) / w).fold(0.0, f64::max).min(1.0);
    let hb = if s.two_level { m.alive_labs as f64 / s.hb_s } else { 0.0 }; // heartbeats: analytic add-on to the root load
    HOut { finished: m.finish.is_some(), end_s, ideal_s: s.n / fleet, done: m.done, tasks_s: (m.tasks - t0) as f64 / w, root_cpu: ((m.busy[0] - b0[0]) / w + hb * m.root_free_cost(0.0)).min(1.0), root_msgs_s: (m.root_msgs - r0) as f64 / w + hb, lab_cpu_max, redone: m.redone, wasted: m.wasted, lost_with_lab: m.lost, part_units: m.part_u }
}

/// The four hierarchy studies (docs/sim/HIERARCHY.md). Writes hier_*.csv into `dir` and prints the tables.
fn hier_study(dir: &str) {
    let w = |name: &str, body: String| std::fs::write(format!("{dir}/{name}"), body).unwrap_or_else(|e| die(&e.to_string()));
    let base = HScn::default();
    // (1) saturation
    let mut csv = String::from("pcs,chunk_s,arch,lab_pcs,tasks_per_s,demand_tasks_per_s,served_pct,root_cpu_pct,root_msgs_per_s,lab_cpu_max_pct\n");
    println!("(1) saturation: tasks/s (root CPU %, busiest lab CPU %)");
    println!("  PCs:                                  200 / 500 / 1000 / 2000 / 5000 / 10000");
    for chunk in [0.1, 0.5, 1.0] {
        for (arch, lp) in [("1-level", 0usize), ("2-level", 40), ("2-level", 100)] {
            let mut line = format!("  chunk {chunk:>3} s {arch} {}", if lp > 0 { format!("labs of {lp:>3}") } else { "             ".into() });
            for pcs in [200usize, 500, 1000, 2000, 5000, 10000] {
                let o = hsim(&HScn { pcs, lab_pcs: lp.max(50), two_level: lp > 0, chunk_s: chunk, max_s: 12.0, warm_s: 3.0, ..base.clone() });
                let demand = pcs as f64 / chunk;
                csv += &format!("{pcs},{chunk},{arch},{lp},{:.0},{:.0},{:.1},{:.2},{:.0},{:.1}\n", o.tasks_s, demand, 100.0 * o.tasks_s / demand, 100.0 * o.root_cpu, o.root_msgs_s, 100.0 * o.lab_cpu_max);
                line += &format!(" {:>6.0} ({:.1}%,{:.0}%)", o.tasks_s, 100.0 * o.root_cpu, 100.0 * o.lab_cpu_max);
            }
            println!("{line}");
        }
    }
    w("hier_saturation.csv", csv);
    // (2) faults in a 30-minute job on 1,000 PCs, labs of 50, 1 s chunks
    let job = |two: bool| HScn { pcs: 1000, lab_pcs: 50, two_level: two, chunk_s: 1.0, n: 1000.0 * 1800.0, ..base.clone() };
    let cases: Vec<(&str, HScn)> = vec![
        ("1-level, no fault", job(false)),
        ("1-level, root killed at 10 min, standby 5.7 s", HScn { root_kill: Some((600.0, 5.7)), ..job(false) }),
        ("1-level, root killed at 10 min, manual restart 300 s", HScn { root_kill: Some((600.0, 300.0)), ..job(false) }),
        ("2-level, no fault", job(true)),
        ("2-level, lab head killed at 10 min", HScn { kill_lab: Some((3, 600.0)), ..job(true) }),
        ("2-level, root killed at 10 min, standby 5.7 s", HScn { root_kill: Some((600.0, 5.7)), ..job(true) }),
        ("2-level, root killed at 10 min, manual restart 300 s", HScn { root_kill: Some((600.0, 300.0)), ..job(true) }),
    ];
    let mut csv = String::from("case,makespan_s,delta_vs_same_arch_no_fault_s,delta_pct,recomputed_units,regranted_units,ideal_s\n");
    println!("(2) 30-min job, 1,000 PCs");
    let mut ref_end = 0.0;
    for (name, sc) in &cases {
        let o = hsim(sc);
        if name.ends_with("no fault") {
            ref_end = o.end_s;
        }
        let d = o.end_s - ref_end;
        csv += &format!("\"{name}\",{:.2},{:.2},{:.3},{:.0},{:.0},{:.1}\n", o.end_s, d, 100.0 * d / ref_end, o.lost_with_lab, o.redone, o.ideal_s);
        println!("  {name:<56} {:>8.1} s  {:>+8.1} s  recomputed {:>6.0}  re-granted {:>6.0}", o.end_s, d, o.lost_with_lab, o.redone);
    }
    w("hier_faults.csv", csv);
    // (3) 60 s partition of lab 0 (50 PCs), averaged over 6 start times (the lease phase matters); 0.5 s chunks
    let part = |two: bool, l: f64, ttl: f64, at: f64| HScn { pcs: 1000, lab_pcs: 50, two_level: two, chunk_s: 0.5, lease_s: l, ttl_s: ttl, part: Some((0, at, 60.0)), max_s: 300.0, ..base.clone() };
    let pavg = |two: bool, l: f64, ttl: f64| {
        let (mut pct, mut wasted) = (0.0, 0.0);
        for k in 0..6 {
            let sc = part(two, l, ttl, 100.0 + 9.7 * k as f64);
            let o = hsim(&sc);
            pct += 100.0 * o.part_units / (60.0 * lab_cap(&sc)) / 6.0;
            wasted += o.wasted / 6.0;
        }
        (pct, wasted)
    };
    let mut csv = String::from("arch,lease_s,ttl_s,pct_of_full_speed_during_partition,wasted_units\n");
    println!("(3) 60 s partition of one 50-PC lab: share of its full speed that its PCs still finish inside the window (mean of 6 start times)");
    for (two, l) in [(false, 30.0), (true, 10.0), (true, 30.0), (true, 60.0), (true, 120.0)] {
        let (pct, wasted) = pavg(two, l, 90.0);
        let arch = if two { "2-level" } else { "1-level" };
        csv += &format!("{arch},{},90,{pct:.1},{wasted:.0}\n", if two { l } else { 0.0 });
        println!("  {arch} L={:>4}: {pct:>5.1} % of full speed, wasted {wasted:.0} units", if two { l } else { 0.0 });
    }
    w("hier_partition.csv", csv);
    // (4) lease length
    let mut csv = String::from("lease_s,root_cpu_pct,root_msgs_per_s,lab_kill_delta_s,lab_kill_recomputed_units,root_kill_5_7s_delta_s,root_kill_300s_delta_s,partition_window_pct\n");
    println!("(4) lease length L: root CPU, root msgs/s | lab kill: delta s (recomputed units) | root kill 5.7 s, 300 s: delta s | partition share");
    for l in [2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0] {
        let sat = hsim(&HScn { lease_s: l, max_s: 60.0, warm_s: 10.0, n: 1e12, ..job(true) });
        let lk = hsim(&HScn { lease_s: l, kill_lab: Some((3, 600.0)), ..job(true) });
        let r5 = hsim(&HScn { lease_s: l, root_kill: Some((600.0, 5.7)), ..job(true) });
        let r300 = hsim(&HScn { lease_s: l, root_kill: Some((600.0, 300.0)), ..job(true) });
        let b = hsim(&HScn { lease_s: l, ..job(true) }).end_s;
        let (pw, _) = pavg(true, l, 90.0);
        csv += &format!("{l},{:.3},{:.1},{:.2},{:.0},{:.2},{:.2},{:.1}\n", 100.0 * sat.root_cpu, sat.root_msgs_s, lk.end_s - b, lk.lost_with_lab, r5.end_s - b, r300.end_s - b, pw);
        println!("  L={l:>4}: root {:>5.2} % {:>6.1}/s | lab kill {:>+6.1} s ({:>6.0}) | root kill {:>+5.1} / {:>+6.1} s | partition {pw:>5.1} %", 100.0 * sat.root_cpu, sat.root_msgs_s, lk.end_s - b, lk.lost_with_lab, r5.end_s - b, r300.end_s - b);
    }
    w("hier_lease.csv", csv);
    // (4b) silence timeout, L = 30 s
    let mut csv = String::from("ttl_s,partition_window_pct,partition_wasted_units,lab_kill_delta_s\n");
    println!("(4b) silence timeout ttl, L = 30 s: partition share, wasted units | lab kill delta s");
    let b = hsim(&job(true)).end_s;
    for ttl in [10.0, 30.0, 90.0, 300.0] {
        let (pw, wa) = pavg(true, 30.0, ttl);
        let lk = hsim(&HScn { ttl_s: ttl, kill_lab: Some((3, 600.0)), ..job(true) });
        csv += &format!("{ttl},{pw:.1},{wa:.0},{:.2}\n", lk.end_s - b);
        println!("  ttl={ttl:>4}: partition {pw:>5.1} %, wasted {wa:>6.0} | lab kill {:>+5.1} s", lk.end_s - b);
    }
    w("hier_ttl.csv", csv);
}

/// summed speed of lab 0's PCs in a scenario (same draw as hsim)
fn lab_cap(s: &HScn) -> f64 {
    let mut dr = Rng(s.seed ^ 0xA5A5_5A5A_1234_4321);
    (0..s.pcs).map(|i| (i, dr.mult(s.speed_cv))).filter(|(i, _)| *i < s.lab_pcs).map(|(_, v)| v).sum()
}

#[cfg(test)]
mod hier_model_tests {
    #[test]
    fn model_matches_the_study_table() {
        // docs/sim/HIERARCHY.md (1): 1,000 PCs on 0.1 s chunks saturate one head; labs of 40 leave the root at under 1 %
        let (one, root, lab) = super::hier_model(1000, 25, 0.1, 30.0);
        assert!(one >= 0.99 && root < 0.05 && (0.1..0.3).contains(&lab), "{one} {root} {lab}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(j: Value) -> Out {
        simulate(&Scn::parse(&j.to_string()).unwrap())
    }
    fn base() -> Value {
        json!({"nodes": [{"count": 10, "rate": 1e5, "jitter": 0.2}], "job": {"n": 1e7}})
    }

    #[test]
    fn api_caps_and_reply() {
        let r = run_api(&base()).unwrap();
        assert!(r["summary"]["finished"].as_bool().unwrap() && r["series"].as_str().unwrap().starts_with("t_s,"));
        assert!(run_api(&json!({"nodes": [{"count": 20001}], "job": {"n": 1e6}})).unwrap_err().contains("20000"));
        assert!(run_api(&json!({"day": {"hours": 49}, "pcs": [{"count": 2}]})).unwrap_err().contains("48"));
        assert!(run_api(&json!({"day": {"hours": 1, "tick_s": 10}, "pcs": [{"count": 3, "rate": 1e6}]})).unwrap()["summary"]["kwh"].is_number());
    }

    #[test]
    fn deterministic() {
        let mut j = base();
        j["nodes"][0]["mtbf_s"] = json!(20);
        j["nodes"][0]["down_s"] = json!(5);
        j["seed"] = json!(7);
        assert_eq!(go(j.clone()).summary(), go(j.clone()).summary());
        j["seed"] = json!(8);
        assert_ne!(go(j.clone()).summary(), go(base()).summary());
    }

    #[test]
    fn conserves_work_and_recovers_from_node_failure() {
        let mut j = base();
        j["nodes"][0]["mtbf_s"] = json!(15);
        j["nodes"][0]["down_s"] = json!(5);
        let r = go(j);
        assert!(r.finished && r.done == r.n && r.pending == 0 && r.inflight == 0);
        assert!(r.failures > 0 && r.recovered > 0, "{} {}", r.failures, r.recovered);
    }

    #[test]
    fn head_failure() {
        let ok = go(base());
        let mut j = base();
        j["head_failure"] = json!({"at_s": 3, "recovery_s": 6});
        let dead = go(j.clone());
        assert!(!dead.finished && dead.done < dead.n && (dead.end_s - 3.0).abs() < 1e-9);
        j["architecture"] = json!("standby");
        let sb = go(j);
        let d = sb.end_s - ok.end_s;
        assert!(sb.finished && sb.done == sb.n && d > 3.0 && d < 7.0, "delay {d}"); // nodes keep working on their in-flight chunks
    }

    #[test]
    fn saturated_head_is_the_bottleneck_and_relays_fix_it() {
        let mut j = json!({"nodes": (0..10).map(|l| json!({"count": 200, "rate": 1e6, "lab": format!("L{l}")})).collect::<Vec<_>>(), "job": {"n": 1e10}});
        let single = go(j.clone());
        assert!(single.head_util > 0.95 && single.end_s > 2.0 * single.ideal_s);
        j["architecture"] = json!("relays");
        assert!(go(j).end_s < 0.7 * single.end_s);
    }

    // ---- calibration (SIM-04): the two measured head points and the adaptive 200-agent job ----
    #[test]
    fn head_ceiling_matches_the_measured_points() {
        for (n, lo, hi) in [(200usize, 2100.0, 2670.0), (1000, 1560.0, 1900.0)] {
            let r = go(json!({"nodes": [{"count": n, "rate": 1e6}], "job": {"n": (n * 5000 * 40) as f64, "policy": "fixed", "chunk": 5000}}));
            let tps = r.tasks as f64 / r.end_s;
            assert!(tps > lo && tps < hi, "{n} agents: {tps} tasks/s not in {lo}..{hi}");
        }
    }

    #[test]
    fn adaptive_200_agents_takes_about_11_s() {
        let r = go(json!({"nodes": [{"count": 200, "rate": 1e6}], "job": {"n": 2e9}}));
        assert!(r.end_s > 10.6 && r.end_s < 11.4, "{}", r.end_s);
    }

    #[test]
    fn speed_spread_and_run_noise_change_the_fleet_but_not_the_seed_determinism() {
        let mut j = base();
        j["nodes"][0]["speed_cv"] = json!(0.1);
        j["nodes"][0]["run_cv"] = json!(0.05);
        let (a, b) = (go(j.clone()), go(j.clone()));
        assert_eq!(a.summary(), b.summary());
        assert!((a.ideal_s - go(base()).ideal_s).abs() > 1e-6);
    }

    #[test]
    fn manual_restart_without_standby_resumes_after_the_restart_time() {
        let mut j = base();
        j["head_failure"] = json!({"at_s": 3, "manual_restart_s": 100});
        let r = go(j);
        let ok = go(base());
        assert!(r.finished && r.done == r.n && r.end_s > ok.end_s + 90.0 && r.end_s < ok.end_s + 101.0, "{} {}", r.end_s, ok.end_s);
    }

    #[test]
    fn group_failing_at_a_time_is_requeued_and_costs_about_its_share() {
        let r = go(json!({"nodes": [{"count": 8, "rate": 1e5}, {"count": 2, "rate": 1e5, "fail_at_s": 40, "down_s": 1e9}], "job": {"n": 1e8}}));
        assert!(r.finished && r.done == r.n && r.failures == 2 && r.recovered > 0);
        assert!(r.end_s > r.ideal_s * 1.1, "{} {}", r.end_s, r.ideal_s); // 2 of 10 PCs lost at 40 % of the job
    }

    // ---- hierarchy mode ----
    fn hs(two: bool) -> HScn {
        HScn { pcs: 200, lab_pcs: 50, two_level: two, chunk_s: 1.0, n: 200.0 * 120.0, ..HScn::default() }
    }

    #[test]
    fn hier_both_levels_finish_and_count_every_unit_once() {
        for two in [false, true] {
            let o = hsim(&hs(two));
            assert!(o.finished && (o.done - 24000.0).abs() < 1e-3, "{two}: {} {}", o.done, o.end_s);
            assert!(o.end_s > o.ideal_s && o.end_s < 1.06 * o.ideal_s, "{two}: {} vs ideal {}", o.end_s, o.ideal_s);
            assert!(o.wasted == 0.0 && o.redone == 0.0 && o.lost_with_lab == 0.0);
        }
        let (a, b) = (hsim(&hs(true)).summary(), hsim(&hs(true)).summary());
        assert_eq!(a, b);
    }

    #[test]
    fn hier_one_level_matches_the_calibrated_head_ceiling() {
        // 500 PCs on 0.1 s chunks ask for 5,000 tasks/s; one head at 379 us + 0.199 us x 500 serves about 2,090
        let o = hsim(&HScn { pcs: 500, chunk_s: 0.1, max_s: 6.0, warm_s: 2.0, ..HScn::default() });
        assert!(o.tasks_s > 1900.0 && o.tasks_s < 2200.0 && o.root_cpu > 0.99, "{} {}", o.tasks_s, o.root_cpu);
    }

    #[test]
    fn hier_labs_lift_the_ceiling_and_the_root_sees_only_leases() {
        let sc = |two| HScn { pcs: 2000, lab_pcs: 100, two_level: two, chunk_s: 0.5, max_s: 6.0, warm_s: 2.0, ..HScn::default() };
        let (one, two) = (hsim(&sc(false)), hsim(&sc(true)));
        assert!(two.tasks_s > 2.5 * one.tasks_s && two.tasks_s > 3800.0, "{} {}", one.tasks_s, two.tasks_s);
        assert!(two.root_cpu < 0.05 && two.lab_cpu_max < 0.3 && two.root_msgs_s < 100.0, "{} {} {}", two.root_cpu, two.lab_cpu_max, two.root_msgs_s);
    }

    #[test]
    fn hier_lab_kill_regrants_the_lease_and_the_pcs_fall_back_to_the_root() {
        let ok = hsim(&hs(true));
        let o = hsim(&HScn { kill_lab: Some((1, 40.0)), ttl_s: 10.0, ..hs(true) }); // a short timeout: the 120 s job would otherwise wait for the fence at the tail
        assert!(o.finished && (o.done - 24000.0).abs() < 1e-3, "{}", o.done);
        assert!(o.lost_with_lab > 0.0 && o.redone >= o.lost_with_lab, "{} {}", o.lost_with_lab, o.redone);
        // the lab's PCs kept working through the root: the cost is about the lost lease, not a quarter of the fleet
        assert!(o.end_s > ok.end_s && o.end_s < ok.end_s + 10.0, "{} {}", o.end_s, ok.end_s);
    }

    #[test]
    fn hier_root_outage_is_hidden_by_leases_only_when_they_are_long_enough() {
        let kill = |two, l| HScn { root_kill: Some((40.0, 30.0)), lease_s: l, ..hs(two) };
        let d = |sc: HScn| { let ok = HScn { root_kill: None, ..sc.clone() }; hsim(&sc).end_s - hsim(&ok).end_s };
        let one = d(kill(false, 30.0));
        let (long, short) = (d(kill(true, 60.0)), d(kill(true, 5.0)));
        assert!(one > 25.0 && long < 3.0 && short > 15.0, "{one} {long} {short}");
    }

    #[test]
    fn hier_partitioned_lab_keeps_computing_its_lease_and_uploads_at_heal() {
        let p = |two| HScn { n: 1e12, chunk_s: 0.5, part: Some((0, 30.0, 40.0)), max_s: 100.0, lease_s: 60.0, ..hs(two) };
        let (one, two) = (hsim(&p(false)), hsim(&p(true)));
        let full = 40.0 * lab_cap(&p(true));
        assert!(one.part_units < 0.05 * full && two.part_units > 0.5 * full, "{} {} of {full}", one.part_units, two.part_units);
        assert!(two.wasted == 0.0 && two.redone == 0.0); // silent 40 s < ttl 90 s: the leases survive and are accepted
        // silent longer than the timeout: the root gives the range away and the late report is thrown away
        let late = hsim(&HScn { ttl_s: 10.0, ..p(true) });
        assert!(late.redone > 0.0 && late.wasted > 0.0, "{} {}", late.redone, late.wasted);
    }

    // ---- day mode ----
    fn day_of(j: Value) -> Value {
        day(&j)
    }
    fn lab(n: usize) -> Value {
        json!([{"count": n, "rate": 1e6}])
    }

    #[test]
    fn day_idle_lab_uses_idle_power_and_eco_sleeps_it() {
        let mut j = json!({"day": {"hours": 4, "tick_s": 10}, "pcs": lab(10)});
        let a = day_of(j.clone());
        assert!((a["kwh"].as_f64().unwrap() - 10.0 * 25.0 * 4.0 / 1e3).abs() < 1e-6);
        j["eco"] = json!({"enabled": true, "idle_sleep_s": 600});
        let e = day_of(j);
        assert!(e["kwh"].as_f64().unwrap() < 0.4 * a["kwh"].as_f64().unwrap(), "{} {}", e["kwh"], a["kwh"]);
    }

    #[test]
    fn day_job_finishes_on_time_and_max_pcs_limits_peak() {
        // 10 PCs x 1e6 x 0.97: 5.82e8 units takes ~60 s on all 10, ~120 s on 5
        let all = day_of(json!({"day": {"hours": 1, "tick_s": 1}, "pcs": lab(10), "jobs": [{"name": "a", "work": 5.82e8}]}));
        let r = all["jobs"][0]["run_s"].as_f64().unwrap();
        assert!(r > 59.0 && r < 65.0, "{r}");
        let half = day_of(json!({"day": {"hours": 1, "tick_s": 1}, "pcs": lab(10), "jobs": [{"name": "a", "work": 5.82e8, "max_pcs": 5}]}));
        assert_eq!(half["jobs"][0]["peak_pcs"], 5);
        assert!(half["jobs"][0]["run_s"].as_f64().unwrap() > 1.9 * r);
    }

    #[test]
    fn day_eco_wakes_only_the_pcs_a_job_may_use() {
        let j = |mx: u32| json!({"day": {"hours": 2, "tick_s": 5}, "pcs": lab(20), "eco": {"enabled": true, "idle_sleep_s": 60}, "power": {"wake_s": 30},
            "jobs": [{"name": "a", "arrive_h": 1, "work": 1e8, "max_pcs": mx}]});
        let (few, all) = (day_of(j(3)), day_of(j(0)));
        assert!(few["wakes"].as_u64().unwrap() <= 3 + 20 && few["kwh"].as_f64().unwrap() < all["kwh"].as_f64().unwrap());
        assert!(few["jobs"][0]["wait_s"].as_f64().unwrap() >= 29.0); // it waited for a boot
    }

    #[test]
    fn day_class_blocks_pcs_and_prewake_hides_the_boot() {
        let j = |pre: f64| json!({"day": {"hours": 3, "tick_s": 5}, "pcs": lab(10), "eco": {"enabled": true, "idle_sleep_s": 60, "prewake_s": pre}, "power": {"wake_s": 40},
            "reservations": [{"start_h": 2, "end_h": 2.5, "first_pc": 0, "pcs": 4}]});
        assert!(day_of(j(10.0))["class_delay_pc_s"].as_f64().unwrap() > 0.0);
        assert_eq!(day_of(j(120.0))["class_delay_pc_s"].as_f64().unwrap(), 0.0);
    }
}
