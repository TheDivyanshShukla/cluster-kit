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
use crate::sched::{self, ChunkPolicy};
use crate::{die, Opts};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

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
struct Ev {
    t: f64,
    seq: u64,
    k: K,
}
impl PartialEq for Ev {
    fn eq(&self, o: &Self) -> bool {
        self.seq == o.seq
    }
}
impl Eq for Ev {}
impl PartialOrd for Ev {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Ev {
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
    pub pending: u64,
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

pub fn run(o: &Opts) {
    let path = o.rest.first().unwrap_or_else(|| die("usage: ck sim SCENARIO.json [--out DIR]"));
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let dir = std::env::var("CK_SIM_OUT").unwrap_or_else(|_| "sim-out".into());
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(&format!("{dir}: {e}")));
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
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
    let (idle_w, busy_w, sleep_w, boot_w, int_w, wake_s) = (f(p, "idle_w", 25.0), f(p, "busy_w", 65.0), f(p, "sleep_w", 2.0), f(p, "boot_w", 65.0), f(p, "interactive_w", 45.0), f(p, "wake_s", 45.0));
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
            let cands: Vec<sched::PlaceNode> = (0..n).filter(|&i| pcs[i].st == 2 && t >= pcs[i].down_until && !blocked[i] && !taken[i]).map(|i| sched::PlaceNode { node_id: ids[i].clone(), rate: pcs[i].rate, threads: pcs[i].threads, holds: jobs[j].holders[i] }).collect();
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
                    let all: Vec<sched::PlaceNode> = (0..n).filter(|&i| !blocked[i] && !taken[i] && pcs[i].st == 0).map(|i| sched::PlaceNode { node_id: ids[i].clone(), rate: pcs[i].rate, threads: pcs[i].threads, holds: false }).collect();
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
