//! Agent B: task leases, worker capabilities, retries, affinity. Pure and in-memory.
//!
//! Time is passed in, never read. `reap(now_us)` and `advance(now_us)` move the clock; every other call uses the
//! last time it was given, so the head calls `reap(now)` at the top of each lease/heartbeat/index handler.
//!
//! Generations: a task starts at generation 1 / attempt 1. Every reassignment (lease expiry, retryable failure)
//! bumps both. A batch carrying an older generation is refused with `stale_lease`. A live camera that fails over
//! to another worker gets a new generation, and its tracker restarts there: track IDs do NOT continue across the
//! failover (the new worker's tracker numbers are fresh), which is the explicit identity reset the spec asks for.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::watch::domain::{self, ApiError, IndexJob, JobState, LeaseRef, SourceKind, TaskLease, Us};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerCaps {
    pub id: String,
    pub host: String,
    pub cores: u32,
    /// e.g. "ffmpeg". A task whose `settings.decoder` names one needs it here.
    #[serde(default)]
    pub decoders: Vec<String>,
    /// Model version IDs installed and loadable on this worker. A job needs all of its `model_versions`.
    #[serde(default)]
    pub models: Vec<String>,
    /// Webcam device names attached to this worker.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Recent inferred frames per second. 0 = not measured yet (cores are used as the weight).
    #[serde(default)]
    pub measured_fps: f64,
}

/// One schedulable unit: a whole file, a segment of a file, an image, or a live camera.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task_id: String,
    pub source_id: String,
    pub media_id: String,
    pub kind: SourceKind,
    /// [start_us, end_us) of media time. `[None, None]` = whole media / live.
    #[serde(default)]
    pub segment: [Option<Us>; 2],
    #[serde(default)]
    pub overlap_us: Us,
    /// Webcam device name; only workers listing it in `devices` may run the task. Required for `Webcam`.
    #[serde(default)]
    pub device: Option<String>,
    /// Empty = any worker.
    #[serde(default)]
    pub eligible_workers: Vec<String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

impl TaskSpec {
    fn live(&self) -> bool {
        matches!(self.kind, SourceKind::Live | SourceKind::Webcam)
    }
}

/// Split a video into `seg_us` segments. Every segment after the first reads `overlap_us` earlier for tracker
/// warm-up; `ingest::owns` keeps those frames from being indexed twice. The last segment's end is `None`
/// (to end of media) so a rounded duration cannot strand the final frames.
pub fn plan_segments(job_id: &str, source_id: &str, media_id: &str, duration_us: Us, seg_us: Us, overlap_us: Us) -> Vec<TaskSpec> {
    let seg_us = seg_us.max(1);
    let n = ((duration_us + seg_us - 1) / seg_us).max(1);
    (0..n)
        .map(|i| {
            let start = i * seg_us;
            let last = i == n - 1;
            TaskSpec {
                task_id: domain::stable_id("task", &[job_id, media_id, &start.to_string()]),
                source_id: source_id.into(),
                media_id: media_id.into(),
                kind: SourceKind::Archive,
                segment: [Some(start), if last { None } else { Some(start + seg_us) }],
                overlap_us: if i == 0 { 0 } else { overlap_us },
                device: None,
                eligible_workers: vec![],
                settings: serde_json::json!({}),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
enum St {
    Queued,
    Leased { worker: String, expires: Us },
    Done,
    Failed(ApiError),
}

struct Task {
    spec: TaskSpec,
    job_id: String,
    generation: u64,
    attempt: u32,
    st: St,
    queued_at: Us,
}

struct W {
    caps: WorkerCaps,
    seen: Us,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reaped {
    pub task_id: String,
    pub worker: String,
    pub generation: u64,
    /// `Some` when the retry budget was exhausted and the task is now Failed.
    pub failed: Option<ApiError>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub job_id: String,
    pub media_id: String,
    pub state: &'static str,
    pub worker: Option<String>,
    pub generation: u64,
    pub attempt: u32,
    pub error: Option<ApiError>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkerView {
    pub caps: WorkerCaps,
    pub alive: bool,
    pub leases: usize,
}

pub struct Scheduler {
    lease_us: Us,
    max_attempts: u32,
    now: Us,
    workers: BTreeMap<String, W>,
    tasks: BTreeMap<String, Task>,
    jobs: BTreeMap<String, IndexJob>,
}

fn stale(msg: impl Into<String>) -> ApiError {
    ApiError::new("stale_lease", msg)
}

impl Scheduler {
    /// `lease_us`: how long a lease and a worker's liveness last without a heartbeat. Retry budget is 3 attempts.
    pub fn new(lease_us: Us) -> Self {
        Scheduler { lease_us: lease_us.max(1), max_attempts: 3, now: 0, workers: BTreeMap::new(), tasks: BTreeMap::new(), jobs: BTreeMap::new() }
    }

    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    pub fn advance(&mut self, now_us: Us) {
        self.now = self.now.max(now_us);
    }

    pub fn register(&mut self, caps: WorkerCaps) {
        let seen = self.now;
        self.workers.insert(caps.id.clone(), W { caps, seen });
    }

    fn alive(&self, w: &W) -> bool {
        self.now - w.seen <= self.lease_us
    }

    fn held(&self, worker: &str) -> usize {
        self.tasks.values().filter(|t| matches!(&t.st, St::Leased { worker: w, .. } if w == worker)).count()
    }

    /// Refresh liveness and extend every lease this worker still holds. Returns the leases the head still
    /// recognises: a worker whose local lease is missing from the reply has lost it and must stop that task.
    /// Unknown worker = `not_found` (it should `register` again).
    pub fn heartbeat(&mut self, worker: &str) -> Result<Vec<LeaseRef>, ApiError> {
        let now = self.now;
        let ttl = self.lease_us;
        let w = self.workers.get_mut(worker).ok_or_else(|| ApiError::new("not_found", format!("unknown worker {worker}; register first")))?;
        w.seen = now;
        let mut out = Vec::new();
        for (id, t) in self.tasks.iter_mut() {
            if let St::Leased { worker: lw, expires } = &mut t.st {
                if lw == worker && *expires > now {
                    *expires = now + ttl;
                    out.push(LeaseRef { task_id: id.clone(), generation: t.generation });
                }
            }
        }
        Ok(out)
    }

    /// Add a job and its tasks. A webcam task must name its device; duplicate task IDs are a conflict.
    pub fn submit(&mut self, mut job: IndexJob, specs: Vec<TaskSpec>) -> Result<(), ApiError> {
        for s in &specs {
            if self.tasks.contains_key(&s.task_id) {
                return Err(ApiError::new("conflict", format!("task {} already exists", s.task_id)));
            }
            if s.kind == SourceKind::Webcam && s.device.is_none() {
                return Err(ApiError::new("invalid_request", format!("webcam task {} must name its device", s.task_id)));
            }
        }
        job.state = JobState::Queued;
        let now = self.now;
        for s in specs {
            self.tasks.insert(s.task_id.clone(), Task { spec: s, job_id: job.id.clone(), generation: 1, attempt: 1, st: St::Queued, queued_at: now });
        }
        self.jobs.insert(job.id.clone(), job);
        Ok(())
    }

    fn eligible(&self, w: &W, t: &Task) -> bool {
        let s = &t.spec;
        if !self.alive(w) || self.held(&w.caps.id) >= w.caps.cores.max(1) as usize {
            return false;
        }
        if !s.eligible_workers.is_empty() && !s.eligible_workers.contains(&w.caps.id) {
            return false;
        }
        if s.device.as_ref().is_some_and(|d| !w.caps.devices.contains(d)) {
            return false;
        }
        if s.settings.get("decoder").and_then(|d| d.as_str()).is_some_and(|d| !w.caps.decoders.iter().any(|x| x == d)) {
            return false;
        }
        self.jobs.get(&t.job_id).is_none_or(|j| j.model_versions.iter().all(|m| w.caps.models.contains(m)))
    }

    /// Held leases per unit of throughput (measured fps, else cores). Lower = more headroom.
    fn load(&self, w: &W) -> f64 {
        let weight = if w.caps.measured_fps > 0.0 { w.caps.measured_fps } else { w.caps.cores.max(1) as f64 };
        self.held(&w.caps.id) as f64 / weight
    }

    /// Hand `worker` the best queued task it may run, or `None`. Pinned tasks (webcam / named workers) go first,
    /// then oldest. A worker is skipped for a task while a less-loaded eligible worker exists (load = leases per
    /// measured fps), unless the task has waited over half a lease, so a slow poller never starves the queue.
    pub fn lease(&mut self, worker: &str) -> Option<TaskLease> {
        let now = self.now;
        let me = self.workers.get(worker)?;
        let my_load = self.load(me);
        let mut order: Vec<(&String, &Task)> = self.tasks.iter().filter(|(_, t)| t.st == St::Queued).collect();
        order.sort_by_key(|(id, t)| (t.spec.device.is_none() && t.spec.eligible_workers.is_empty(), t.queued_at, (*id).clone()));
        let pick = order.into_iter().find(|(_, t)| {
            if !self.eligible(me, t) {
                return false;
            }
            let better = self.workers.values().any(|o| o.caps.id != worker && self.eligible(o, t) && self.load(o) + 1e-9 < my_load);
            !better || now - t.queued_at > self.lease_us / 2
        });
        let id = pick?.0.clone();
        let ttl = self.lease_us;
        let t = self.tasks.get_mut(&id)?;
        t.st = St::Leased { worker: worker.to_string(), expires: now + ttl };
        let mut settings = t.spec.settings.clone();
        if let Some(o) = settings.as_object_mut() {
            o.insert("source_id".into(), t.spec.source_id.clone().into());
        } else {
            settings = serde_json::json!({"source_id": t.spec.source_id});
        }
        let lease = TaskLease {
            task_id: id,
            job_id: t.job_id.clone(),
            media_id: t.spec.media_id.clone(),
            segment: t.spec.segment,
            worker: worker.to_string(),
            generation: t.generation,
            expires_utc_us: now + ttl,
            attempt: t.attempt,
            overlap_us: t.spec.overlap_us,
            settings,
        };
        self.set_job_running(&lease.job_id);
        Some(lease)
    }

    fn set_job_running(&mut self, job: &str) {
        if let Some(j) = self.jobs.get_mut(job) {
            j.state = JobState::Running;
        }
    }

    /// Is this lease still the live one? Wrong generation, expired lease or failed task = `stale_lease`.
    /// A finished task with the right generation passes (a retried final batch is harmless: storage is idempotent).
    pub fn check(&self, r: &LeaseRef) -> Result<(), ApiError> {
        let t = self.tasks.get(&r.task_id).ok_or_else(|| ApiError::new("not_found", format!("unknown task {}", r.task_id)))?;
        if t.generation != r.generation {
            return Err(stale(format!("task {} is at generation {}, batch carried {}", r.task_id, t.generation, r.generation)));
        }
        match &t.st {
            St::Leased { expires, .. } if *expires <= self.now => Err(stale(format!("lease on {} expired", r.task_id))),
            St::Leased { .. } | St::Done => Ok(()),
            St::Queued => Err(stale(format!("task {} was requeued", r.task_id))),
            St::Failed(e) => Err(stale(format!("task {} failed: {}", r.task_id, e.message))),
        }
    }

    pub fn complete(&mut self, task_id: &str, generation: u64) -> Result<(), ApiError> {
        self.check(&LeaseRef { task_id: task_id.into(), generation })?;
        self.tasks.get_mut(task_id).unwrap().st = St::Done;
        Ok(())
    }

    /// Worker reports failure. `permanent` (bad media, missing model) is never retried; otherwise the task is
    /// requeued with generation+1 and attempt+1 until the budget runs out.
    pub fn fail(&mut self, task_id: &str, generation: u64, permanent: bool) -> Result<(), ApiError> {
        let msg = if permanent { "worker reported a permanent failure" } else { "worker reported a failure" };
        self.fail_with(task_id, generation, permanent, ApiError::new(if permanent { "permanent_failure" } else { "task_failed" }, msg))
    }

    pub fn fail_with(&mut self, task_id: &str, generation: u64, permanent: bool, err: ApiError) -> Result<(), ApiError> {
        self.check(&LeaseRef { task_id: task_id.into(), generation })?;
        if permanent {
            self.tasks.get_mut(task_id).unwrap().st = St::Failed(err);
        } else {
            self.requeue(task_id, true);
        }
        Ok(())
    }

    /// Requeue with a bumped generation, or fail when the budget is spent. Returns the failure if it failed.
    fn requeue(&mut self, task_id: &str, budgeted: bool) -> Option<ApiError> {
        let (now, max) = (self.now, self.max_attempts);
        let t = self.tasks.get_mut(task_id)?;
        if budgeted && t.attempt >= max {
            let e = ApiError::new("retry_budget_exhausted", format!("task failed {} times", t.attempt));
            t.st = St::Failed(e.clone());
            return Some(e);
        }
        t.generation += 1;
        t.attempt += 1;
        t.st = St::Queued;
        t.queued_at = now;
        None
    }

    /// Requeue every task whose lease ran out. Live/webcam tasks are exempt from the retry budget on expiry
    /// (losing a worker must not permanently kill a camera); everything else spends an attempt.
    pub fn reap(&mut self, now_us: Us) -> Vec<Reaped> {
        self.advance(now_us);
        let expired: Vec<(String, String)> = self
            .tasks
            .iter()
            .filter_map(|(id, t)| match &t.st {
                St::Leased { worker, expires } if *expires <= self.now => Some((id.clone(), worker.clone())),
                _ => None,
            })
            .collect();
        expired
            .into_iter()
            .map(|(id, worker)| {
                let live = self.tasks[&id].spec.live();
                let failed = self.requeue(&id, !live);
                Reaped { generation: self.tasks[&id].generation, task_id: id, worker, failed }
            })
            .collect()
    }

    /// Move finite work to a worker that has none. A late-joining worker would otherwise idle until
    /// the first worker finishes. Only archive/image tasks move (live cameras keep affinity) and only
    /// from a worker holding two or more of them. The moved task is requeued at generation+1 without
    /// spending an attempt; its old holder loses the lease at its next heartbeat and the head resumes
    /// the new lease from the committed checkpoint. Returns the task IDs moved.
    pub fn rebalance(&mut self) -> Vec<String> {
        let mut moved = Vec::new();
        loop {
            let idle: Vec<String> = self.workers.values().filter(|w| self.alive(w) && self.held(&w.caps.id) == 0).map(|w| w.caps.id.clone()).collect();
            if idle.is_empty() {
                break;
            }
            // Busiest holder of movable work first; ties by worker id, newest lease moved first.
            let mut by: BTreeMap<String, Vec<(Us, String)>> = BTreeMap::new();
            for (id, t) in &self.tasks {
                if let St::Leased { worker, expires } = &t.st {
                    if !t.spec.live() && t.spec.device.is_none() && t.spec.eligible_workers.is_empty() {
                        by.entry(worker.clone()).or_default().push((*expires, id.clone()));
                    }
                }
            }
            let pick = by
                .into_iter()
                .filter(|(_, v)| v.len() >= 2)
                .max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(&a.0)))
                .and_then(|(_, mut v)| {
                    v.sort();
                    v.pop().map(|(_, id)| id)
                });
            let Some(id) = pick else { break };
            let eligible_idle = idle.iter().any(|w| self.workers.get(w).is_some_and(|w| self.eligible(w, &self.tasks[&id])));
            if !eligible_idle {
                break;
            }
            let now = self.now;
            let t = self.tasks.get_mut(&id).unwrap();
            t.generation += 1;
            t.st = St::Queued;
            t.queued_at = now - self.lease_us; // first in line for the idle worker
            moved.push(id);
            if moved.len() >= idle.len() {
                break;
            }
        }
        moved
    }

    pub fn tasks(&self) -> Vec<TaskView> {
        self.tasks
            .iter()
            .map(|(id, t)| {
                let (state, worker, error) = match &t.st {
                    St::Queued => ("queued", None, None),
                    St::Leased { worker, .. } => ("leased", Some(worker.clone()), None),
                    St::Done => ("done", None, None),
                    St::Failed(e) => ("failed", None, Some(e.clone())),
                };
                TaskView { task_id: id.clone(), job_id: t.job_id.clone(), media_id: t.spec.media_id.clone(), state, worker, generation: t.generation, attempt: t.attempt, error }
            })
            .collect()
    }

    pub fn workers(&self) -> Vec<WorkerView> {
        self.workers.values().map(|w| WorkerView { caps: w.caps.clone(), alive: self.alive(w), leases: self.held(&w.caps.id) }).collect()
    }

    /// Job state derived from its tasks: Completed only when every task is Done; Failed when the rest are settled
    /// and at least one Failed.
    pub fn job_state(&self, job_id: &str) -> Option<JobState> {
        self.jobs.get(job_id)?;
        let ts: Vec<&Task> = self.tasks.values().filter(|t| t.job_id == job_id).collect();
        let n = |f: &dyn Fn(&St) -> bool| ts.iter().filter(|t| f(&t.st)).count();
        let (queued, leased, done, failed) = (
            n(&|s| *s == St::Queued),
            n(&|s| matches!(s, St::Leased { .. })),
            n(&|s| *s == St::Done),
            n(&|s| matches!(s, St::Failed(_))),
        );
        Some(if leased == 0 && queued == 0 {
            if failed > 0 { JobState::Failed } else { JobState::Completed }
        } else if leased > 0 || done + failed > 0 {
            JobState::Running
        } else {
            JobState::Queued
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::ingest;

    const S: Us = 1_000_000;

    fn caps(id: &str, cores: u32) -> WorkerCaps {
        WorkerCaps { id: id.into(), host: id.into(), cores, decoders: vec!["ffmpeg".into()], models: vec!["m1".into()], devices: vec![], measured_fps: 0.0 }
    }

    fn job(id: &str) -> IndexJob {
        IndexJob {
            id: id.into(), manifest_version: "v1".into(), media_ids: vec![], model_versions: vec!["m1".into()], sampling: serde_json::json!({}),
            state: JobState::Queued, checkpoint: Default::default(), coverage: Default::default(), error: None, created_utc_us: 0, finished_utc_us: None,
        }
    }

    fn spec(id: &str, kind: SourceKind) -> TaskSpec {
        TaskSpec { task_id: id.into(), source_id: "src".into(), media_id: format!("med_{id}"), kind, segment: [None, None], overlap_us: 0, device: None, eligible_workers: vec![], settings: serde_json::json!({}) }
    }

    fn sched() -> Scheduler {
        let mut s = Scheduler::new(10 * S);
        s.register(caps("w1", 4));
        s.register(caps("w2", 4));
        s
    }

    #[test]
    fn late_worker_takes_movable_work() {
        let mut s = Scheduler::new(10 * S);
        s.register(caps("w1", 8));
        let specs: Vec<TaskSpec> = (0..3).map(|i| TaskSpec { task_id: format!("t{i}"), source_id: "s".into(), media_id: format!("m{i}"), kind: SourceKind::Archive, segment: [None, None], overlap_us: 0, device: None, eligible_workers: vec![], settings: serde_json::json!({}) }).collect();
        s.submit(job("j"), specs).unwrap();
        while s.lease("w1").is_some() {}
        s.register(caps("w2", 8));
        let moved = s.rebalance();
        assert_eq!(moved.len(), 1, "one task moves to the single idle worker");
        let l = s.lease("w2").expect("w2 gets the moved task");
        assert_eq!(l.generation, 2);
        assert_eq!(l.attempt, 1, "rebalancing does not spend an attempt");
        // The old holder's lease is gone from its heartbeat.
        assert!(!s.heartbeat("w1").unwrap().iter().any(|r| r.task_id == l.task_id));
        assert!(s.rebalance().is_empty(), "nothing moves once every worker has work");
    }

    #[test]
    fn live_tasks_keep_affinity_when_a_worker_joins() {
        let mut s = Scheduler::new(10 * S);
        s.register(caps("w1", 8));
        let specs: Vec<TaskSpec> = (0..2).map(|i| TaskSpec { task_id: format!("cam{i}"), source_id: "s".into(), media_id: format!("m{i}"), kind: SourceKind::Live, segment: [None, None], overlap_us: 0, device: None, eligible_workers: vec![], settings: serde_json::json!({}) }).collect();
        s.submit(job("j"), specs).unwrap();
        while s.lease("w1").is_some() {}
        s.register(caps("w2", 8));
        assert!(s.rebalance().is_empty());
    }

    #[test]
    fn two_workers_split_the_queue() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("a", SourceKind::Archive), spec("b", SourceKind::Archive)]).unwrap();
        let a = s.lease("w1").unwrap();
        assert!(s.lease("w1").is_none(), "w1 is busier than idle w2 and the task is young");
        let b = s.lease("w2").unwrap();
        assert_ne!(a.task_id, b.task_id);
        assert_eq!((a.generation, a.attempt, b.generation), (1, 1, 1));
        assert!(s.lease("w2").is_none());
        s.complete(&a.task_id, a.generation).unwrap();
        s.complete(&b.task_id, b.generation).unwrap();
        assert_eq!(s.job_state("j"), Some(JobState::Completed));
    }

    #[test]
    fn faster_worker_gets_proportionally_more() {
        let mut s = Scheduler::new(10 * S);
        let mut fast = caps("fast", 8);
        fast.measured_fps = 30.0;
        let mut slow = caps("slow", 8);
        slow.measured_fps = 10.0;
        s.register(slow);
        s.register(fast);
        let specs: Vec<_> = (0..4).map(|i| spec(&format!("t{i}"), SourceKind::Archive)).collect();
        s.submit(job("j"), specs).unwrap();
        let mut got = std::collections::BTreeMap::<String, u32>::new();
        for _ in 0..4 {
            for w in ["slow", "fast"] {
                if let Some(l) = s.lease(w) {
                    *got.entry(l.worker).or_default() += 1;
                }
            }
        }
        assert_eq!(got["fast"], 3);
        assert_eq!(got["slow"], 1);
    }

    #[test]
    fn worker_loss_reaps_and_reassigns_with_new_generation() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("a", SourceKind::Archive)]).unwrap();
        let first = s.lease("w1").unwrap();
        // w1 dies silently. w2 keeps heartbeating.
        s.advance(6 * S);
        s.heartbeat("w2").unwrap();
        assert!(s.reap(6 * S).is_empty(), "lease still valid");
        s.advance(11 * S);
        s.heartbeat("w2").unwrap();
        let reaped = s.reap(11 * S);
        assert_eq!(reaped.len(), 1);
        assert_eq!((reaped[0].worker.as_str(), reaped[0].generation, reaped[0].failed.clone()), ("w1", 2, None));
        let second = s.lease("w2").unwrap();
        assert_eq!((second.task_id.as_str(), second.generation, second.attempt), (first.task_id.as_str(), 2, 2));
        // The dead worker comes back and posts a late batch / complete: refused.
        let late = LeaseRef { task_id: first.task_id.clone(), generation: first.generation };
        assert_eq!(s.check(&late).unwrap_err().code, "stale_lease");
        assert_eq!(s.complete(&first.task_id, 1).unwrap_err().code, "stale_lease");
        s.check(&LeaseRef { task_id: second.task_id, generation: 2 }).unwrap();
        assert!(s.heartbeat("w1").unwrap().is_empty(), "revived w1 learns it holds nothing");
        // And a dead worker is not handed new work until it heartbeats again.
        s.advance(40 * S);
        assert!(s.lease("w1").is_none());
    }

    #[test]
    fn expired_but_unreaped_lease_is_stale() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("a", SourceKind::Archive)]).unwrap();
        let l = s.lease("w1").unwrap();
        s.advance(11 * S);
        assert_eq!(s.check(&LeaseRef { task_id: l.task_id, generation: 1 }).unwrap_err().code, "stale_lease");
    }

    #[test]
    fn permanent_failure_is_never_retried() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("a", SourceKind::Archive)]).unwrap();
        let l = s.lease("w1").unwrap();
        s.fail("a", l.generation, true).unwrap();
        assert!(s.lease("w1").is_none() && s.lease("w2").is_none());
        s.advance(100 * S);
        assert!(s.reap(100 * S).is_empty());
        assert_eq!(s.tasks()[0].state, "failed");
        assert_eq!(s.job_state("j"), Some(JobState::Failed));
    }

    #[test]
    fn retry_budget_then_failed() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("a", SourceKind::Archive)]).unwrap();
        for attempt in 1..=3u32 {
            let w = if attempt % 2 == 1 { "w1" } else { "w2" };
            let l = s.lease(w).unwrap_or_else(|| s.lease(if w == "w1" { "w2" } else { "w1" }).unwrap());
            assert_eq!((l.attempt, l.generation), (attempt, attempt as u64));
            s.fail("a", l.generation, false).unwrap();
        }
        assert_eq!(s.tasks()[0].state, "failed");
        assert_eq!(s.tasks()[0].error.as_ref().unwrap().code, "retry_budget_exhausted");
        assert!(s.lease("w1").is_none());
    }

    #[test]
    fn webcam_is_pinned_to_the_worker_with_the_device() {
        let mut s = sched();
        let mut cam = spec("cam", SourceKind::Webcam);
        assert_eq!(s.submit(job("j0"), vec![cam.clone()]).unwrap_err().code, "invalid_request");
        cam.device = Some("FaceTime HD".into());
        s.submit(job("j"), vec![cam]).unwrap();
        assert!(s.lease("w1").is_none() && s.lease("w2").is_none(), "nobody has the device yet");
        let mut c2 = caps("w2", 4);
        c2.devices = vec!["FaceTime HD".into()];
        s.register(c2);
        assert!(s.lease("w1").is_none());
        let l = s.lease("w2").unwrap();
        // Worker loss: webcam goes back to Queued with a new generation, still only w2 may run it, and it is
        // exempt from the retry budget so a flaky laptop cannot kill the camera permanently.
        for round in 0..5u64 {
            let t = (20 + round * 20) as Us * S;
            s.reap(t);
            assert_eq!(s.tasks()[0].state, "queued");
            s.register(caps("w1", 4));
            assert!(s.lease("w1").is_none());
            let mut c = caps("w2", 4);
            c.devices = vec!["FaceTime HD".into()];
            s.register(c);
            let again = s.lease("w2").unwrap();
            assert_eq!(again.generation, l.generation + round + 1);
            s.advance(t + 11 * S);
        }
    }

    #[test]
    fn live_camera_keeps_its_worker_while_alive() {
        let mut s = sched();
        s.submit(job("j"), vec![spec("cam", SourceKind::Live)]).unwrap();
        let l = s.lease("w1").unwrap();
        for k in 1..=5u64 {
            s.advance(k as Us * 8 * S);
            s.heartbeat("w1").unwrap();
            s.heartbeat("w2").unwrap();
            assert!(s.reap(k as Us * 8 * S).is_empty());
        }
        let t = s.tasks();
        assert_eq!((t[0].worker.as_deref(), t[0].generation), (Some("w1"), l.generation));
    }

    #[test]
    fn model_and_decoder_requirements_filter_workers() {
        let mut s = sched();
        let mut c = caps("w3", 4);
        c.models = vec![];
        s.register(c);
        s.submit(job("j"), vec![spec("a", SourceKind::Archive)]).unwrap();
        assert!(s.lease("w3").is_none(), "w3 lacks model m1");
    }

    #[test]
    fn overlapped_segments_index_every_frame_exactly_once() {
        let segs = plan_segments("j", "src", "med", 30 * S, 10 * S, 2 * S);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].segment, [Some(0), Some(10 * S)]);
        assert_eq!(segs[2].segment, [Some(20 * S), None]);
        assert_eq!((segs[0].overlap_us, segs[1].overlap_us), (0, 2 * S));
        let mut s = Scheduler::new(10 * S);
        let mut c = caps("w", 8);
        c.measured_fps = 1.0;
        s.register(c);
        s.submit(job("j"), segs).unwrap();
        let leases: Vec<TaskLease> = (0..3).map(|_| s.lease("w").unwrap()).collect();
        // Each task decodes from (start - overlap); every frame time is owned by exactly one lease.
        let mut owners = 0;
        for t in (0..30 * S).step_by(100_000) {
            let n = leases.iter().filter(|l| ingest::owns(l, t)).count();
            assert_eq!(n, 1, "frame at {t} owned by {n} leases");
            owners += n;
        }
        assert_eq!(owners, 300);
        // The overlap region a segment re-reads is owned by the previous one.
        let l1 = leases.iter().find(|l| l.segment[0] == Some(10 * S)).unwrap();
        assert!(!ingest::owns(l1, 10 * S - l1.overlap_us));
    }
}
