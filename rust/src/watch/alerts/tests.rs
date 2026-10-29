use super::*;
use crate::watch::config;

const S: i64 = 1_000_000;

fn fs(src: &str, t_ms: i64, people: Option<u32>, hold_phone: Option<u32>) -> FrameSummary {
    let mut class_counts = BTreeMap::new();
    if let Some(p) = people {
        class_counts.insert("person".to_string(), Count::Known(p));
        class_counts.insert("cell phone".to_string(), Count::Known(14));
    }
    let mut relation_counts = BTreeMap::new();
    if let Some(h) = hold_phone {
        relation_counts.insert("holding:cell phone".to_string(), Count::Known(h));
    }
    FrameSummary {
        observation_id: format!("obs_{src}_{t_ms}"), source_id: src.into(), media_id: "m1".into(),
        media_time_us: t_ms * 1000, capture_time_utc_us: None, zone: None, min_confidence: 0.4,
        class_counts, relation_counts, quality: Quality::Ok,
    }
}

fn spec(id: &str, cond: Condition, hold_ms: i64) -> RuleSpec {
    RuleSpec {
        id: id.into(), version: "1".into(), name: id.into(), scope: Scope::default(), condition: cond, hold_ms,
        cooldown_ms: 0, severity: "warning".into(), min_confidence: 0.0, unknown_policy: UnknownPolicy::Break,
        max_gap_ms: 1500, enabled: true, migration_warning: None,
    }
}
fn lua(id: &str, src: &str, hold_ms: i64) -> RuleSpec {
    spec(id, Condition::Lua(src.into()), hold_ms)
}
fn eng(r: RuleSpec) -> Engine {
    Engine::new(vec![r]).with_clock(|| 7)
}
fn phones_ratio() -> &'static str {
    "return function(ctx) local r = ctx:ratio_people_with('holding','cell phone') return r >= 0.20 end"
}
/// People=60 summaries every `step_ms`: `true` frames when `on(t)`.
fn feed(e: &mut Engine, from_ms: i64, to_ms: i64, step_ms: i64, on: impl Fn(i64) -> bool) -> Vec<Alert> {
    let mut out = vec![];
    let mut t = from_ms;
    while t <= to_ms {
        out.extend(e.observe(&fs("a", t, Some(60), Some(if on(t) { 12 } else { 3 }))));
        t += step_ms;
    }
    out
}

#[test]
fn twelve_of_sixty_is_twenty_percent() {
    let mut e = eng(lua("r", phones_ratio(), 0));
    let a = e.observe(&fs("a", 0, Some(60), Some(12)));
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].state, AlertState::Active);
    assert_eq!(a[0].evidence, vec!["obs_a_0".to_string()]);
    let mut e = eng(lua("r", phones_ratio(), 0));
    assert!(e.observe(&fs("a", 0, Some(60), Some(11))).is_empty());
}

#[test]
fn zero_denominator_is_unknown_not_alert() {
    let mut e = eng(lua("r", phones_ratio(), 0));
    assert!(e.observe(&fs("a", 0, Some(0), Some(0))).is_empty());
    assert!(e.diagnostics().iter().any(|d| d.code == "zero_denominator"));
    assert!(e.diagnostics().iter().all(|d| d.level != "error"));
}

#[test]
fn unsupported_relation_is_unknown_with_diagnostic() {
    let mut e = eng(lua("r", phones_ratio(), 0));
    assert!(e.observe(&fs("a", 0, Some(60), None)).is_empty());
    assert!(e.diagnostics().iter().any(|d| d.code == "unsupported" && d.message.contains("holding")));
    // class not analyzed at all
    let mut e = eng(lua("k", "ctx:count('knife') == 0", 0));
    assert!(e.observe(&fs("a", 0, Some(60), Some(1))).is_empty());
    assert!(e.diagnostics().iter().any(|d| d.code == "unknown"));
}

#[test]
fn dropped_frames_break_the_hold() {
    let mut e = eng(lua("r", phones_ratio(), 3000));
    // frames at 0,1000 then a 3 s hole, then 4000.. : window restarts at 4000
    let mut out = vec![];
    for t in [0, 1000, 4000, 5000, 6000, 7000] {
        out.extend(e.observe(&fs("a", t, Some(60), Some(12))));
    }
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].start_us, 4000 * 1000);
    // control: no hole -> active at 3000 with start 0
    let mut e = eng(lua("r", phones_ratio(), 3000));
    let a = feed(&mut e, 0, 3000, 1000, |_| true);
    assert_eq!((a.len(), a[0].start_us), (1, 0));
}

#[test]
fn varying_fps_same_elapsed_time_same_result() {
    let run = |step| {
        let mut e = eng(lua("r", phones_ratio(), 5000));
        let a = feed(&mut e, 0, 10_000, step, |_| true);
        (a.len(), a[0].id.clone(), a[0].start_us)
    };
    let (n1, id1, s1) = run(1000); // 1 fps: 11 frames
    let (n2, id2, s2) = run(100); // 10 fps: 101 frames
    assert_eq!((n1, n2), (1, 1));
    assert_eq!(id1, id2);
    assert_eq!(s1, s2);
    // too little elapsed time never fires, however many frames
    let mut e = eng(lua("r", phones_ratio(), 5000));
    assert!(feed(&mut e, 0, 4900, 10, |_| true).is_empty());
}

#[test]
fn infinite_loop_is_stopped_and_engine_continues() {
    let mut e = Engine::new(vec![
        lua("bad", "return function(ctx) while true do end end", 0),
        spec("good", Condition::Dsl("count(\"cell phone\") >= 4".into()), 0),
    ]);
    let t0 = std::time::Instant::now();
    let a = e.observe(&fs("a", 0, Some(60), Some(12)));
    assert!(t0.elapsed().as_secs() < 5);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].rule_id, "good");
    assert!(e.diagnostics().iter().any(|d| d.rule_id == "bad" && d.code == "budget_exceeded" && d.level == "error"));
    // the bad rule is still installed (fresh state) and the engine keeps running
    assert!(e.observe(&fs("a", 1000, Some(60), Some(12))).is_empty());
    let d = e.diagnostics().iter().find(|d| d.rule_id == "bad").unwrap();
    assert_eq!(d.count, 2);
}

#[test]
fn top_level_loop_and_memory_bomb_are_stopped() {
    let mut e = Engine::new(vec![lua("top", "return (function() while true do end end)()", 0)]);
    assert!(e.observe(&fs("a", 0, Some(1), Some(1))).is_empty());
    assert!(e.diagnostics().iter().any(|d| d.code == "compile_error" && d.message.contains("budget")), "{:?}", e.diagnostics());

    let mut e = Engine::new(vec![
        lua("mem", "return function(ctx) local s = string.rep('x', 1000000000) return #s > 0 end", 0),
        lua("mem2", "return function(ctx) local t = {} for i = 1, 100000000 do t[i] = {i} end return true end", 0),
    ]);
    assert!(e.observe(&fs("a", 0, Some(1), Some(1))).is_empty());
    let codes: Vec<(String, String)> = e.diagnostics().iter().map(|d| (d.rule_id.clone(), d.code.clone())).collect();
    assert!(codes.contains(&("mem".into(), "memory_limit".into())), "{codes:?}");
    assert!(codes.iter().any(|(r, c)| r == "mem2" && (c == "memory_limit" || c == "budget_exceeded")), "{codes:?}");
}

#[test]
fn sandbox_has_no_host_access() {
    let src = "return function(ctx) return io == nil and os == nil and load == nil and require == nil and dofile == nil and debug == nil and package == nil and loadfile == nil and string.dump == nil end";
    let mut e = eng(lua("sb", src, 0));
    assert_eq!(e.observe(&fs("a", 0, Some(1), Some(1))).len(), 1, "{:?}", e.diagnostics());
    let mut e = eng(lua("sb2", "return function(ctx) return os.execute('true') end", 0));
    assert!(e.observe(&fs("a", 0, Some(1), Some(1))).is_empty());
    assert!(e.diagnostics().iter().any(|d| d.code == "rule_error"));
}

#[test]
fn restart_via_snapshot_continues_same_alert_without_duplicates() {
    let r = lua("r", phones_ratio(), 3000);
    let mut e1 = eng(r.clone());
    let a = feed(&mut e1, 0, 3000, 1000, |_| true);
    assert_eq!(a.len(), 1);
    let id = a[0].id.clone();
    let snap = serde_json::to_string(&e1.snapshot()).unwrap();

    let mut e2 = eng(r);
    assert!(e2.restore(serde_json::from_str(&snap).unwrap()).is_empty());
    // failover replays older frames, then new ones arrive; nothing is re-emitted as new
    assert!(feed(&mut e2, 0, 4000, 1000, |_| true).is_empty());
    let out = feed(&mut e2, 5000, 5000, 1000, |_| false);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, id);
    assert_eq!(out[0].state, AlertState::Resolved);
    assert_eq!(out[0].end_us, Some(4000 * 1000));
    assert_eq!(e2.alerts().len(), 1);
}

#[test]
fn same_observation_twice_is_a_noop() {
    let mut e = eng(lua("r", phones_ratio(), 0));
    let f = fs("a", 0, Some(60), Some(12));
    assert_eq!(e.observe(&f).len(), 1);
    assert!(e.observe(&f).is_empty());
}

#[test]
fn cooldown_suppresses_refire() {
    let mut r = lua("r", phones_ratio(), 1000);
    r.cooldown_ms = 5000;
    let mut e = eng(r);
    let on = |t: i64| !(2000..3000).contains(&t) ;
    let out = feed(&mut e, 0, 8000, 1000, on);
    // active@1000, resolved at 2000 (end=1000), cooldown until 6000, new window starts 6000, active@7000
    let kinds: Vec<(AlertState, i64)> = out.iter().map(|a| (a.state, a.start_us / S)).collect();
    assert_eq!(kinds, vec![(AlertState::Active, 0), (AlertState::Resolved, 0), (AlertState::Active, 6)]);
    assert_ne!(out[0].id, out[2].id);
}

#[test]
fn decision_survives_machine_updates() {
    let mut e = eng(lua("r", phones_ratio(), 0));
    let a = e.observe(&fs("a", 0, Some(60), Some(12)));
    let id = a[0].id.clone();
    assert_eq!(e.decide("nope", Decision::Confirmed, "alice").unwrap_err().code, "not_found");
    assert!(e.decide(&id, Decision::Confirmed, " ").is_err());
    let d = e.decide(&id, Decision::Confirmed, "alice").unwrap();
    assert_eq!((d.decision, d.state), (Decision::Confirmed, AlertState::Active));
    let r = e.observe(&fs("a", 1000, Some(60), Some(1)));
    assert_eq!(r[0].state, AlertState::Resolved);
    assert_eq!((r[0].decision, r[0].actor.as_deref()), (Decision::Confirmed, Some("alice")));
    // snapshot round trip keeps the decision
    let mut e2 = eng(lua("r", phones_ratio(), 0));
    e2.restore(e.snapshot());
    assert_eq!(e2.alert(&id).unwrap().decision, Decision::Confirmed);
}

#[test]
fn unknown_policies() {
    let frames = |e: &mut Engine| {
        let mut out = vec![];
        out.extend(e.observe(&fs("a", 0, Some(60), Some(12))));
        let mut blur = fs("a", 1000, Some(60), Some(12));
        blur.quality = Quality::Blurry;
        out.extend(e.observe(&blur));
        out.extend(e.observe(&fs("a", 2000, Some(60), Some(12))));
        out.extend(e.observe(&fs("a", 3000, Some(60), Some(12))));
        out
    };
    let mut brk = lua("r", phones_ratio(), 3000);
    brk.unknown_policy = UnknownPolicy::Break;
    assert!(frames(&mut eng(brk)).is_empty(), "blurry frame breaks the window");
    let mut hold = lua("r", phones_ratio(), 3000);
    hold.unknown_policy = UnknownPolicy::Hold;
    hold.max_gap_ms = 2500;
    let o = frames(&mut eng(hold));
    assert_eq!((o.len(), o[0].start_us), (1, 0));
    let mut ign = lua("r", phones_ratio(), 3000);
    ign.unknown_policy = UnknownPolicy::Ignore;
    ign.max_gap_ms = 2500;
    assert_eq!(frames(&mut eng(ign)).len(), 1);
}

#[test]
fn legacy_dsl_compiles_and_evaluates_three_valued() {
    for line in [r#"count("cell phone") > 40"#, r#"seen("pistol") or seen("knife")"#, r#"ratio("cell phone","person") >= 0.2"#,
                 r#"lua: count("cell phone") >= 4 and count("person") > 0"#] {
        spec("l", Condition::Dsl(line.into()), 0).validate().unwrap();
    }
    assert!(spec("l", Condition::Dsl("count(phone) > 1".into()), 0).validate().is_err());
    let mut e = eng(spec("l", Condition::Dsl(r#"ratio("cell phone","person") >= 0.2"#.into()), 0));
    assert_eq!(e.observe(&fs("a", 0, Some(60), Some(1))).len(), 1); // 14/60 visible phones
    let mut e = eng(spec("l", Condition::Dsl(r#"count("knife") == 0"#.into()), 0));
    assert!(e.observe(&fs("a", 0, Some(60), None)).is_empty()); // knife not analyzed: Unknown, not zero
    assert!(e.diagnostics().iter().any(|d| d.code == "unknown_input"));
    let mut e = eng(spec("l", Condition::Dsl(r#"lua: count("cell phone") >= 4 and count("person") > 0"#.into()), 0));
    assert_eq!(e.observe(&fs("a", 0, Some(60), None)).len(), 1);
    let mut e = eng(spec("l", Condition::Dsl(r#"seen("pistol") or count("person") > 5"#.into()), 0));
    assert_eq!(e.observe(&fs("a", 0, Some(60), None)).len(), 1); // true or unknown = true
}

#[test]
fn from_legacy_maps_hold_frames_explicitly() {
    let r = config::Rule { id: "r1".into(), name: "Phones".into(), camera: "cam1".into(), line: r#"count("cell phone") > 4"#.into(), hold: 5, enabled: true };
    let s = RuleSpec::from_legacy(&r).unwrap();
    assert_eq!(s.hold_ms, 5000);
    assert!(s.migration_warning.as_deref().unwrap().contains("frame count"));
    assert_eq!(s.scope.source_ids, vec!["cam1".to_string()]);
    assert_eq!(s.unknown_policy, UnknownPolicy::Hold);
    assert_eq!(s, RuleSpec::from_legacy(&r).unwrap());
    let all = config::Rule { camera: String::new(), ..r.clone() };
    assert!(RuleSpec::from_legacy(&all).unwrap().scope.source_ids.is_empty());
    assert!(RuleSpec::from_legacy(&config::Rule { line: "garbage".into(), ..r }).is_err());
}

#[test]
fn scope_zone_and_version_reset() {
    let mut r = lua("r", phones_ratio(), 2000);
    r.scope = Scope { source_ids: vec!["a".into()], zone: None };
    let mut e = eng(r.clone());
    assert!(e.observe(&fs("b", 0, Some(60), Some(12))).is_empty());
    assert!(feed(&mut e, 0, 2000, 1000, |_| true).iter().any(|a| a.state == AlertState::Active));
    // new version: active alert resolved explicitly, state reset
    let mut r2 = r.clone();
    r2.version = "2".into();
    let resolved = e.set_rules(vec![r2]);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].state, AlertState::Resolved);
    let a = feed(&mut e, 3000, 5000, 1000, |_| true);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].rule_version, "2");
    let mut z = lua("z", phones_ratio(), 0);
    z.scope.zone = Some("door".into());
    let mut e = eng(z);
    assert!(e.observe(&fs("a", 0, Some(60), Some(12))).is_empty());
    let mut f = fs("a", 1000, Some(60), Some(12));
    f.zone = Some("door".into());
    assert_eq!(e.observe(&f).len(), 1);
}

#[test]
fn low_confidence_summary_is_unknown() {
    let mut r = lua("r", phones_ratio(), 0);
    r.min_confidence = 0.5;
    let mut e = eng(r);
    assert!(e.observe(&fs("a", 0, Some(60), Some(12))).is_empty()); // summary built at 0.4
    assert!(e.diagnostics().iter().any(|d| d.code == "low_confidence"));
}

/// Docs examples are tested: every `<!-- expect: X -->` + ```lua block in docs/RULES.md.
#[test]
fn docs_examples_run() {
    let doc = include_str!("../../../../docs/RULES.md");
    let fixture: FrameSummary = serde_json::from_str(include_str!("../contracts/frame_summary.v1.json")).unwrap();
    let (mut expect, mut n, mut code) = (None::<String>, 0, None::<String>);
    for line in doc.lines() {
        if let Some(c) = code.as_mut() {
            if line.starts_with("```") {
                let want = expect.take().unwrap();
                let mut l = lua::LuaRule::new(c).unwrap_or_else(|e| panic!("example does not compile: {e}\n{c}"));
                let (res, _) = l.eval(&Arc::new(fixture.clone()));
                let got = match res.unwrap_or_else(|e| panic!("example errored: {e}\n{c}")) {
                    Some(true) => "true", Some(false) => "false", None => "unknown",
                };
                assert_eq!(got, want, "example:\n{c}");
                n += 1;
                code = None;
            } else {
                c.push_str(line);
                c.push('\n');
            }
        } else if let Some(x) = line.strip_prefix("<!-- expect: ") {
            expect = Some(x.trim_end_matches("-->").trim().to_string());
        } else if line.starts_with("```lua") && expect.is_some() {
            code = Some(String::new());
        }
    }
    assert!(n >= 5, "found {n} examples");
}

// ---- backtest over a fake store ----

struct Fake(Vec<FrameSummary>);

impl Store for Fake {
    fn upsert_source(&self, _: &Source) -> StoreResult<()> { Ok(()) }
    fn sources(&self) -> StoreResult<Vec<Source>> { Ok(vec![]) }
    fn upsert_media(&self, _: &MediaAsset) -> StoreResult<()> { Ok(()) }
    fn media(&self, _: &str) -> StoreResult<Option<MediaAsset>> { Ok(None) }
    fn media_for_source(&self, _: &str) -> StoreResult<Vec<MediaAsset>> { Ok(vec![]) }
    fn upsert_model(&self, _: &ModelVersion) -> StoreResult<()> { Ok(()) }
    fn models(&self) -> StoreResult<Vec<ModelVersion>> { Ok(vec![]) }
    fn upsert_job(&self, _: &IndexJob) -> StoreResult<()> { Ok(()) }
    fn job(&self, _: &str) -> StoreResult<Option<IndexJob>> { Ok(None) }
    fn jobs(&self) -> StoreResult<Vec<IndexJob>> { Ok(vec![]) }
    fn commit_batch(&self, _: &IndexBatch) -> StoreResult<BatchOutcome> { Ok(BatchOutcome::default()) }
    fn detections(&self, _: &DetectionFilter, _: &Page) -> StoreResult<(Vec<(FrameObservation, Detection)>, Option<String>)> { Ok((vec![], None)) }
    fn count_detections(&self, _: &DetectionFilter) -> StoreResult<u64> { Ok(0) }
    fn observations(&self, _: &str, _: Us, _: Us) -> StoreResult<Vec<(FrameObservation, Vec<Detection>, Vec<Relation>)>> { Ok(vec![]) }
    fn summaries(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<FrameSummary>, Option<String>)> {
        let mut rows: Vec<FrameSummary> = self.0.iter()
            .filter(|s| f.source_ids.is_empty() || f.source_ids.contains(&s.source_id))
            .filter(|s| f.media_ids.is_empty() || f.media_ids.contains(&s.media_id)).cloned().collect();
        rows.sort_by(|a, b| (&a.media_id, a.media_time_us).cmp(&(&b.media_id, b.media_time_us)));
        let start: usize = page.after.as_deref().map(|a| a.parse().unwrap()).unwrap_or(0);
        let end = (start + page.limit as usize).min(rows.len());
        let next = (end < rows.len()).then(|| end.to_string());
        Ok((rows[start..end].to_vec(), next))
    }
    fn relations(&self, _: &DetectionFilter, _: Option<Predicate>, _: &Page) -> StoreResult<(Vec<Relation>, Option<String>)> { Ok((vec![], None)) }
    fn save_query_run(&self, _: &QueryRun, _: &[ReviewMatch]) -> StoreResult<()> { Ok(()) }
    fn query_run(&self, _: &str) -> StoreResult<Option<QueryRun>> { Ok(None) }
    fn matches(&self, _: &str, _: &Page) -> StoreResult<(Vec<ReviewMatch>, Option<String>)> { Ok((vec![], None)) }
    fn save_review(&self, r: &ReviewSelection, _: Option<u64>) -> StoreResult<ReviewSelection> { Ok(r.clone()) }
    fn review(&self, _: &str) -> StoreResult<Option<ReviewSelection>> { Ok(None) }
    fn reviews(&self) -> StoreResult<Vec<ReviewSelection>> { Ok(vec![]) }
    fn upsert_alert(&self, _: &Alert) -> StoreResult<()> { Ok(()) }
    fn alerts(&self, _: &Page) -> StoreResult<(Vec<Alert>, Option<String>)> { Ok((vec![], None)) }
    fn audit(&self, _: &AuditRecord) -> StoreResult<()> { Ok(()) }
    fn audit_log(&self, _: Option<&str>, _: &Page) -> StoreResult<(Vec<AuditRecord>, Option<String>)> { Ok((vec![], None)) }
    fn snapshot(&self) -> StoreResult<u64> { Ok(0) }
}

#[test]
fn backtest_pages_in_media_time_order_and_matches_live() {
    // 2500 frames > one page of 1000. Shuffled input; two windows of "on".
    let mut rows = vec![];
    for i in (0..2500).rev() {
        let t = i * 100; // 10 fps
        let on = (20_000..60_000).contains(&t) || (100_000..130_000).contains(&t);
        rows.push(fs("a", t, Some(60), Some(if on { 12 } else { 1 })));
    }
    for i in 0..60 {
        let mut other = fs("b", i * 100, Some(60), Some(12));
        other.media_id = "m2".into();
        rows.push(other);
    }
    let store = Fake(rows.clone());
    let r = lua("r", phones_ratio(), 5000);
    let scope = QueryScope { source_ids: vec!["a".into()], media_ids: vec![], from_utc_us: None, to_utc_us: None };
    let rep = backtest_report(&store, &r, &scope).unwrap();
    assert_eq!(rep.observations, 2500);
    let rows_len = rows.len();
    assert_eq!(rows_len, 2560);
    assert_eq!(rep.alerts.len(), 2);
    assert!(rep.alerts.iter().all(|a| a.state == AlertState::Resolved && a.end_us.is_some()));
    assert_eq!((rep.alerts[0].start_us, rep.alerts[0].end_us), (20_000_000, Some(59_900_000)));
    assert_eq!(rep.alerts[0].clock, ClockDomain::Media { media_id: "m1".into() });
    assert!(!rep.alerts[0].evidence.is_empty());
    // live replay of the same frames gives the same ids
    let mut live = eng(r.clone());
    rows.sort_by_key(|s| (s.media_id.clone(), s.media_time_us));
    let mut ids = std::collections::BTreeSet::new();
    for s in rows.iter().filter(|s| s.source_id == "a") {
        ids.extend(live.observe(s).into_iter().map(|a| a.id));
    }
    assert_eq!(ids, rep.alerts.iter().map(|a| a.id.clone()).collect());
    // all sources, and an invalid spec
    let all = backtest(&store, &r, &QueryScope { source_ids: vec![], media_ids: vec![], from_utc_us: None, to_utc_us: None }).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(backtest(&store, &lua("bad", "return 5", 0), &scope).unwrap_err().code, "invalid_request");
}
