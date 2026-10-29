//! Lua queries: `Request.lua`. A sandboxed script reads the store through paged iterators
//! (`db.detections{}`, `db.frames{}`, `db.relations{}`, `db.tracks{}`) that use the same
//! pushdown and scan budget as SQL, and calls `emit(...)` to add review matches.
//!
//! Sandbox: only base/math/string/table; no io/os/package/debug/load/require; 16 MiB memory
//! limit; instruction-count and wall-clock budget. Instruction/deadline overruns return what
//! was emitted so far plus a warning; memory overruns and script errors are `runtime_error`.

use super::*;
use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Table as LT, Value as LV, VmState};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

const MAX_SRC: usize = 32 * 1024;
const MEM_LIMIT: usize = 16 * 1024 * 1024;
const TICK: u32 = 1000;
const MAX_TICKS: u32 = 100_000; // 100M VM instructions
const MAX_EMIT: usize = 100_000;

const PRELUDE: &str = r#"
local open, nxt = __open, __next
db = {}
for _, k in ipairs{'detections', 'frames', 'relations', 'tracks'} do
  db[k] = function(a) local h = open(k, a or {}); return function() return nxt(h) end end
end
function collect(it) local t = {} for r in it do t[#t + 1] = r end return t end
function count(t)
  if type(t) == 'function' then local n = 0 for _ in t do n = n + 1 end return n end
  return #t
end
function distinct(t, key)
  if type(t) == 'function' then t = collect(t) end
  local seen, n = {}, 0
  for _, r in ipairs(t) do
    local v = r[key]
    if v ~= nil then
      if key == 'track_id' then v = tostring(r.media_id) .. '/' .. tostring(v) end
      if not seen[v] then seen[v] = true n = n + 1 end
    end
  end
  return n
end
"#;

enum LRow {
    Det(FrameObservation, Detection),
    Frame(FrameSummary),
    Rel(Relation),
    Track(String, TrackRow),
}

struct It {
    kind: String,
    filter: DetectionFilter,
    pred: Option<Predicate>,
    buf: VecDeque<LRow>,
    after: Option<String>,
    done: bool,
}

struct St {
    sc: Scan,
    iters: Vec<It>,
    seen: HashMap<String, (Point, ResultKind)>,
    tracks: HashMap<String, Vec<Point>>,
    emitted: Vec<(Point, ResultKind)>,
    warnings: Vec<String>,
    fatal: Option<ApiError>,
    missing: u64,
    holding: bool,
}

fn point(o: &FrameObservation, det: Option<&Detection>, c: f32) -> Point {
    Point {
        source_id: o.source_id.clone(),
        media_id: o.media_id.clone(),
        observation_id: o.observation_id.clone(),
        media_time_us: o.media_time_us,
        detection_id: det.map(|d| d.detection_id.clone()),
        relation_id: None,
        track_id: det.and_then(|d| d.track_id.clone()),
        confidence: c,
    }
}

fn opt<T: mlua::IntoLua>(t: &LT, k: &str, v: Option<T>) -> mlua::Result<()> {
    if let Some(v) = v {
        t.set(k, v)?;
    }
    Ok(())
}

fn strs(a: &LT, k: &str) -> mlua::Result<Vec<String>> {
    Ok(match a.get::<LV>(k)? {
        LV::String(s) => vec![s.to_string_lossy().to_string()],
        LV::Table(t) => t.sequence_values::<String>().collect::<Result<_, _>>()?,
        _ => vec![],
    })
}

fn row_table(lua: &Lua, st: &mut St, r: &LRow) -> mlua::Result<LT> {
    let t = lua.create_table()?;
    match r {
        LRow::Det(o, d) => {
            t.set("observation_id", o.observation_id.as_str())?;
            t.set("detection_id", d.detection_id.as_str())?;
            t.set("source_id", o.source_id.as_str())?;
            t.set("media_id", o.media_id.as_str())?;
            t.set("class", d.class.as_str())?;
            t.set("confidence", d.confidence.to_string().parse::<f64>().unwrap_or(d.confidence as f64))?;
            t.set("model_version", d.model_version.as_str())?;
            opt(&t, "track_id", d.track_id.as_deref())?;
            t.set("media_time_us", o.media_time_us)?;
            opt(&t, "capture_time_utc_us", o.capture_time_utc_us)?;
            st.seen.entry(o.observation_id.clone()).or_insert((point(o, None, d.confidence), ResultKind::Detections));
        }
        LRow::Frame(s) => {
            t.set("observation_id", s.observation_id.as_str())?;
            t.set("source_id", s.source_id.as_str())?;
            t.set("media_id", s.media_id.as_str())?;
            t.set("media_time_us", s.media_time_us)?;
            opt(&t, "capture_time_utc_us", s.capture_time_utc_us)?;
            t.set("min_confidence", s.min_confidence.to_string().parse::<f64>().unwrap_or(0.0))?;
            // Unknown / unsupported counts are absent (nil), never zero.
            let (c, r) = (lua.create_table()?, lua.create_table()?);
            for (k, v) in &s.class_counts {
                if let Some(n) = v.known() {
                    c.set(k.as_str(), n)?;
                }
            }
            for (k, v) in &s.relation_counts {
                if let Some(n) = v.known() {
                    r.set(k.as_str(), n)?;
                }
            }
            t.set("counts", c)?;
            t.set("relations", r)?;
            st.seen.insert(
                s.observation_id.clone(),
                (
                    Point {
                        source_id: s.source_id.clone(),
                        media_id: s.media_id.clone(),
                        observation_id: s.observation_id.clone(),
                        media_time_us: s.media_time_us,
                        detection_id: None,
                        relation_id: None,
                        track_id: None,
                        confidence: s.min_confidence,
                    },
                    ResultKind::Frames,
                ),
            );
        }
        LRow::Rel(r) => {
            t.set("relation_id", r.relation_id.as_str())?;
            t.set("observation_id", r.observation_id.as_str())?;
            t.set("detection_id", r.subject_detection_id.as_str())?;
            t.set("predicate", pred_name(r.predicate))?;
            t.set("object_class", r.object_class.as_str())?;
            opt(&t, "track_id", r.subject_track_id.as_deref())?;
            opt(&t, "subject_track_id", r.subject_track_id.as_deref())?;
            opt(&t, "object_track_id", r.object_track_id.as_deref())?;
            t.set("confidence", r.confidence.to_string().parse::<f64>().unwrap_or(0.0))?;
            t.set("module_version", r.module_version.as_str())?;
        }
        LRow::Track(key, tr) => {
            t.set("key", key.as_str())?;
            t.set("track_id", tr.track_id.as_str())?;
            t.set("source_id", tr.source_id.as_str())?;
            t.set("media_id", tr.media_id.as_str())?;
            t.set("class", tr.class.as_str())?;
            t.set("model_version", tr.model_version.as_str())?;
            t.set("confidence", tr.confidence.to_string().parse::<f64>().unwrap_or(0.0))?;
            t.set("media_time_us", tr.first_us)?;
            t.set("start_us", tr.first_us)?;
            t.set("end_us", tr.last_us)?;
            t.set("observations", tr.pairs.iter().map(|(o, _)| &o.observation_id).collect::<HashSet<_>>().len())?;
        }
    }
    Ok(t)
}

fn to_json(v: &LV, depth: usize) -> Value {
    if depth > 8 {
        return Value::Null;
    }
    match v {
        LV::Boolean(b) => json!(b),
        LV::Integer(i) => json!(i),
        LV::Number(n) if n.is_finite() => json!(n),
        LV::String(s) => json!(s.to_string_lossy().to_string()),
        LV::Table(t) => {
            let n = t.raw_len();
            if n > 0 {
                Value::Array((1..=n.min(10_000)).filter_map(|i| t.raw_get::<LV>(i).ok()).map(|x| to_json(&x, depth + 1)).collect())
            } else {
                let mut m = serde_json::Map::new();
                for (k, x) in t.clone().pairs::<LV, LV>().flatten().take(1000) {
                    let key = match &k {
                        LV::String(s) => s.to_string_lossy().to_string(),
                        LV::Integer(i) => i.to_string(),
                        _ => continue,
                    };
                    m.insert(key, to_json(&x, depth + 1));
                }
                Value::Object(m)
            }
        }
        _ => Value::Null,
    }
}

fn open_iter(store: &dyn Store, st: &mut St, lim: &Limits, scope: &QueryScope, kind: &str, a: &LT) -> mlua::Result<usize> {
    let ext = |m: String| mlua::Error::external(m);
    let mut f = DetectionFilter {
        source_ids: scope.source_ids.clone(),
        media_ids: scope.media_ids.clone(),
        utc_from_us: scope.from_utc_us,
        utc_to_us: scope.to_utc_us,
        ..Default::default()
    };
    let mut dead = false;
    narrow(&mut f.source_ids, strs(a, "source_ids")?, &mut dead);
    narrow(&mut f.media_ids, strs(a, "media_ids")?, &mut dead);
    let mut cls = strs(a, "class")?;
    cls.extend(strs(a, "classes")?);
    f.classes = cls;
    f.model_versions = strs(a, "model_version")?;
    f.track_ids = strs(a, "track_id")?;
    f.min_confidence = a.get::<Option<f64>>("min_confidence")?.map(|c| c as f32);
    f.media_from_us = a.get("from_us")?;
    f.media_to_us = a.get("to_us")?;
    // narrow() on empty lists is a no-op; a non-empty request intersecting a non-empty scope may be dead.
    let mut pred = None;
    if kind == "relations" {
        if let Some(p) = a.get::<Option<String>>("predicate")? {
            pred = match p.to_ascii_lowercase().as_str() {
                "near" => Some(Predicate::Near),
                "holding" => Some(Predicate::Holding),
                o => return Err(ext(format!("unknown predicate '{o}'"))),
            };
        }
        if pred == Some(Predicate::Holding) && !st.holding {
            st.fatal = Some(ApiError::new(
                "unsupported",
                "'holding' needs an installed relation module that declares Holding support (hand/pose-object model); use 'near' for proximity only",
            ));
            return Err(ext("unsupported: holding".into()));
        }
        // Place relations on the timeline: resolve their observations (bounded, shared budget).
        let f2 = DetectionFilter { source_ids: f.source_ids.clone(), media_ids: f.media_ids.clone(), utc_from_us: f.utc_from_us, utc_to_us: f.utc_to_us, ..Default::default() };
        let mut after = None;
        while !dead && st.sc.partial.is_none() {
            if st.sc.start.elapsed() >= lim.deadline || st.sc.scanned >= lim.max_scan {
                st.sc.partial = Some("scan budget reached".into());
                break;
            }
            let (rows, next) = store.detections(&f2, &Page { after: after.clone(), limit: lim.page }).map_err(|e| ext(e.message))?;
            for (o, d) in rows {
                st.sc.scanned += 1;
                st.seen.entry(o.observation_id.clone()).or_insert((point(&o, None, d.confidence), ResultKind::Relations));
            }
            match next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
    }
    let mut buf = VecDeque::new();
    let mut done = dead || kind == "tracks";
    if kind == "tracks" && !dead {
        let mut by: BTreeMap<(String, String), TrackRow> = BTreeMap::new();
        let mut after = None;
        loop {
            if st.sc.start.elapsed() >= lim.deadline || st.sc.scanned >= lim.max_scan {
                st.sc.partial = Some("scan budget reached".into());
                break;
            }
            let (rows, next) = store.detections(&f, &Page { after: after.clone(), limit: lim.page }).map_err(|e| ext(e.message))?;
            for (o, d) in rows {
                st.sc.scanned += 1;
                if let Some(tid) = d.track_id.clone() {
                    let e = by.entry((o.media_id.clone(), tid.clone())).or_insert_with(|| TrackRow {
                        source_id: o.source_id.clone(), media_id: o.media_id.clone(), track_id: tid, class: d.class.clone(),
                        model_version: d.model_version.clone(), confidence: 0.0, first_us: o.media_time_us, last_us: o.media_time_us, pairs: vec![],
                    });
                    e.confidence = e.confidence.max(d.confidence);
                    e.first_us = e.first_us.min(o.media_time_us);
                    e.last_us = e.last_us.max(o.media_time_us);
                    e.pairs.push((o, d));
                }
            }
            match next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
        for ((m, t), tr) in by {
            let key = format!("{m}\0{t}");
            st.tracks.insert(key.clone(), tr.pairs.iter().map(|(o, d)| point(o, Some(d), d.confidence)).collect());
            buf.push_back(LRow::Track(key, tr));
        }
        done = true;
    }
    st.iters.push(It { kind: kind.to_string(), filter: f, pred, buf, after: None, done });
    Ok(st.iters.len() - 1)
}

fn next_row(store: &dyn Store, st: &mut St, lim: &Limits, h: usize) -> mlua::Result<Option<LRow>> {
    loop {
        let St { sc, iters, .. } = &mut *st;
        let it = iters.get_mut(h).ok_or_else(|| mlua::Error::external("bad iterator"))?;
        if let Some(r) = it.buf.pop_front() {
            return Ok(Some(r));
        }
        if it.done {
            return Ok(None);
        }
        if sc.start.elapsed() >= lim.deadline {
            sc.partial = Some("deadline exceeded".into());
            return Ok(None);
        }
        if sc.scanned >= lim.max_scan {
            sc.partial = Some(format!("scan limit of {} rows reached", lim.max_scan));
            return Ok(None);
        }
        let page = Page { after: it.after.clone(), limit: lim.page };
        let ext = |e: ApiError| mlua::Error::external(e.message);
        let next = match it.kind.as_str() {
            "detections" => {
                let (rows, n) = store.detections(&it.filter, &page).map_err(ext)?;
                it.buf.extend(rows.into_iter().map(|(o, d)| LRow::Det(o, d)));
                sc.scanned += it.buf.len() as u64;
                n
            }
            "frames" => {
                let (rows, n) = store.summaries(&it.filter, &page).map_err(ext)?;
                it.buf.extend(rows.into_iter().map(LRow::Frame));
                sc.scanned += it.buf.len() as u64;
                n
            }
            _ => {
                let (rows, n) = store.relations(&it.filter, it.pred, &page).map_err(ext)?;
                it.buf.extend(rows.into_iter().map(LRow::Rel));
                sc.scanned += it.buf.len() as u64;
                n
            }
        };
        match next {
            Some(c) => it.after = Some(c),
            None => it.done = true,
        }
    }
}

fn emit(st: &mut St, v: LV) -> mlua::Result<()> {
    let (obs, det, rel, key) = match &v {
        LV::String(s) => (s.to_string_lossy().to_string(), None, None, None),
        LV::Table(t) => (
            t.get::<Option<String>>("observation_id")?.unwrap_or_default(),
            t.get::<Option<String>>("detection_id")?,
            t.get::<Option<String>>("relation_id")?,
            t.get::<Option<String>>("key")?,
        ),
        _ => return Err(mlua::Error::external("emit() takes an observation id or a row")),
    };
    if st.emitted.len() >= MAX_EMIT {
        if !st.warnings.iter().any(|w| w.contains("emit limit")) {
            st.warnings.push(format!("emit limit of {MAX_EMIT} reached; later emits ignored"));
        }
        return Ok(());
    }
    if let Some(k) = key {
        if let Some(pts) = st.tracks.get(&k) {
            let pts = pts.clone();
            st.emitted.extend(pts.into_iter().map(|p| (p, ResultKind::Tracks)));
        }
        return Ok(());
    }
    match st.seen.get(&obs).cloned() {
        Some((mut p, k)) => {
            if det.is_some() {
                p.detection_id = det.clone();
            }
            if let (Some(r), LV::Table(t)) = (&rel, &v) {
                p.relation_id = Some(r.clone());
                p.track_id = t.get::<Option<String>>("track_id")?;
                p.confidence = t.get::<Option<f64>>("confidence")?.unwrap_or(0.0) as f32;
            } else if let LV::Table(t) = &v {
                if det.is_some() {
                    p.track_id = t.get::<Option<String>>("track_id")?;
                    p.confidence = t.get::<Option<f64>>("confidence")?.map_or(p.confidence, |c| c as f32);
                }
            }
            let kind = if rel.is_some() { ResultKind::Relations } else { k };
            st.emitted.push((p, kind));
        }
        None => st.missing += 1,
    }
    Ok(())
}

pub(super) fn run(store: &dyn Store, req: &Request, lim: &Limits) -> Result<Outcome, ApiError> {
    let t0 = Instant::now();
    let src = req.lua.as_deref().unwrap_or("").trim();
    if req.sql.as_ref().is_some_and(|s| !s.trim().is_empty()) || req.class.is_some() || req.min_confidence.is_some() {
        return Err(ApiError::new("invalid_request", "`lua` cannot be combined with sql, class or min_confidence"));
    }
    if src.is_empty() || src.len() > MAX_SRC {
        return Err(if src.is_empty() { ApiError::new("invalid_request", "empty Lua script") } else { too_large(format!("Lua script exceeds {MAX_SRC} bytes")) });
    }
    for (n, v) in [("gap_us", req.gap_us), ("pre_us", req.pre_us), ("post_us", req.post_us)] {
        if v.is_some_and(|v| v < 0) {
            return Err(ApiError::new("invalid_request", format!("{n} must not be negative")));
        }
    }
    let models = store.models()?;
    let snapshot = store.snapshot()?;
    let sha: String = Sha256::digest(src.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let normalized = format!("lua:{sha}");
    let limit = req.limit.unwrap_or(MAX_ROWS).min(MAX_ROWS) as usize;
    let opts = matches::Opts {
        gap_us: req.gap_us.unwrap_or(2_000_000),
        pre_us: req.pre_us.unwrap_or(3_000_000),
        post_us: req.post_us.unwrap_or(3_000_000),
        ..Default::default()
    };
    let run_id = stable_id(
        "qr",
        &[&normalized, &serde_json::to_string(&req.scope).unwrap_or_default(), &snapshot.to_string(), &format!("{}/{}/{}/{}", opts.gap_us, opts.pre_us, opts.post_us, limit)],
    );

    let st = RefCell::new(St {
        sc: Scan { start: t0, scanned: 0, partial: None },
        iters: vec![],
        seen: HashMap::new(),
        tracks: HashMap::new(),
        emitted: vec![],
        warnings: vec![],
        fatal: None,
        missing: 0,
        holding: models.iter().any(|m| m.supported_relations.contains(&Predicate::Holding)),
    });

    let e = |e: mlua::Error| ApiError::new("runtime_error", e.to_string());
    let lua = Lua::new_with(StdLib::MATH | StdLib::STRING | StdLib::TABLE, LuaOptions::new()).map_err(e)?;
    lua.set_memory_limit(MEM_LIMIT).map_err(e)?;
    let g = lua.globals();
    for name in ["load", "loadfile", "loadstring", "dofile", "require", "collectgarbage", "print", "debug", "io", "os", "package"] {
        g.set(name, LV::Nil).map_err(e)?;
    }
    if let Ok(s) = g.get::<LT>("string") {
        s.set("dump", LV::Nil).map_err(e)?;
    }
    let ticks = Arc::new(AtomicU32::new(0));
    let budget = Arc::new(AtomicBool::new(false));
    let (t2, b2, deadline) = (ticks.clone(), budget.clone(), lim.deadline);
    lua.set_hook(HookTriggers { every_nth_instruction: Some(TICK), ..HookTriggers::default() }, move |_, _| {
        if t2.fetch_add(1, Ordering::Relaxed) >= MAX_TICKS || t0.elapsed() >= deadline {
            b2.store(true, Ordering::Relaxed);
            Err(mlua::Error::runtime("budget exceeded"))
        } else {
            Ok(VmState::Continue)
        }
    });

    let mut values = Vec::new();
    let res: Result<(), mlua::Error> = lua.scope(|scope| {
        let open = scope.create_function_mut(|_, (kind, a): (String, LT)| {
            let mut s = st.borrow_mut();
            open_iter(store, &mut s, lim, &req.scope, &kind, &a).map(|h| h as i64)
        })?;
        let nxt = scope.create_function_mut(|lua, h: i64| {
            let mut s = st.borrow_mut();
            match next_row(store, &mut s, lim, h as usize)? {
                Some(r) => Ok(LV::Table(row_table(lua, &mut s, &r)?)),
                None => Ok(LV::Nil),
            }
        })?;
        let emit_fn = scope.create_function_mut(|_, v: LV| emit(&mut st.borrow_mut(), v))?;
        g.set("__open", open)?;
        g.set("__next", nxt)?;
        g.set("emit", emit_fn)?;
        lua.load(PRELUDE).set_name("=prelude").exec()?;
        g.set("__open", LV::Nil)?;
        g.set("__next", LV::Nil)?;
        let f = lua.load(src).set_name("=lua").into_function()?;
        let mut ret: LV = f.call(())?;
        if let LV::Function(func) = ret {
            ret = func.call(g.get::<LV>("db")?)?;
        }
        match to_json(&ret, 0) {
            Value::Null => {}
            Value::Array(a) => values = a,
            v => values = vec![v],
        }
        Ok(())
    });
    let mut st = st.into_inner();
    if let Some(f) = st.fatal.take() {
        return Err(f);
    }
    if let Err(err) = res {
        if budget.load(Ordering::Relaxed) {
            st.warnings.push(format!("partial results: script stopped by its instruction/time budget after {} rows scanned", st.sc.scanned));
        } else {
            let msg = err.to_string();
            return Err(match err {
                mlua::Error::SyntaxError { .. } => ApiError::new("parse_error", msg),
                _ if msg.contains("not enough memory") => ApiError::new("runtime_error", "script exceeded its 16 MiB memory limit"),
                _ => ApiError::new("runtime_error", msg),
            });
        }
    }

    // Materialize emitted evidence exactly like SQL.
    let emitted_rows = st.emitted.len();
    let kinds: BTreeSet<u8> = st.emitted.iter().map(|(_, k)| *k as u8).collect();
    let kind = match (st.emitted.first(), kinds.len()) {
        (Some((_, k)), 1) => *k,
        (Some(_), _) => ResultKind::Detections,
        (None, _) if !values.is_empty() => ResultKind::Aggregate,
        _ => ResultKind::Detections,
    };
    let pts: Vec<Point> = st.emitted.iter().take(limit).map(|(p, _)| p.clone()).collect();
    let media = media_map(store, pts.iter().map(|p| p.media_id.clone()))?;
    let out_matches = matches::windows_from_points(&pts, &media, &opts, &run_id);
    let distinct: HashSet<(&str, &str)> = pts.iter().map(|p| (p.observation_id.as_str(), p.detection_id.as_deref().unwrap_or(""))).collect();
    let result_count = if pts.is_empty() && !values.is_empty() {
        match values.first() {
            Some(Value::Number(n)) if values.len() == 1 => n.as_f64().map_or(1, |f| f.max(0.0) as u64),
            _ => values.len() as u64,
        }
    } else {
        distinct.len() as u64
    };
    let unit = if pts.is_empty() && !values.is_empty() { "value" } else { "observations" }.to_string();
    let distinct_tracks = Some(pts.iter().filter_map(|p| p.track_id.as_ref().map(|t| (&p.media_id, t))).collect::<HashSet<_>>().len() as u64);

    let mut warnings = std::mem::take(&mut st.warnings);
    if let Some(r) = &st.sc.partial {
        warnings.push(format!("partial results: scan stopped after {} rows ({r}); narrow the scope or add filters", st.sc.scanned));
    }
    if emitted_rows > limit {
        warnings.push(format!("results limited to {limit} rows"));
    }
    if st.missing > 0 {
        warnings.push(format!("{} emit() call(s) ignored: observation was not read by this script (iterate it first)", st.missing));
    }
    let (cov, cov_partial) = coverage(store, &req.scope)?;
    if result_count == 0 && (cov_partial || st.sc.partial.is_some() || budget.load(Ordering::Relaxed)) {
        warnings.push("zero results are not evidence of absence: indexing coverage is unknown or partial for this scope".into());
    }
    if kind == ResultKind::Aggregate {
        warnings.push(format!("unit: {unit}"));
    }
    let run = QueryRun {
        id: run_id,
        schema_version: SCHEMA_VERSION,
        normalized,
        scope: req.scope.clone(),
        kind,
        snapshot,
        coverage: cov,
        warnings,
        result_count,
        elapsed_us: t0.elapsed().as_micros() as u64,
        actor: req.actor.clone(),
        cursor: None,
    };
    store.save_query_run(&run, &out_matches)?;
    Ok(Outcome { run, matches: out_matches, unit, distinct_tracks, values })
}
