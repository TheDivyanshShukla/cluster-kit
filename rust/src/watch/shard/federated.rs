//! `FederatedStore`: `domain::Store` on the head with observation reads fanned out to the nodes.
//!
//! Catalog calls (sources, media, jobs, query runs, reviews, alerts, audit) go to the head's own store. Reads of
//! observations, detections, summaries and relations go to every node that can hold the requested recordings, in
//! parallel, each under a deadline. The answers are merged with a k-way merge on (media, time, id), so the existing
//! query engine sees one ordered stream. Which computers answered is recorded for the run (`shard::take_run`).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::merge::{key_cursor, kmerge, Cursor};
use super::*;

pub struct FederatedStore {
    cat: Arc<dyn Store>,
    reg: Arc<Registry>,
    #[allow(dead_code)]
    data: PathBuf,
    deadline: Duration,
}

type Outcome<R> = (String, StoreResult<R>, u64);

impl FederatedStore {
    pub fn new(cat: Arc<dyn Store>, reg: Arc<Registry>, data: PathBuf) -> Self {
        FederatedStore { cat, reg, data, deadline: deadline() }
    }

    pub fn with_deadline(mut self, d: Duration) -> Self {
        self.deadline = d;
        self
    }

    /// Nodes that can hold rows for this filter. A node whose media set is unknown (just started) is always asked.
    fn plan(&self, f: &DetectionFilter) -> Vec<String> {
        let mut media: std::collections::BTreeSet<String> = f.media_ids.iter().cloned().collect();
        for s in &f.source_ids {
            if let Ok(v) = self.cat.media_for_source(s) {
                media.extend(v.into_iter().map(|m| m.id));
            }
        }
        self.reg
            .ids()
            .into_iter()
            .filter(|id| {
                if media.is_empty() || id == "head" {
                    return true;
                }
                self.reg.get(id).map_or(true, |n| n.media.is_empty() || n.media.iter().any(|m| media.contains(m)))
            })
            .collect()
    }

    /// Run `f` on each node in its own thread and wait until the deadline. Late answers are dropped. A node the
    /// registry calls down (or that already failed in this run) is not called at all.
    fn scatter<R, F>(&self, ids: &[String], record: bool, f: F) -> Vec<Outcome<R>>
    where
        R: Send + 'static,
        F: Fn(&str, Arc<dyn NodeClient>, Duration) -> StoreResult<R> + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        let t0 = Instant::now();
        let (tx, rx) = mpsc::channel::<Outcome<R>>();
        let mut out: Vec<Outcome<R>> = Vec::new();
        let mut waiting: Vec<String> = Vec::new();
        for id in ids {
            let rec_state = self.reg.state(id);
            if record {
                with_ctx(|c| c.stats.entry(id.clone()).or_default().registry_state = Some(rec_state));
            }
            let skip = rec_state == State::Down || (record && with_ctx(|c| c.failed.contains(id)));
            let client = if skip { None } else { self.reg.client(id) };
            let Some(client) = client else {
                out.push((id.clone(), Err(unavailable(format!("computer {id} is not answering"))), 0));
                continue;
            };
            waiting.push(id.clone());
            let (tx, f, id, d) = (tx.clone(), f.clone(), id.clone(), self.deadline);
            std::thread::spawn(move || {
                let s = Instant::now();
                let r = f(&id, client, d);
                let _ = tx.send((id, r, s.elapsed().as_millis() as u64));
            });
        }
        drop(tx);
        let end = t0 + self.deadline;
        while !waiting.is_empty() {
            let left = end.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(o) => {
                    waiting.retain(|w| *w != o.0);
                    out.push(o);
                }
                Err(_) => break,
            }
        }
        for id in waiting {
            out.push((id, Err(unavailable("timed out")), self.deadline.as_millis() as u64));
        }
        if record {
            with_ctx(|c| {
                for (id, r, ms) in &out {
                    let s = c.stats.entry(id.clone()).or_default();
                    s.elapsed_ms += ms;
                    s.max_call_ms = s.max_call_ms.max(*ms);
                    if let Err(e) = r {
                        s.error = Some(e.message.clone());
                        c.failed.insert(id.clone());
                    }
                }
            });
        }
        out
    }

    fn gather(&self, kind: Kind, f: &DetectionFilter, predicate: Option<Predicate>, page: &Page) -> StoreResult<(Vec<serde_json::Value>, Option<String>)> {
        let limit = page.limit.clamp(1, 10_000) as usize;
        let mut cur = match &page.after {
            Some(a) => Cursor::decode(a)?,
            None => Cursor { v: with_ctx(|c| c.vector.clone()), ..Default::default() },
        };
        // Give up on nodes that already failed in this run (or cannot be reached); they stay flagged if this read needs them.
        let plan = self.plan(f);
        let failed_before = with_ctx(|c| c.failed.clone());
        for id in &plan {
            if failed_before.contains(id) || cur.x.contains(id) {
                with_ctx(|c| {
                    c.failed.insert(id.clone());
                    let err = c.early.get(id).cloned();
                    let s = c.stats.entry(id.clone()).or_default();
                    if s.error.is_none() {
                        s.error = err;
                    }
                });
                cur.x.insert(id.clone());
            }
        }
        let ids: Vec<String> = plan.into_iter().filter(|id| !cur.d.contains(id) && !cur.x.contains(id)).collect();
        let reqs: HashMap<String, ShardReq> = ids
            .iter()
            .map(|id| (id.clone(), ShardReq { filter: f.clone(), page: Page { after: cur.p.get(id).cloned(), limit: limit as u32 }, snapshot: cur.v.get(id).copied(), predicate }))
            .collect();
        let results = self.scatter(&ids, true, move |id, c, d| c.page(kind, &reqs[id], d));
        let mut lists = Vec::new();
        let mut nexts: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (id, r, _) in results {
            match r {
                Ok(rows) => {
                    nexts.insert(id.clone(), rows.next.clone());
                    lists.push((id, rows.rows));
                }
                Err(_) => {
                    cur.x.insert(id);
                }
            }
        }
        let had_rows: BTreeMap<String, bool> = lists.iter().map(|(n, r)| (n.clone(), !r.is_empty())).collect();
        let m = kmerge(lists, limit);
        for (id, next) in &nexts {
            if let Some(k) = m.consumed.get(id) {
                cur.p.insert(id.clone(), key_cursor(k));
            } else if !had_rows[id] {
                if let Some(n) = next {
                    cur.p.insert(id.clone(), n.clone()); // every fetched row was hidden by the snapshot: move past them
                }
            }
            if !m.leftover.contains(id) && next.is_none() {
                cur.d.insert(id.clone());
            }
        }
        with_ctx(|c| {
            for (id, _) in &m.rows {
                c.stats.entry(id.clone()).or_default().rows += 1;
            }
        });
        let more = ids.iter().any(|id| !cur.d.contains(id) && !cur.x.contains(id));
        Ok((m.rows.into_iter().map(|(_, r)| r.v).collect(), more.then(|| cur.encode())))
    }

    fn parse<T: serde::de::DeserializeOwned>(rows: Vec<serde_json::Value>) -> StoreResult<Vec<T>> {
        rows.into_iter().map(|v| serde_json::from_value(v).map_err(ie)).collect()
    }
}

/// Append the vector snapshot of a finished run, so the run can be explained and re-read later.
pub fn remember_vector(run: &QueryRun, vector: &BTreeMap<String, u64>) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir().join("shard_snapshots.jsonl")) {
        let _ = writeln!(f, "{}", serde_json::json!({ "run": run.id, "snapshot": run.snapshot, "vector": vector }));
    }
}

/// `POST /api/v1/federated/{detections|summaries|relations}`: one page of federated rows with the node report.
pub fn browse(api: &Arc<Api>, kind: &str, body: &[u8]) -> Result<Reply, ApiError> {
    let req: ShardReq = serde_json::from_slice(body).map_err(|e| ApiError::new("invalid_request", format!("invalid JSON: {e}")))?;
    let _ = take_run();
    if req.page.after.is_none() {
        api.store.snapshot()?; // starts the run: records the vector this traversal reads at
    }
    let (rows, next) = match kind {
        "detections" => {
            let (r, n) = api.store.detections(&req.filter, &req.page)?;
            (serde_json::to_value(r).map_err(ie)?, n)
        }
        "summaries" => {
            let (r, n) = api.store.summaries(&req.filter, &req.page)?;
            (serde_json::to_value(r).map_err(ie)?, n)
        }
        _ => {
            let (r, n) = api.store.relations(&req.filter, req.predicate, &req.page)?;
            (serde_json::to_value(r).map_err(ie)?, n)
        }
    };
    let rep = take_run().report();
    let mut out = serde_json::json!({ "rows": rows, "next": next, "partial": false, "nodes": [] });
    if let Some(r) = &rep {
        out["partial"] = r.partial.into();
        out["nodes"] = serde_json::Value::Array(r.nodes.clone());
        out["snapshot_vector"] = serde_json::to_value(&r.vector).unwrap_or_default();
        if r.partial {
            out["warnings"] = serde_json::json!([format!("{} of {} computers answered; results may be incomplete", r.answered, r.total)]);
        }
    }
    Ok(Reply::json(&out))
}

impl Store for FederatedStore {
    fn upsert_source(&self, s: &Source) -> StoreResult<()> {
        self.cat.upsert_source(s)
    }
    fn sources(&self) -> StoreResult<Vec<Source>> {
        self.cat.sources()
    }
    fn upsert_media(&self, m: &MediaAsset) -> StoreResult<()> {
        self.cat.upsert_media(m)
    }
    fn media(&self, id: &str) -> StoreResult<Option<MediaAsset>> {
        self.cat.media(id)
    }
    fn media_for_source(&self, source_id: &str) -> StoreResult<Vec<MediaAsset>> {
        self.cat.media_for_source(source_id)
    }
    fn upsert_model(&self, m: &ModelVersion) -> StoreResult<()> {
        self.cat.upsert_model(m)
    }
    fn models(&self) -> StoreResult<Vec<ModelVersion>> {
        self.cat.models()
    }
    fn upsert_job(&self, j: &IndexJob) -> StoreResult<()> {
        self.cat.upsert_job(j)
    }
    fn job(&self, id: &str) -> StoreResult<Option<IndexJob>> {
        self.cat.job(id)
    }
    fn jobs(&self) -> StoreResult<Vec<IndexJob>> {
        self.cat.jobs()
    }
    /// Only reached by workers that do not shard (`CW_SHARDS` off on that worker): the head keeps those rows itself.
    fn commit_batch(&self, b: &IndexBatch) -> StoreResult<BatchOutcome> {
        self.cat.commit_batch(b)
    }

    fn detections(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<(FrameObservation, Detection)>, Option<String>)> {
        let (rows, next) = self.gather(Kind::Detections, f, None, page)?;
        Ok((Self::parse(rows)?, next))
    }

    fn count_detections(&self, f: &DetectionFilter) -> StoreResult<u64> {
        let ids = self.plan(f);
        let vector = with_ctx(|c| c.vector.clone());
        let req = ShardReq { filter: f.clone(), ..Default::default() };
        let res = self.scatter(&ids, true, move |id, c, d| c.count(&ShardReq { snapshot: vector.get(id).copied(), ..req.clone() }, d));
        Ok(res.into_iter().filter_map(|(_, r, _)| r.ok()).sum())
    }

    fn observations(&self, media_id: &str, from_us: Us, to_us: Us) -> StoreResult<Vec<(FrameObservation, Vec<Detection>, Vec<Relation>)>> {
        let ids = self.plan(&DetectionFilter { media_ids: vec![media_id.into()], ..Default::default() });
        let media = media_id.to_string();
        let res = self.scatter(&ids, true, move |_, c, d| c.observations(&media, from_us, to_us, d));
        let mut all: Vec<ObsRow> = Vec::new();
        for (id, r, _) in res {
            if let Ok(v) = r {
                with_ctx(|c| c.stats.entry(id).or_default().rows += v.len() as u64);
                all.extend(v);
            }
        }
        all.sort_by(|a, b| (a.0.media_time_us, &a.0.observation_id).cmp(&(b.0.media_time_us, &b.0.observation_id)));
        all.dedup_by(|a, b| a.0.observation_id == b.0.observation_id);
        Ok(all)
    }

    fn summaries(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<FrameSummary>, Option<String>)> {
        let (rows, next) = self.gather(Kind::Summaries, f, None, page)?;
        Ok((Self::parse(rows)?, next))
    }

    fn relations(&self, f: &DetectionFilter, predicate: Option<Predicate>, page: &Page) -> StoreResult<(Vec<Relation>, Option<String>)> {
        let (rows, next) = self.gather(Kind::Relations, f, predicate, page)?;
        Ok((Self::parse(rows)?, next))
    }

    fn save_query_run(&self, q: &QueryRun, matches: &[ReviewMatch]) -> StoreResult<()> {
        self.cat.save_query_run(q, matches)
    }
    fn query_run(&self, id: &str) -> StoreResult<Option<QueryRun>> {
        self.cat.query_run(id)
    }
    fn matches(&self, query_run_id: &str, page: &Page) -> StoreResult<(Vec<ReviewMatch>, Option<String>)> {
        self.cat.matches(query_run_id, page)
    }
    fn save_review(&self, r: &ReviewSelection, expected_version: Option<u64>) -> StoreResult<ReviewSelection> {
        self.cat.save_review(r, expected_version)
    }
    fn review(&self, id: &str) -> StoreResult<Option<ReviewSelection>> {
        self.cat.review(id)
    }
    fn reviews(&self) -> StoreResult<Vec<ReviewSelection>> {
        self.cat.reviews()
    }
    fn upsert_alert(&self, a: &Alert) -> StoreResult<()> {
        self.cat.upsert_alert(a)
    }
    fn alerts(&self, page: &Page) -> StoreResult<(Vec<Alert>, Option<String>)> {
        self.cat.alerts(page)
    }
    fn audit(&self, r: &AuditRecord) -> StoreResult<()> {
        self.cat.audit(r)
    }
    fn audit_log(&self, text: Option<&str>, page: &Page) -> StoreResult<(Vec<AuditRecord>, Option<String>)> {
        self.cat.audit_log(text, page)
    }
    fn review_match(&self, id: &str) -> StoreResult<Option<ReviewMatch>> {
        self.cat.review_match(id)
    }

    /// Starts a query run on this thread: polls every node's commit sequence (short timeout, cached value for a node
    /// that does not answer) and fixes the vector the run reads at. The scalar is the sum, so it only changes when
    /// some node committed.
    fn snapshot(&self) -> StoreResult<u64> {
        let _ = take_run();
        let ids = self.reg.ids();
        let live: Vec<String> = ids.iter().filter(|id| self.reg.state(id) != State::Down).cloned().collect();
        let res = self.scatter(&live, false, |_, c, _| c.status(Duration::from_millis(1000)));
        let mut vector: BTreeMap<String, u64> = BTreeMap::new();
        for id in &ids {
            if let Some(n) = self.reg.get(id) {
                vector.insert(id.clone(), n.snapshot);
            }
        }
        let mut early = BTreeMap::new();
        for (id, r, _) in res {
            match r {
                Ok(s) => {
                    if id != "head" {
                        self.reg.update_status(&id, &s);
                    }
                    vector.insert(id, s.snapshot);
                }
                Err(e) => {
                    early.insert(id, e.message);
                }
            }
        }
        for id in ids.iter().filter(|id| !live.contains(id)) {
            early.insert(id.clone(), format!("computer {id} is not answering"));
        }
        let total = vector.values().sum();
        with_ctx(|c| {
            c.vector = vector;
            // A node that cannot even report its sequence is skipped for the whole run: one wait, not one per scan.
            c.failed.extend(early.keys().cloned());
            c.early = early;
        });
        Ok(total)
    }
}
