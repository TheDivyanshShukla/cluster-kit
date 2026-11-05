//! `ck sim SCENARIO.json [--out DIR]`: deterministic discrete-event simulation of a cluster (SIM-02, SIM-03).
//! It calls sched::chunk_len, so the simulated head chunks work exactly like the real one (decision D5).
//! Writes DIR/timeline.csv (t_s, done, rate_per_s, busy_nodes, connected_nodes, head_busy_frac) and DIR/summary.json.
//!
//! Scenario (every field optional, defaults in brackets):
//! {
//!   "nodes": [{"count": 1, "rate": 1e6, "jitter": 0.0, "mtbf_s": 0, "down_s": 60, "lab": "A"}],
//!                 rate = numbers/s per node; jitter = per-chunk uniform spread (+-fraction of the time);
//!                 mtbf_s = mean time between failures (exponential), 0 = never; down_s = time away after a failure
//!   "job": {"n": 1e9, "policy": "adaptive", "chunk": 0},          policy adaptive|guided|fixed; chunk only for fixed
//!   "network": {"latency_ms": 0.3},                                one way
//!   "head": {"service_us": 470, "inflight": 2},                    head time per task message; chunks in flight per node
//!   "architecture": "single",                                      single | standby | relays | relays+standby
//!   "relays": {"per_lab": true, "batch": 20, "extra_latency_ms": 0.3},   batch = tasks per head message
//!   "head_failure": {"at_s": 0, "recovery_s": 6},                  at_s 0 = none; without a standby the job stops there
//!   "seed": 1, "sample_s": 1, "max_s": 86400                       max_s = stop an unfinished run
//! }
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
}

#[derive(Clone)]
struct Group {
    count: usize,
    rate: f64,
    jitter: f64,
    mtbf: f64,
    down: f64,
    lab: usize,
}

pub struct Scn {
    groups: Vec<Group>,
    n: u64,
    policy: ChunkPolicy,
    chunk: u64,
    lat: f64,
    svc: f64,
    inflight: u32,
    relays: bool,
    per_lab: bool,
    standby: bool,
    batch: f64,
    extra: f64,
    head_fail: Option<(f64, f64)>,
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
            groups.push(Group { count: f(&g, "count", 1.0) as usize, rate: f(&g, "rate", 1e6), jitter: f(&g, "jitter", 0.0), mtbf: f(&g, "mtbf_s", 0.0), down: f(&g, "down_s", 60.0), lab });
        }
        let pol = v["job"]["policy"].as_str().unwrap_or("adaptive");
        let policy = sched::parse_policy(pol).ok_or(format!("bad policy {pol}"))?;
        let arch = v["architecture"].as_str().unwrap_or("single");
        if !["single", "standby", "relays", "relays+standby"].contains(&arch) {
            return Err(format!("bad architecture {arch}"));
        }
        let (r, hf) = (&v["relays"], &v["head_failure"]);
        let at = f(hf, "at_s", 0.0);
        Ok(Scn {
            groups,
            n: f(&v["job"], "n", 1e9) as u64,
            policy,
            chunk: f(&v["job"], "chunk", 0.0) as u64,
            lat: f(&v["network"], "latency_ms", 0.3) / 1e3,
            svc: f(&v["head"], "service_us", 470.0) / 1e6,
            inflight: f(&v["head"], "inflight", 2.0).max(1.0) as u32,
            relays: arch.starts_with("relays"),
            per_lab: r["per_lab"].as_bool().unwrap_or(true),
            standby: arch.ends_with("standby"),
            batch: f(r, "batch", 20.0).max(1.0),
            extra: f(r, "extra_latency_ms", 0.3) / 1e3,
            head_fail: (at > 0.0).then(|| (at, f(hf, "recovery_s", 6.0))),
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
            "recovered_work": self.recovered, "duplicate_work": 0})
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
        if let Some((at, rec)) = s.head_fail.filter(|_| s.standby) {
            if st >= at && st < at + rec {
                st = at + rec; // standby is promoting; the queue waits
            }
        }
        let cost = if s.relays { s.svc / s.batch } else { s.svc };
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
        let end = arrive.max(nd.busy_until) + len as f64 / nd.g.rate * jit;
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
    for g in &s.groups {
        for _ in 0..g.count {
            nodes.push(Nd { g: g.clone(), up: true, gen: 0, used: 0, in_idle: false, busy_until: 0.0, chunks: vec![] });
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
    }
    // no standby: the job stops when the head dies
    let stop = match s.head_fail {
        Some((at, _)) if !s.standby => at.min(s.max_s),
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
    let s = Scn::parse(&text).unwrap_or_else(|e| die(&e));
    let t0 = std::time::Instant::now();
    let r = simulate(&s);
    let dir = std::env::var("CK_SIM_OUT").unwrap_or_else(|_| "sim-out".into());
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(&format!("{dir}: {e}")));
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
}
