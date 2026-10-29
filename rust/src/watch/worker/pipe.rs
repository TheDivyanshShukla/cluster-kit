//! Per-camera detection schedule: when the detector runs, on what, and what is just predicted.
//!
//! Modelled on how production systems cut inference cost without losing objects: Frigate runs its detector only
//! where motion was seen (square crops around motion plus around last-known object boxes) and does a periodic
//! full-picture pass; NVIDIA DeepStream runs the detector every N frames and lets a tracker predict in between.
//!
//! Crops only pay off on large pictures: the detector input is fixed (640), so each crop is one full model run. On a
//! source up to ~960 px the whole picture already fits at about native scale, so there motion mode keeps the gating,
//! the interval and the tracker, and runs the whole picture when it runs at all.
//!
//! Index semantics ("unknown is not zero", see domain.rs):
//! * `Full` and `Regions` frames are INDEXED exactly like a classic pass: one observation, its detections.
//! * `Tracked` frames (between passes while objects are followed) are drawn live from the tracker's prediction and are
//!   NOT indexed; coverage counts them analyzed because the passes on both sides saw the same objects.
//! * `Static` frames (no motion, nothing tracked, keyframe not due) are NOT indexed; coverage counts them analyzed
//!   because the last pass saw the scene and nothing has changed since. The first frame and every scene cut are always
//!   full passes, so a static frame never precedes a look at the scene.
//! * A `Regions` pass only looks inside its crops. Every live track's predicted box is part of the crops, so no
//!   tracked object is ever outside them. If one ever is (`Pipe::uncovered`), it is carried into that frame's batch with
//!   provenance `tracked:<det_id>` and its old confidence, so counts stay correct and nothing is invented: the
//!   carried object lives only until its track expires after `max_age` passes.

use std::collections::HashMap;

use crate::watch::config::Process;
use crate::watch::gate::{self, Motion};
use crate::watch::track::Tracker;
use crate::watch::yolo::Det;

/// Detector input side of every shipped model. A source whose long side is not larger than ~1.5x this gains no detail
/// from a crop (the whole picture already fits at about native scale), and a crop costs the same one model run, so
/// motion mode then keeps the gating, interval and tracker but runs the whole picture instead of crops.
const MODEL_SIDE: u32 = 640;
const CROP_MIN_LONG_SIDE: u32 = MODEL_SIDE * 3 / 2;

/// IoU above which two same-class boxes from overlapping crops are one object.
pub const REGION_NMS_IOU: f32 = 0.6;

#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// Whole picture. `keyframe` = scheduled/forced (start, scene cut, `keyframe_secs`); false = classic full mode or
    /// a fallback because the motion crops were too many or too large.
    Full { keyframe: bool },
    /// Square crops in frame pixels [x0, y0, x1, y1].
    Regions(Vec<[f32; 4]>),
    /// Between detector passes: draw the tracker's prediction, index nothing.
    Tracked,
    /// Nothing moved and nothing is followed: no detector, nothing to draw.
    Static,
}

impl Plan {
    pub fn runs_detector(&self) -> bool {
        matches!(self, Plan::Full { .. } | Plan::Regions(_))
    }
}

/// What a track looked like at its last detector pass (for carry-over and the next batch).
#[derive(Clone, Debug)]
pub struct Last {
    pub class: String,
    pub bbox: [f32; 4],
    pub conf: f32,
    pub color: String,
    pub det_id: String,
    pub model: String,
    pub model_version: String,
}

pub struct Pipe {
    pub tracker: Tracker,
    last_key: Option<f64>,
    since_pass: u32,
    pub last: HashMap<u64, Last>,
    /// The boxes drawn at the last detector pass (JSON, normalised, with `track_id`): the base the tracker's
    /// prediction is drawn from on `Tracked` frames.
    pub shown: Vec<serde_json::Value>,
}

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

impl Pipe {
    pub fn new(scope: &str) -> Self {
        Pipe { tracker: Tracker::scoped(scope), last_key: None, since_pass: 0, last: HashMap::new(), shown: Vec::new() }
    }

    /// Live tracks' predicted boxes that still intersect the picture, normalised.
    fn live_boxes(&self) -> Vec<[f32; 4]> {
        self.tracker
            .live()
            .into_iter()
            .map(|(_, _, b, _)| [b[0].max(0.0), b[1].max(0.0), b[2].min(1.0), b[3].min(1.0)])
            .filter(|b| b[2] > b[0] && b[3] > b[1])
            .collect()
    }

    /// Decide what to do with a sampled frame at media time `at` (seconds). Pure: state moves in `commit`, which the
    /// caller runs once the frame was actually handled (a failed detector leaves the schedule untouched).
    pub fn plan(&self, p: &Process, at: f64, m: &Motion, fw: u32, fh: u32) -> Plan {
        if p.detect_mode != "motion" {
            return Plan::Full { keyframe: false };
        }
        let key_due = self.last_key.map_or(true, |t| at < t || at - t >= p.keyframe_secs as f64);
        if m.cut || key_due {
            return Plan::Full { keyframe: true };
        }
        let live = self.live_boxes();
        if m.boxes.is_empty() && live.is_empty() {
            return Plan::Static;
        }
        let new_motion = m.boxes.iter().any(|b| !live.iter().any(|l| overlaps(*b, *l)));
        if !live.is_empty() && !new_motion && self.since_pass + 1 < p.detect_interval {
            return Plan::Tracked;
        }
        if fw.max(fh) <= CROP_MIN_LONG_SIDE {
            return Plan::Full { keyframe: false };
        }
        let (w, h) = (fw as f32, fh as f32);
        let px: Vec<[f32; 4]> = m.boxes.iter().chain(live.iter()).map(|b| [b[0] * w, b[1] * h, b[2] * w, b[3] * h]).collect();
        match gate::regions_for(&px, p.region_margin, w, h, p.max_regions as usize) {
            Some(r) => Plan::Regions(r),
            None => Plan::Full { keyframe: false },
        }
    }

    /// Boxes to draw on a `Tracked` frame: the last pass's boxes moved along each live track's velocity.
    /// `interval` = sampled frames per pass, so velocity (per pass) is scaled by frames-since-pass / interval.
    pub fn predicted_view(&self, interval: u32) -> Vec<serde_json::Value> {
        let k = self.since_pass as f32 / interval.max(1) as f32;
        let live = self.tracker.live();
        self.shown
            .iter()
            .filter_map(|b| {
                let tid = b["track_id"].as_u64()?;
                if !live.iter().any(|t| t.0 == tid && t.3 == 0) {
                    return None;
                }
                let v = self.tracker.velocity(tid)?;
                let mut b = b.clone();
                let (x, y) = (b["x"].as_f64()? as f32, b["y"].as_f64()? as f32);
                let (w, h) = (b["w"].as_f64()? as f32, b["h"].as_f64()? as f32);
                b["x"] = serde_json::json!((x + v[0] * k).clamp(0.0, (1.0 - w).max(0.0)));
                b["y"] = serde_json::json!((y + v[1] * k).clamp(0.0, (1.0 - h).max(0.0)));
                b["predicted"] = serde_json::json!(true);
                Some(b)
            })
            .collect()
    }

    pub fn commit(&mut self, plan: &Plan, at: f64) {
        match plan {
            Plan::Full { keyframe } => {
                if *keyframe {
                    self.last_key = Some(at);
                }
                self.since_pass = 0;
            }
            Plan::Regions(_) => self.since_pass = 0,
            Plan::Tracked => self.since_pass += 1,
            Plan::Static => {}
        }
    }

    /// Live tracks whose predicted box centre is outside every crop of a `Regions` pass.
    pub fn uncovered(&self, regions: &[[f32; 4]], fw: u32, fh: u32) -> Vec<(u64, Last)> {
        let (w, h) = (fw as f32, fh as f32);
        self.tracker
            .live()
            .into_iter()
            .filter_map(|(n, _, b, _)| {
                let (cx, cy) = ((b[0] + b[2]) * 0.5 * w, (b[1] + b[3]) * 0.5 * h);
                let inside = regions.iter().any(|r| cx >= r[0] && cx <= r[2] && cy >= r[1] && cy <= r[3]);
                if inside { None } else { self.last.get(&n).map(|l| (n, l.clone())) }
            })
            .collect()
    }

    /// Keep only what live tracks may still need.
    pub fn prune(&mut self) {
        let live: Vec<u64> = self.tracker.live().into_iter().map(|t| t.0).collect();
        self.last.retain(|n, _| live.contains(n));
    }
}

/// Detector cost of one frame: model runs and source pixels shown to the detector.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub runs: u32,
    pub pixels: u64,
}

/// Run `detect` on the frame the plan asks for and return boxes in FRAME pixels. Crops come from the full-resolution
/// frame; overlapping crops can see the same object twice, so results go through class-aware NMS.
pub fn run_plan<F>(plan: &Plan, rgb: &[u8], w: u32, h: u32, mut detect: F) -> Result<(Vec<Det>, Cost), String>
where
    F: FnMut(&[u8], u32, u32) -> Result<Vec<Det>, String>,
{
    match plan {
        Plan::Full { .. } => Ok((detect(rgb, w, h)?, Cost { runs: 1, pixels: w as u64 * h as u64 })),
        Plan::Regions(rs) => {
            let mut all = Vec::new();
            let mut cost = Cost::default();
            for r in rs {
                let c = gate::crop_rgb(rgb, w, h, *r);
                for mut d in detect(&c.rgb, c.w, c.h)? {
                    d.xyxy = c.to_frame(d.xyxy);
                    all.push(d);
                }
                cost.runs += 1;
                cost.pixels += c.w as u64 * c.h as u64;
            }
            Ok((gate::nms(all, REGION_NMS_IOU), cost))
        }
        Plan::Tracked | Plan::Static => Ok((Vec::new(), Cost::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::config::preset;

    fn proc() -> Process {
        Process { keyframe_secs: 2.0, detect_interval: 2, ..preset("balanced") }
    }
    fn still() -> Motion {
        Motion::default()
    }
    fn moving(b: [f32; 4]) -> Motion {
        Motion { boxes: vec![b], cut: false }
    }
    const PERSON: [f32; 4] = [0.40, 0.30, 0.50, 0.70];

    fn step(p: &mut Pipe, pr: &Process, at: f64, m: &Motion) -> Plan {
        let plan = p.plan(pr, at, m, 1280, 720);
        p.commit(&plan, at);
        plan
    }

    #[test]
    fn full_mode_always_runs_the_whole_picture() {
        let pr = Process { detect_mode: "full".into(), ..proc() };
        let mut p = Pipe::new("c");
        for t in 0..5 {
            assert_eq!(step(&mut p, &pr, t as f64 * 0.1, &still()), Plan::Full { keyframe: false });
        }
    }

    #[test]
    fn start_cut_and_keyframe_period_force_a_full_pass_and_a_still_scene_is_static() {
        let pr = proc();
        let mut p = Pipe::new("c");
        assert_eq!(step(&mut p, &pr, 0.0, &still()), Plan::Full { keyframe: true });
        for t in 1..20 {
            assert_eq!(step(&mut p, &pr, t as f64 * 0.1, &still()), Plan::Static, "t={t}");
        }
        assert_eq!(step(&mut p, &pr, 2.0, &still()), Plan::Full { keyframe: true }, "keyframe_secs elapsed");
        assert_eq!(step(&mut p, &pr, 2.1, &still()), Plan::Static);
        assert_eq!(step(&mut p, &pr, 2.2, &Motion { boxes: vec![], cut: true }), Plan::Full { keyframe: true }, "scene cut");
        assert_eq!(step(&mut p, &pr, 0.5, &still()), Plan::Full { keyframe: true }, "media time went backwards (loop)");
    }

    #[test]
    fn motion_gets_a_crop_and_a_followed_object_is_predicted_between_passes() {
        let pr = proc();
        let mut p = Pipe::new("c");
        step(&mut p, &pr, 0.0, &still());
        let Plan::Regions(r) = step(&mut p, &pr, 0.1, &moving(PERSON)) else { panic!("expected a region pass") };
        assert_eq!(r.len(), 1);
        let (cx, cy) = ((PERSON[0] + PERSON[2]) / 2.0 * 1280.0, (PERSON[1] + PERSON[3]) / 2.0 * 720.0);
        assert!(r[0][0] <= cx && cx <= r[0][2] && r[0][1] <= cy && cy <= r[0][3]);
        // the detector found the person -> it is followed
        p.tracker.update(&[("person".into(), PERSON)]);
        // detect_interval 2: next sampled frame is predicted only, the one after runs the detector again
        assert_eq!(step(&mut p, &pr, 0.2, &moving(PERSON)), Plan::Tracked);
        assert!(matches!(step(&mut p, &pr, 0.3, &moving(PERSON)), Plan::Regions(_)));
        // motion somewhere ELSE cannot wait for the interval
        p.tracker.update(&[("person".into(), PERSON)]);
        assert_eq!(step(&mut p, &pr, 0.4, &moving(PERSON)), Plan::Tracked);
        let Plan::Regions(two) = step(&mut p, &pr, 0.5, &Motion { boxes: vec![PERSON, [0.85, 0.1, 0.95, 0.3]], cut: false }) else { panic!() };
        assert_eq!(two.len(), 2, "tracked person + new mover");
    }

    #[test]
    fn a_still_but_tracked_object_is_kept_alive_until_its_track_expires() {
        let pr = Process { detect_interval: 1, ..proc() };
        let mut p = Pipe::new("c");
        step(&mut p, &pr, 0.0, &still());
        p.tracker.update(&[("person".into(), PERSON)]);
        let mut passes = 0;
        for i in 1..=8 {
            let plan = step(&mut p, &pr, i as f64 * 0.1, &still());
            if matches!(plan, Plan::Regions(_)) {
                passes += 1;
                p.tracker.update(&[]); // detector no longer sees it
            }
        }
        assert!(passes >= 5 && passes <= 6, "followed for max_age passes, then Static: {passes}");
        assert_eq!(step(&mut p, &pr, 1.0, &still()), Plan::Static);
    }

    #[test]
    fn tracked_frames_draw_the_prediction_not_the_old_box() {
        let pr = proc();
        let mut p = Pipe::new("c");
        step(&mut p, &pr, 0.0, &still());
        p.tracker.update(&[("car".into(), [0.10, 0.4, 0.20, 0.5])]);
        let ids = p.tracker.update(&[("car".into(), [0.14, 0.4, 0.24, 0.5])]);
        p.shown = vec![serde_json::json!({"class": "car", "track_id": ids[0], "x": 0.14, "y": 0.4, "w": 0.1, "h": 0.1})];
        // the keyframe was the pass (since_pass 0); this sampled frame is half way to the next one
        assert_eq!(step(&mut p, &pr, 0.1, &moving([0.14, 0.4, 0.24, 0.5])), Plan::Tracked);
        let v = p.predicted_view(2);
        assert_eq!(v.len(), 1);
        assert!((v[0]["x"].as_f64().unwrap() - 0.16).abs() < 1e-4, "half a pass of 0.04 velocity: {}", v[0]);
        assert_eq!(v[0]["predicted"], true);
    }

    #[test]
    fn small_sources_get_the_whole_picture_not_a_crop() {
        let pr = proc();
        let mut p = Pipe::new("c");
        step(&mut p, &pr, 0.0, &still());
        let plan = p.plan(&pr, 0.1, &moving(PERSON), 768, 432);
        assert_eq!(plan, Plan::Full { keyframe: false }, "a crop of a 768 px picture adds no detail and costs the same run");
        assert!(matches!(p.plan(&pr, 0.1, &moving(PERSON), 1920, 1080), Plan::Regions(_)));
        assert_eq!(p.plan(&pr, 0.1, &still(), 768, 432), Plan::Static, "gating still applies");
    }

    #[test]
    fn too_many_crops_fall_back_to_one_full_pass() {
        let pr = Process { max_regions: 2, ..proc() };
        let mut p = Pipe::new("c");
        step(&mut p, &pr, 0.0, &still());
        let m = Motion { boxes: vec![[0.0, 0.0, 0.05, 0.1], [0.4, 0.0, 0.45, 0.1], [0.9, 0.8, 0.95, 0.9]], cut: false };
        assert_eq!(step(&mut p, &pr, 0.1, &m), Plan::Full { keyframe: false });
    }

    #[test]
    fn run_plan_maps_crops_back_and_dedups_overlaps() {
        let (w, h) = (200u32, 100u32);
        let rgb = vec![7u8; (w * h * 3) as usize];
        // two overlapping 60x60 crops; each "sees" the same object at frame (50,20)-(70,50)
        let regions = vec![[30.0, 10.0, 90.0, 70.0], [40.0, 15.0, 100.0, 75.0]];
        let mut seen = Vec::new();
        let (dets, cost) = run_plan(&Plan::Regions(regions), &rgb, w, h, |_, cw, ch| {
            let n = seen.len();
            seen.push((cw, ch));
            let (ox, oy) = if n == 0 { (30.0, 10.0) } else { (40.0, 15.0) };
            Ok(vec![Det { xyxy: [50.0 - ox, 20.0 - oy, 70.0 - ox, 50.0 - oy], class: "person".into(), conf: 0.5 + n as f32 * 0.2 }])
        })
        .unwrap();
        assert_eq!(seen, vec![(60, 60), (60, 60)]);
        assert_eq!(dets.len(), 1, "same object in two crops -> one box");
        assert_eq!(dets[0].xyxy, [50.0, 20.0, 70.0, 50.0]);
        assert_eq!(dets[0].conf, 0.7);
        assert_eq!(cost, Cost { runs: 2, pixels: 7200 });
        let (_, full) = run_plan(&Plan::Full { keyframe: true }, &rgb, w, h, |_, _, _| Ok(vec![])).unwrap();
        assert_eq!(full, Cost { runs: 1, pixels: 20_000 });
        assert!(run_plan(&Plan::Static, &rgb, w, h, |_, _, _| panic!("no detector on a static frame")).unwrap().0.is_empty());
    }

    #[test]
    fn a_track_outside_every_crop_is_carried_not_dropped() {
        let mut p = Pipe::new("c");
        let ids = p.tracker.update(&[("person".into(), [0.1, 0.2, 0.2, 0.6]), ("car".into(), [0.7, 0.5, 0.9, 0.8])]);
        p.last.insert(ids[0], Last { class: "person".into(), bbox: [0.1, 0.2, 0.2, 0.6], conf: 0.8, color: "red".into(), det_id: "det_a".into(), model: "m".into(), model_version: "mv".into() });
        p.last.insert(ids[1], Last { class: "car".into(), bbox: [0.7, 0.5, 0.9, 0.8], conf: 0.7, color: "gray".into(), det_id: "det_b".into(), model: "m".into(), model_version: "mv".into() });
        // a crop over the person only
        let cut = p.uncovered(&[[50.0, 100.0, 350.0, 400.0]], 1280, 720);
        assert_eq!(cut.len(), 1);
        assert_eq!((cut[0].1.class.as_str(), cut[0].1.det_id.as_str(), cut[0].1.conf), ("car", "det_b", 0.7));
        // crops built by the planner always cover live tracks, so nothing is carried in practice
        let pr = proc();
        step(&mut p, &pr, 0.0, &still());
        if let Plan::Regions(r) = p.plan(&pr, 0.1, &moving([0.4, 0.4, 0.45, 0.5]), 1280, 720) {
            assert!(p.uncovered(&r, 1280, 720).is_empty());
        }
        // an expired track is forgotten
        for _ in 0..7 { p.tracker.update(&[]); }
        p.prune();
        assert!(p.last.is_empty());
    }
}

/// Real-media comparison of `detect_mode` full vs motion on `vision/footage/*.mp4` against the 30 hand-labelled
/// frames in `vision/eval/labels/sample30.json`. Needs ffmpeg, the ONNX model and a release build:
///   CW_BENCH_OUT=/tmp/bench.md cargo test --release ... pipeline_bench -- --ignored --nocapture
#[cfg(test)]
mod bench {
    use super::*;
    use crate::watch::config::{preset, Process};
    use crate::watch::decode::Feed;
    use crate::watch::gate::{MotionModel, MotionParams};
    use crate::watch::yolo::Engine;
    use std::collections::BTreeMap;
    use std::time::Instant;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Kind {
        Pass,
        Tracked,
        Static,
    }

    struct Rec {
        t_us: i64,
        kind: Kind,
        counts: BTreeMap<String, u32>,
    }

    #[derive(Default)]
    struct Run {
        sampled: u64,
        passes: u64,
        keyframes: u64,
        region_frames: u64,
        fallbacks: u64,
        tracked: u64,
        statics: u64,
        model_runs: u64,
        px_inferred: u64,
        px_full: u64,
        wall: f64,
        detect: f64,
        recs: Vec<Rec>,
    }

    fn probe_fps(path: &str) -> f64 {
        crate::watch::decode::probe(path).fps
    }

    fn run(root: &std::path::Path, engine: &mut Engine, labels: &[String], file: &str, p: &Process, upscale: bool) -> Run {
        let url = root.join("vision/footage").join(file).to_string_lossy().into_owned();
        let fps = probe_fps(&url);
        let src_w = 768; // both clips; decoding wider only upscales
        let mut feed = Feed::open(&url, "archive", 0.0, if upscale { p.decode_width() } else { p.decode_width().min(src_w) }).expect("ffmpeg");
        let mut model = MotionModel::default();
        let mp = MotionParams::new(p.motion_sensitivity, p.motion_min_area);
        let mut pipe = Pipe::new("bench");
        let mut r = Run::default();
        let started = Instant::now();
        let mut idx = 0u64;
        while let Ok((w, h, rgb)) = feed.next_frame() {
            let at = idx as f64 / fps;
            let motion = (p.detect_mode == "motion").then(|| model.update(&gate::shrink_gray_box(&rgb, w, h, 256), &mp)).unwrap_or_default();
            let sampled = p.every <= 1 || idx % p.every as u64 == 0;
            idx += 1;
            if !sampled {
                continue;
            }
            r.sampled += 1;
            r.px_full += w as u64 * h as u64;
            let t_us = (at * 1e6).round() as i64;
            let plan = pipe.plan(p, at, &motion, w, h);
            pipe.commit(&plan, at);
            match &plan {
                Plan::Static => {
                    r.statics += 1;
                    r.recs.push(Rec { t_us, kind: Kind::Static, counts: BTreeMap::new() });
                }
                Plan::Tracked => {
                    r.tracked += 1;
                    let mut counts = BTreeMap::new();
                    for b in pipe.predicted_view(p.detect_interval) {
                        *counts.entry(b["class"].as_str().unwrap_or("").to_string()).or_insert(0) += 1;
                    }
                    r.recs.push(Rec { t_us, kind: Kind::Tracked, counts });
                }
                _ => {
                    let t = Instant::now();
                    let (dets, cost) = run_plan(&plan, &rgb, w, h, |c, cw, ch| engine.run(c, cw, ch, 0.4, labels, labels)).expect("detector");
                    r.detect += t.elapsed().as_secs_f64();
                    r.passes += 1;
                    r.model_runs += cost.runs as u64;
                    r.px_inferred += cost.pixels;
                    match &plan {
                        Plan::Full { keyframe: true } => r.keyframes += 1,
                        Plan::Full { .. } if p.detect_mode == "motion" => r.fallbacks += 1,
                        Plan::Regions(_) => r.region_frames += 1,
                        _ => {}
                    }
                    let (fw, fh) = (w as f32, h as f32);
                    let geometry: Vec<(String, [f32; 4])> = dets.iter().map(|d| (d.class.clone(), [d.xyxy[0] / fw, d.xyxy[1] / fh, d.xyxy[2] / fw, d.xyxy[3] / fh])).collect();
                    let ids = pipe.tracker.update(&geometry);
                    pipe.shown = geometry
                        .iter()
                        .zip(&ids)
                        .map(|((c, b), id)| serde_json::json!({"class": c, "track_id": id, "x": b[0], "y": b[1], "w": b[2] - b[0], "h": b[3] - b[1]}))
                        .collect();
                    pipe.prune();
                    let mut counts = BTreeMap::new();
                    for d in &dets {
                        *counts.entry(d.class.clone()).or_insert(0) += 1;
                    }
                    r.recs.push(Rec { t_us, kind: Kind::Pass, counts });
                }
            }
        }
        r.wall = started.elapsed().as_secs_f64();
        r
    }

    /// (recall, extra, exact frames, frames) over the labelled frames of `media`, using records within 100 ms.
    fn score(labels: &serde_json::Value, media: &str, recs: &[Rec], passes_only: bool, debug: bool) -> (u32, u32, u32, u32, u32, u32) {
        let (mut tp, mut truth, mut extra, mut exact, mut frames, mut unknown) = (0, 0, 0, 0, 0, 0);
        for f in labels["frames"].as_array().unwrap() {
            if f["media"].as_str() != Some(media) {
                continue;
            }
            frames += 1;
            let t = f["media_time_us"].as_i64().unwrap();
            let near = recs.iter().filter(|r| (r.t_us - t).abs() <= 100_000 && (!passes_only || r.kind == Kind::Pass)).min_by_key(|r| (r.t_us - t).abs());
            let mut ok = near.is_some();
            if debug {
                eprintln!("  t={:.2}s truth={} near={:?}", t as f64 / 1e6, f["labels"], near.map(|r| (r.kind, (r.t_us - t) / 1000, r.counts.clone())));
            }
            if near.is_none() {
                unknown += 1;
            }
            for (class, n) in f["labels"].as_object().unwrap() {
                let n = n.as_u64().unwrap() as u32;
                let got = near.map_or(0, |r| r.counts.get(class).copied().unwrap_or(0));
                truth += n;
                tp += got.min(n);
                extra += got.saturating_sub(n);
                if got != n {
                    ok = false;
                }
            }
            exact += ok as u32;
        }
        (tp, truth, extra, exact, frames, unknown)
    }

    #[test]
    #[ignore]
    fn pipeline_bench() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut engine = Engine::load(&root.join("vision/models/yolov5n.onnx"), crate::watch::detector_threads()).expect("model");
        let labels: Vec<String> = ["person", "bicycle", "car", "motorcycle", "bus", "truck", "backpack", "handbag", "suitcase", "cell phone"].iter().map(|s| s.to_string()).collect();
        let truth: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("vision/eval/labels/sample30.json")).unwrap()).unwrap();
        let full = |every: u32, width: u32| Process { detect_mode: "full".into(), every, width, ..preset("balanced") };
        let only = std::env::var("CW_BENCH_ONLY").unwrap_or_default();
        let debug = std::env::var("CW_BENCH_DEBUG").is_ok();
        let configs: Vec<(&str, Process, bool)> = vec![
            ("full / every 2 / 512 px (old balanced)", full(2, 512), false),
            ("full / every 1 / 640 px (old accuracy)", full(1, 640), false),
            ("motion / balanced preset", preset("balanced"), false),
            ("motion / accuracy preset", preset("accuracy"), false),
            ("motion / speed preset", preset("speed"), false),
            ("full / every 2 / 1280 px", full(2, 1280), true),
        ];
        let mut out = String::from("| file | config | sampled | detector frames | model runs | static | tracked | keyframes | crop frames | full fallbacks | px inferred / px full | wall s | detector s | recall (indexed only) | recall (+tracked/static) | extra | exact frames | unknown |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
        // CW_BENCH_WIDE=<dir>: the same two clips shrunk into a 1280x720 canvas (small objects on a wide camera).
        let mut files: Vec<(String, String)> = ["people-detection.mp4", "person-bicycle-car-detection.mp4"].iter().map(|f| (f.to_string(), f.to_string())).collect();
        if let Ok(dir) = std::env::var("CW_BENCH_WIDE") {
            for f in ["people-detection.mp4", "person-bicycle-car-detection.mp4"] {
                files.push((format!("{dir}/{f}"), f.to_string()));
            }
        }
        for (file, media) in files.iter().map(|(a, b)| (a.as_str(), b.as_str())) {
            for (name, p, up) in &configs {
                if !only.is_empty() && !only.split(',').any(|o| format!("{file} {name}").contains(o)) {
                    continue;
                }
                let r = run(&root, &mut engine, &labels, file, p, *up || file.starts_with('/'));
                if debug {
                    eprintln!("{file} {name}");
                }
                let a = score(&truth, media, &r.recs, true, false);
                let b = score(&truth, media, &r.recs, false, debug);
                let pct = |x: u32, y: u32| if y == 0 { "n/a".into() } else { format!("{}/{} = {:.0} %", x, y, 100.0 * x as f64 / y as f64) };
                let file = if file.starts_with('/') { format!("WIDE {media}") } else { file.to_string() };
                out += &format!(
                    "| {file} | {name} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} % | {:.1} | {:.1} | {} ({} frames unknown) | {} | {} | {}/{} | {} |\n",
                    r.sampled, r.passes, r.model_runs, r.statics, r.tracked, r.keyframes, r.region_frames, r.fallbacks,
                    100.0 * r.px_inferred as f64 / r.px_full.max(1) as f64, r.wall, r.detect,
                    pct(a.0, a.1), a.5, pct(b.0, b.1), b.2, b.3, b.4, b.5
                );
                eprintln!("{file} {name}: done");
            }
        }
        println!("{out}");
        if let Ok(path) = std::env::var("CW_BENCH_OUT") {
            std::fs::write(path, out).unwrap();
        }
    }
}
