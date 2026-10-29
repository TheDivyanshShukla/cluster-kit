//! Agent D: matched observations -> ReviewMatch windows.
//!
//! Rules (docs/VISION_IMPLEMENTATION_PLAN.md section 6):
//! - Group by (source, media, clock domain). Different clock domains never merge.
//! - Within a group sort by media time and merge observations whose gap to the previous
//!   matched observation is `<= gap_us`.
//! - `[start_us, end_us]` is exact matched evidence: first matched observation time, and
//!   last matched observation time + `step_us` (one sample step; the default assumes 0.5 s
//!   sampling) clamped to the media duration. The context interval adds `pre_us`/`post_us`
//!   and is clamped to `[0, duration]` when the duration is known. It always contains the window.
//! - Several detections in the same frame produce one window. Re-feeding the same rows
//!   (a retried batch) is deduplicated by detection id and cannot create more windows.
//! - Still images produce one match per observation with `still = true` and `start == end`.

#![allow(dead_code)]

use crate::watch::domain::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Debug)]
pub struct Opts {
    pub gap_us: Us,
    pub pre_us: Us,
    pub post_us: Us,
    /// One sample step added after the last matched observation.
    pub step_us: Us,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { gap_us: 2_000_000, pre_us: 3_000_000, post_us: 3_000_000, step_us: 500_000 }
    }
}

/// One matched piece of evidence. Detection rows, relation rows and frame rows all reduce to this.
#[derive(Clone, Debug)]
pub struct Point {
    pub source_id: String,
    pub media_id: String,
    pub observation_id: String,
    pub media_time_us: Us,
    pub detection_id: Option<String>,
    pub relation_id: Option<String>,
    pub track_id: Option<String>,
    pub confidence: f32,
}

pub fn windows(
    rows: &[(FrameObservation, Detection)],
    media: &HashMap<String, MediaAsset>,
    opts: &Opts,
    query_run_id: &str,
) -> Vec<ReviewMatch> {
    let pts: Vec<Point> = rows
        .iter()
        .map(|(o, d)| Point {
            source_id: o.source_id.clone(),
            media_id: o.media_id.clone(),
            observation_id: o.observation_id.clone(),
            media_time_us: o.media_time_us,
            detection_id: Some(d.detection_id.clone()),
            relation_id: None,
            track_id: d.track_id.clone(),
            confidence: d.confidence,
        })
        .collect();
    windows_from_points(&pts, media, opts, query_run_id)
}

pub fn windows_from_points(
    points: &[Point],
    media: &HashMap<String, MediaAsset>,
    opts: &Opts,
    run: &str,
) -> Vec<ReviewMatch> {
    let clock_of = |media_id: &str| match media.get(media_id) {
        Some(m) => m.clock_domain(),
        None => ClockDomain::Media { media_id: media_id.to_string() },
    };
    // Dedup retried rows: same observation + detection + relation is one piece of evidence.
    let mut seen = BTreeSet::new();
    let mut groups: BTreeMap<(String, String, ClockDomain), Vec<&Point>> = BTreeMap::new();
    for p in points {
        if seen.insert((&p.observation_id, &p.detection_id, &p.relation_id)) {
            groups.entry((p.source_id.clone(), p.media_id.clone(), clock_of(&p.media_id))).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    for ((source_id, media_id, clock), mut pts) in groups {
        pts.sort_by(|a, b| (a.media_time_us, &a.observation_id).cmp(&(b.media_time_us, &b.observation_id)));
        let asset = media.get(&media_id);
        let still = asset.is_some_and(|m| m.still);
        let dur = asset.and_then(|m| m.duration_us);
        // Split into runs of points that belong to one window.
        let mut runs: Vec<Vec<&Point>> = Vec::new();
        for p in pts {
            let join = match runs.last() {
                Some(r) if still => r.last().is_some_and(|l| l.observation_id == p.observation_id),
                Some(r) => p.media_time_us - r.last().unwrap().media_time_us <= opts.gap_us,
                None => false,
            };
            if join {
                runs.last_mut().unwrap().push(p);
            } else {
                runs.push(vec![p]);
            }
        }
        for r in runs {
            let start = r[0].media_time_us;
            let last = r.last().unwrap().media_time_us;
            let mut end = if still { start } else { last + opts.step_us };
            if let Some(d) = dur {
                end = end.min(d);
            }
            end = end.max(start);
            let (mut cs, mut ce) = ((start - opts.pre_us).max(0).min(start), end + opts.post_us);
            if still {
                (cs, ce) = (start, end);
            } else if let Some(d) = dur {
                ce = ce.min(d).max(end);
            }
            let mut obs = Vec::new();
            let mut dets = Vec::new();
            let mut rels = Vec::new();
            let mut tracks = BTreeSet::new();
            let mut maxc = 0f32;
            for p in &r {
                if !obs.contains(&p.observation_id) {
                    obs.push(p.observation_id.clone());
                }
                if let Some(d) = &p.detection_id {
                    if !dets.contains(d) {
                        dets.push(d.clone());
                    }
                }
                if let Some(x) = &p.relation_id {
                    if !rels.contains(x) {
                        rels.push(x.clone());
                    }
                }
                if let Some(t) = &p.track_id {
                    tracks.insert(t.clone());
                }
                maxc = maxc.max(p.confidence);
            }
            out.push(ReviewMatch {
                id: stable_id("match", &[run, &media_id, &start.to_string()]),
                query_run_id: run.to_string(),
                source_id: source_id.clone(),
                media_id: media_id.clone(),
                clock: clock.clone(),
                start_us: start,
                end_us: end,
                context_start_us: cs,
                context_end_us: ce,
                capture_start_utc_us: asset.and_then(|m| m.capture_start_utc_us),
                thumbnail: Some(format!("/api/v1/media/{media_id}/frame?t_us={start}")),
                observation_ids: obs,
                detection_ids: dets,
                relation_ids: rels,
                distinct_tracks: tracks.len() as u32,
                max_confidence: maxc,
                verification: Verification::Unreviewed,
                still,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn asset(id: &str, dur: Option<Us>, utc: Option<Us>, still: bool) -> MediaAsset {
        MediaAsset {
            id: id.into(), source_id: "s".into(), uri: "x".into(), fingerprint: "f".into(), duration_us: dur,
            width: None, height: None, codec: None, timebase: None, capture_start_utc_us: utc,
            proxy: ProxyStatus::None, availability: Availability::Available, still,
        }
    }
    fn row(media: &str, t: Us, det: &str, track: Option<&str>, conf: f32) -> (FrameObservation, Detection) {
        let oid = format!("obs_{media}_{t}");
        (
            FrameObservation {
                observation_id: oid.clone(), source_id: "s".into(), media_id: media.into(), job_id: "j".into(),
                frame_index: None, pts: None, timebase: None, media_time_us: t, capture_time_utc_us: None,
                indexed_at_utc_us: 0, model_versions: vec![], quality: Quality::Ok, thumbnail: None,
            },
            Detection {
                detection_id: format!("det_{det}"), observation_id: oid, class: "person".into(), confidence: conf,
                bbox: [0.0; 4], model_version: "m".into(), track_id: track.map(Into::into),
                provenance: "model".into(), color: None,
            },
        )
    }
    fn mm(v: Vec<MediaAsset>) -> HashMap<String, MediaAsset> {
        v.into_iter().map(|m| (m.id.clone(), m)).collect()
    }
    const S: Us = 1_000_000;

    #[test]
    fn gap_merges_and_splits() {
        let rows = vec![row("a", 10 * S, "1", None, 0.5), row("a", 11 * S, "2", None, 0.9), row("a", 13 * S, "3", None, 0.6), row("a", 20 * S, "4", None, 0.7)];
        let m = windows(&rows, &mm(vec![asset("a", Some(60 * S), None, false)]), &Opts::default(), "qr");
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].start_us, m[0].end_us), (10 * S, 13 * S + 500_000));
        assert_eq!(m[0].observation_ids.len(), 3);
        assert_eq!(m[0].max_confidence, 0.9);
        assert_eq!(m[1].start_us, 20 * S);
        // A tighter gap splits the first window too.
        let o = Opts { gap_us: S / 2, ..Opts::default() };
        assert_eq!(windows(&rows, &mm(vec![asset("a", Some(60 * S), None, false)]), &o, "qr").len(), 4);
    }

    #[test]
    fn same_frame_and_retry_make_one_window() {
        let (o, d1) = row("a", 5 * S, "1", Some("t1"), 0.8);
        let mut d2 = d1.clone();
        d2.detection_id = "det_2".into();
        d2.track_id = Some("t2".into());
        let rows = vec![(o.clone(), d1.clone()), (o.clone(), d2.clone()), (o.clone(), d1), (o, d2)];
        let m = windows(&rows, &mm(vec![asset("a", None, None, false)]), &Opts::default(), "qr");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].detection_ids.len(), 2);
        assert_eq!(m[0].distinct_tracks, 2);
    }

    #[test]
    fn same_track_repeated_is_one_track() {
        let rows: Vec<_> = (0..5).map(|i| row("a", i * S, &i.to_string(), Some("t1"), 0.8)).collect();
        let m = windows(&rows, &mm(vec![]), &Opts::default(), "qr");
        assert_eq!((m.len(), m[0].distinct_tracks), (1, 1));
    }

    #[test]
    fn mixed_clocks_never_merge_and_context_clamps() {
        let rows = vec![row("a", S, "1", None, 0.5), row("b", S, "2", None, 0.5)];
        let media = mm(vec![asset("a", Some(2 * S), Some(1_700_000_000 * S), false), asset("b", Some(2 * S), None, false)]);
        let m = windows(&rows, &media, &Opts::default(), "qr");
        assert_eq!(m.len(), 2);
        assert!(m.iter().any(|x| x.clock == ClockDomain::Utc) && m.iter().any(|x| x.clock == ClockDomain::Media { media_id: "b".into() }));
        for x in &m {
            assert_eq!((x.context_start_us, x.context_end_us), (0, 2 * S));
            assert!(x.context_start_us <= x.start_us && x.end_us <= x.context_end_us);
        }
    }

    #[test]
    fn stills_one_match_per_observation() {
        let rows = vec![row("img", 0, "1", None, 0.5), row("img", 0, "2", None, 0.6)];
        let mut rows2 = rows.clone();
        rows2.push(row("img", 0, "3", None, 0.9));
        let m = windows(&rows2, &mm(vec![asset("img", None, None, true)]), &Opts::default(), "qr");
        assert_eq!(m.len(), 1); // all three share observation obs_img_0
        assert!(m[0].still && m[0].start_us == m[0].end_us && m[0].detection_ids.len() == 3);
        let mut r2 = vec![row("img", 0, "1", None, 0.5)];
        let mut other = row("img", 0, "9", None, 0.5);
        other.0.observation_id = "obs_other".into();
        other.1.observation_id = "obs_other".into();
        r2.push(other);
        assert_eq!(windows(&r2, &mm(vec![asset("img", None, None, true)]), &Opts::default(), "qr").len(), 2);
    }

    #[test]
    fn ids_are_stable() {
        let rows = vec![row("a", S, "1", None, 0.5)];
        let a = windows(&rows, &mm(vec![]), &Opts::default(), "qr");
        let b = windows(&rows, &mm(vec![]), &Opts::default(), "qr");
        assert_eq!(a[0].id, b[0].id);
        assert_ne!(a[0].id, windows(&rows, &mm(vec![]), &Opts::default(), "qr2")[0].id);
    }
}
