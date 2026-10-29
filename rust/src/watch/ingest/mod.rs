//! Agent B: deterministic import manifests, v1 batch construction, coverage tracking, lease ownership.
//!
//! Manifests are built on the head only. Workers receive media IDs / segments and never enumerate folders.

use std::path::{Path, PathBuf};

use crate::watch::domain::{
    self, ApiError, Coverage, Detection, FrameObservation, IndexBatch, LeaseRef, Predicate, Quality, Relation, Source, SourceKind,
    TaskLease, Us,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaClass {
    Image,
    Video,
}

const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "bmp", "webp", "tif", "tiff"];
const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mpeg", "ts", "mts", "m2ts", "wmv", "flv"];

/// Classify by extension (lowercased). `None` = not media; the manifest skips it.
pub fn classify(p: &Path) -> Option<MediaClass> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    if IMAGE_EXT.contains(&ext.as_str()) {
        Some(MediaClass::Image)
    } else if VIDEO_EXT.contains(&ext.as_str()) {
        Some(MediaClass::Video)
    } else {
        None
    }
}

/// Media list for a source: deterministic (sorted by path), images and videos only, hidden files skipped.
/// Non-recursive: a folder's direct children. Use [`manifest_opts`] with `recursive = true` to descend.
/// An empty folder is `Ok(vec![])`; a missing path is `not_found`; live/webcam sources have no manifest.
pub fn manifest(source: &Source) -> Result<Vec<PathBuf>, ApiError> {
    manifest_opts(source, false)
}

/// Same as [`manifest`] with an explicit recursion flag. Directory symlinks are not followed; depth is capped at 32.
pub fn manifest_opts(source: &Source, recursive: bool) -> Result<Vec<PathBuf>, ApiError> {
    match source.kind {
        SourceKind::Live | SourceKind::Webcam => {
            return Err(ApiError::new("invalid_request", format!("source {} is a live stream: it has no media manifest", source.id)))
        }
        _ => {}
    }
    let root = PathBuf::from(crate::watch::config::resolve_url(&source.location));
    let meta = std::fs::metadata(&root).map_err(|_| ApiError::new("not_found", format!("path not found: {}", root.display())))?;
    let mut out = Vec::new();
    if meta.is_file() {
        if classify(&root).is_none() {
            return Err(ApiError::new("unsupported", format!("not a recognised image or video file: {}", root.display())));
        }
        out.push(root);
    } else {
        walk(&root, recursive, 0, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, recursive: bool, depth: u32, out: &mut Vec<PathBuf>) -> Result<(), ApiError> {
    let rd = std::fs::read_dir(dir).map_err(|e| ApiError::new("not_found", format!("cannot read {}: {e}", dir.display())))?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if recursive && depth < 32 {
                walk(&p, recursive, depth + 1, out)?;
            }
        } else if ft.is_file() && classify(&p).is_some() {
            out.push(p);
        }
    }
    Ok(())
}

/// Same shape as the catalog's media ID. Used only when the head did not supply one.
pub fn media_id_for(source_id: &str, fingerprint: &str) -> String {
    domain::stable_id("med", &[source_id, fingerprint])
}

/// One detection as the worker sees it. `bbox` is normalized [x0, y0, x1, y1]; `track` is the worker-local tracker number.
#[derive(Clone, Debug)]
pub struct DetIn {
    pub class: String,
    pub confidence: f32,
    pub model_version: String,
    pub bbox: [f32; 4],
    pub track: Option<u64>,
    pub color: Option<String>,
    /// `None` = "model" (the detector saw it on this frame). `Some("tracked:<det_id>")` = carried over from a live
    /// track that this frame's detector passes did not cover, so its absence would be unknown, not zero.
    pub provenance: Option<String>,
}

/// Everything the worker knows for one inferred frame.
pub struct FrameIn<'a> {
    pub source_id: &'a str,
    /// From the lease / catalog when known.
    pub media_id: Option<&'a str>,
    /// Content fingerprint when known. `None` falls back to `path:<resolved url>`, which is weaker:
    /// it changes if the same file is mounted at another path, so observation IDs then differ per mount.
    pub fingerprint: Option<&'a str>,
    pub resolved_url: &'a str,
    pub job_id: &'a str,
    pub media_time_us: Us,
    pub frame_index: Option<u64>,
    pub model_versions: &'a [String],
    pub detections: &'a [DetIn],
    /// (subject index, object index) into `detections`: the subject is near the object by bbox proximity.
    pub near: &'a [(usize, usize)],
    pub thumbnail_hex: Option<String>,
    pub indexed_at_utc_us: Us,
    pub lease: Option<LeaseRef>,
}

/// Build the v1 batch. Pure: identical input gives an identical batch, so a retry cannot raise counts.
pub fn batch_from_frame(f: &FrameIn) -> IndexBatch {
    let fp = f.fingerprint.map(str::to_string).unwrap_or_else(|| format!("path:{}", f.resolved_url));
    let media_id = f.media_id.map(str::to_string).unwrap_or_else(|| media_id_for(f.source_id, &fp));
    let mut versions: Vec<String> = f.model_versions.to_vec();
    versions.sort();
    versions.dedup();
    let obs_id = domain::observation_id(&fp, f.media_time_us, &versions.join("+"));
    let mut ordinal: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut dets: Vec<Detection> = Vec::with_capacity(f.detections.len());
    for d in f.detections {
        let n = ordinal.entry(d.model_version.as_str()).or_insert(0);
        dets.push(Detection {
            detection_id: domain::detection_id(&obs_id, &d.model_version, *n),
            observation_id: obs_id.clone(),
            class: d.class.clone(),
            confidence: d.confidence,
            bbox: d.bbox,
            model_version: d.model_version.clone(),
            track_id: d.track.map(|t| format!("{}/{}/{}/{}", f.source_id, media_id, f.job_id, t)),
            provenance: d.provenance.clone().unwrap_or_else(|| "model".into()),
            color: d.color.clone().filter(|c| !c.is_empty()),
        });
        *n += 1;
    }
    let relations = f
        .near
        .iter()
        .filter(|(s, o)| *s < dets.len() && *o < dets.len() && s != o)
        .map(|&(s, o)| {
            let (sd, od) = (&dets[s], &dets[o]);
            Relation {
                relation_id: domain::stable_id("rel", &[&obs_id, &sd.detection_id, "near", &od.detection_id]),
                observation_id: obs_id.clone(),
                subject_detection_id: sd.detection_id.clone(),
                subject_track_id: sd.track_id.clone(),
                predicate: Predicate::Near,
                object_detection_id: od.detection_id.clone(),
                object_track_id: od.track_id.clone(),
                object_class: od.class.clone(),
                confidence: sd.confidence.min(od.confidence),
                module_version: "bbox-proximity@1".into(),
                method: "bbox-proximity".into(),
            }
        })
        .collect();
    IndexBatch {
        schema_version: domain::SCHEMA_VERSION,
        lease: f.lease.clone(),
        observation: FrameObservation {
            observation_id: obs_id.clone(),
            source_id: f.source_id.into(),
            media_id,
            job_id: f.job_id.into(),
            frame_index: f.frame_index,
            pts: None,
            timebase: None,
            media_time_us: f.media_time_us,
            capture_time_utc_us: None,
            indexed_at_utc_us: f.indexed_at_utc_us,
            model_versions: versions,
            quality: Quality::Ok,
            thumbnail: f.thumbnail_hex.as_ref().map(|_| format!("{obs_id}.jpg")),
        },
        detections: dets,
        relations,
        thumbnail_hex: f.thumbnail_hex.clone(),
    }
}

/// Should an observation at `media_time_us` be indexed under this lease? The overlap before `segment[0]`
/// is decoded to warm trackers only; the previous segment owns it, so nothing is indexed twice.
pub fn owns(lease: &TaskLease, media_time_us: Us) -> bool {
    media_time_us >= lease.segment[0].unwrap_or(0) && lease.segment[1].map_or(true, |end| media_time_us < end)
}

/// Cumulative frame accounting plus merged analyzed intervals.
#[derive(Clone, Debug, Default)]
pub struct CoverageTracker {
    pub cov: Coverage,
}

impl CoverageTracker {
    pub fn decoded(&mut self) {
        self.cov.decoded += 1;
    }
    pub fn skipped(&mut self) {
        self.cov.skipped += 1;
    }
    pub fn failed(&mut self) {
        self.cov.failed += 1;
    }
    /// A frame at `t_us` was inferred and acknowledged; it stands for `span_us` of media (one frame x sampling step).
    /// Intervals touching or overlapping within one span merge; a larger gap stays visible as a gap.
    pub fn inferred(&mut self, t_us: Us, span_us: Us) {
        self.cov.inferred += 1;
        self.extend(t_us, span_us);
    }
    /// A sampled frame the detector did not run on but that is known to hold nothing new: a static scene right after a
    /// pass that saw it, or a tracker-predicted frame between passes. Counts as skipped, extends the analyzed intervals.
    pub fn carried(&mut self, t_us: Us, span_us: Us) {
        self.cov.skipped += 1;
        self.extend(t_us, span_us);
    }
    fn extend(&mut self, t_us: Us, span_us: Us) {
        let end = t_us + span_us.max(1);
        match self.cov.analyzed.last_mut() {
            Some(last) if t_us <= last[1] + span_us.max(1) && t_us >= last[0] => last[1] = last[1].max(end),
            _ => self.cov.analyzed.push([t_us, end]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cw-ingest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn src(kind: SourceKind, loc: &Path) -> Source {
        Source { id: "s".into(), name: "s".into(), kind, location: loc.to_string_lossy().into(), credential_ref: None, eligible_workers: vec![], enabled: true, capture_timezone: None }
    }

    #[test]
    fn manifest_is_sorted_nested_and_deterministic() {
        let d = tmpdir("m");
        std::fs::create_dir_all(d.join("b/deep")).unwrap();
        std::fs::create_dir_all(d.join("a")).unwrap();
        for f in ["z.mp4", "b/two.MOV", "b/deep/x.png", "a/one.jpg", "notes.txt", ".hidden.mp4", "b/skip.doc"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let flat = manifest(&src(SourceKind::Collection, &d)).unwrap();
        assert_eq!(flat, vec![d.join("z.mp4")]);
        let deep = manifest_opts(&src(SourceKind::Collection, &d), true).unwrap();
        let rel: Vec<_> = deep.iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(rel, ["a/one.jpg", "b/deep/x.png", "b/two.MOV", "z.mp4"]);
        assert_eq!(deep, manifest_opts(&src(SourceKind::Collection, &d), true).unwrap());
        assert_eq!(classify(&d.join("b/two.MOV")), Some(MediaClass::Video));
        assert_eq!(classify(&d.join("a/one.jpg")), Some(MediaClass::Image));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn manifest_errors_are_typed() {
        let e = manifest(&src(SourceKind::Archive, Path::new("/definitely/not/here.mp4"))).unwrap_err();
        assert_eq!(e.code, "not_found");
        let e = manifest(&src(SourceKind::Live, Path::new("rtsp://x"))).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        let d = tmpdir("t");
        std::fs::write(d.join("a.txt"), b"x").unwrap();
        assert_eq!(manifest(&src(SourceKind::Archive, &d.join("a.txt"))).unwrap_err().code, "unsupported");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn frame<'a>(dets: &'a [DetIn], versions: &'a [String], near: &'a [(usize, usize)]) -> FrameIn<'a> {
        FrameIn {
            source_id: "cam", media_id: None, fingerprint: None, resolved_url: "/m/a.mp4", job_id: "job1", media_time_us: 1_500_000,
            frame_index: Some(37), model_versions: versions, detections: dets, near, thumbnail_hex: Some("ffd8ffd9".into()),
            indexed_at_utc_us: 42, lease: None,
        }
    }

    #[test]
    fn batch_ids_identical_across_calls_and_scoped() {
        let dets = vec![
            DetIn { class: "person".into(), confidence: 0.9, model_version: "m1".into(), bbox: [0.1, 0.1, 0.5, 0.9], track: Some(7), color: Some("red".into()), provenance: None },
            DetIn { class: "cell phone".into(), confidence: 0.6, model_version: "m1".into(), bbox: [0.2, 0.4, 0.25, 0.5], track: Some(8), color: None, provenance: Some("tracked:det_x".into()) },
        ];
        let versions = vec!["m1".to_string()];
        let a = batch_from_frame(&frame(&dets, &versions, &[(1, 0)]));
        let b = batch_from_frame(&frame(&dets, &versions, &[(1, 0)]));
        assert_eq!(a, b);
        assert!(a.observation.observation_id.starts_with("obs_"));
        assert_eq!(a.detections[0].detection_id, domain::detection_id(&a.observation.observation_id, "m1", 0));
        assert_eq!(a.detections[1].detection_id, domain::detection_id(&a.observation.observation_id, "m1", 1));
        assert_eq!((a.detections[0].provenance.as_str(), a.detections[1].provenance.as_str()), ("model", "tracked:det_x"));
        let tid = a.detections[0].track_id.clone().unwrap();
        assert_eq!(tid, format!("cam/{}/job1/7", a.observation.media_id));
        assert_eq!(a.relations.len(), 1);
        assert_eq!(a.relations[0].method, "bbox-proximity");
        assert_eq!(a.relations[0].predicate, Predicate::Near);
        assert_eq!(a.relations[0].object_class, "person");
        // A different frame time is a different observation.
        let mut f = frame(&dets, &versions, &[]);
        f.media_time_us += 1;
        assert_ne!(batch_from_frame(&f).observation.observation_id, a.observation.observation_id);
        // The batch survives a JSON round trip (what the wire carries).
        let wire: IndexBatch = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(wire, a);
    }

    #[test]
    fn overlap_is_not_owned() {
        let mk = |s: Option<Us>, e: Option<Us>| TaskLease {
            task_id: "t".into(), job_id: "j".into(), media_id: "m".into(), segment: [s, e], worker: "w".into(), generation: 1,
            expires_utc_us: 0, attempt: 1, overlap_us: 2_000_000, settings: serde_json::json!({}),
        };
        let second = mk(Some(10_000_000), Some(20_000_000));
        assert!(!owns(&second, 8_500_000), "overlap read for warm-up is not indexed");
        assert!(owns(&second, 10_000_000));
        assert!(!owns(&second, 20_000_000), "end is exclusive; the next segment owns it");
        assert!(owns(&mk(Some(20_000_000), None), 20_000_000));
        assert!(owns(&mk(None, None), 0));
    }

    #[test]
    fn coverage_merges_adjacent_and_keeps_gaps() {
        let mut c = CoverageTracker::default();
        for t in [0, 100_000, 200_000, 1_000_000] {
            c.inferred(t, 100_000);
        }
        c.decoded();
        c.carried(1_100_000, 100_000);
        assert_eq!((c.cov.inferred, c.cov.skipped), (4, 1));
        assert_eq!(c.cov.analyzed, vec![[0, 300_000], [1_000_000, 1_200_000]]);
        let mut c = CoverageTracker::default();
        for t in [0, 100_000, 200_000, 1_000_000] {
            c.inferred(t, 100_000);
        }
        assert_eq!(c.cov.inferred, 4);
        assert_eq!(c.cov.analyzed, vec![[0, 300_000], [1_000_000, 1_100_000]]);
    }
}
