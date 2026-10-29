//! Agent C: model adapter registry, manifest validation, fusion, relation adapters.
//!
//! A module's `labels` in modules.json is ambiguous: for the stock COCO model it is an
//! *interest filter* (10 of 80 classes), for a custom model it must be the *full ordered class list*.
//! `validate` separates the two: the ModelVersion always carries the full ordered list taken from
//! the model's output width; the filter stays in the module and is applied by the caller.

pub mod labels;

use crate::watch::config::Module;
use crate::watch::domain::*;
use crate::watch::{repo_root, yolo};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const PROXIMITY_ID: &str = "mv_proximity";

/// Execution provider actually registered with ONNX Runtime. This build links ORT with no
/// accelerator feature (see Cargo.toml `ort` features), so every session runs on CPU.
/// If a provider is ever registered, change this one function; nothing else claims a provider.
#[allow(unexpected_cfgs)]
pub fn effective_provider() -> &'static str {
    if cfg!(feature = "coreml") { "coreml" } else { "cpu" }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new("invalid_request", msg)
}

pub fn resolve_path(model: &str) -> PathBuf {
    let p = Path::new(model);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let a = repo_root().join(p);
    if a.exists() {
        return a;
    }
    // `cargo test` runs from rust/; the repo root is one up.
    let b = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(p);
    if b.exists() { b } else { a }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Decide layout and the ordered label list from the output width and the module's label list.
/// Returns (output_schema, full ordered labels).
pub fn resolve_layout(cols: usize, module_labels: &[String]) -> Result<(String, Vec<String>), ApiError> {
    let given: Vec<String> = module_labels.iter().filter(|s| !s.is_empty()).cloned().collect();
    // Candidate class counts: yolov5-style has objectness (cols-5), yolov8-style does not (cols-4).
    let v5 = cols.checked_sub(5).filter(|n| *n >= 1);
    let v8 = cols.checked_sub(4).filter(|n| *n >= 1);
    let pick = |n: usize| -> Option<String> {
        if v5 == Some(n) {
            Some("yolov5".to_string())
        } else if v8 == Some(n) {
            Some(if n == 1 { "yolov8-single" } else { "yolov8" }.to_string())
        } else {
            None
        }
    };
    // Full ordered list that fits the output width: a custom (or fully listed COCO) model.
    if !given.is_empty() {
        if let Some(schema) = pick(given.len()) {
            if given.len() == 80 && given != labels::coco80() {
                return Err(bad("80 labels given but they are not the canonical ordered COCO-80 list; class order must match the model"));
            }
            return Ok((schema, given));
        }
    }
    // Stock COCO output: module labels (if any) are only an interest filter and must be COCO names.
    let coco_subset = given.iter().all(|g| labels::COCO.contains(&g.as_str()));
    if let Some(schema) = pick(80) {
        if coco_subset {
            return Ok((schema, labels::coco80()));
        }
    }
    if given.is_empty() {
        return Err(bad(format!(
            "output width {cols} is not COCO-80 (85 or 84); a custom model needs its full ordered label list in the manifest"
        )));
    }
    Err(bad(format!(
        "label count {} does not match model output width {cols} (expected {} labels for yolov5-style or {} for yolov8-style); \
         a custom model must list every class in output order",
        given.len(),
        v5.map(|n| n.to_string()).unwrap_or("?".into()),
        v8.map(|n| n.to_string()).unwrap_or("?".into()),
    )))
}

/// Open the module's artifact, run it once, and describe what it really is.
pub fn validate(module: &Module) -> Result<ModelVersion, ApiError> {
    match module.kind.as_str() {
        "fight" => {
            return Ok(ModelVersion {
                id: format!("mv_{}", module.id),
                version: "heuristic-1".into(),
                artifact_hash: "none".into(),
                adapter: AdapterKind::Fight,
                output_schema: "fight-heuristic".into(),
                labels: vec!["fight".into()],
                supported_relations: vec![],
                thresholds: BTreeMap::new(),
                provider: effective_provider().into(),
                input_size: None,
                validation: "unvalidated".into(),
            })
        }
        "object" | "weapon" => {}
        other => return Err(ApiError::new("unsupported", format!("module kind '{other}' has no adapter"))),
    }
    if module.model.trim().is_empty() {
        let why = if module.kind == "weapon" {
            "weapon module has no model artifact; its classes can only come from a model, so weapon detection is unsupported"
        } else {
            "module has no model artifact"
        };
        return Err(ApiError::new("unsupported", why));
    }
    let path = resolve_path(&module.model);
    if !path.is_file() {
        return Err(ApiError::new("not_found", format!("model file not found: {}", module.model)));
    }
    let hash = sha256_file(&path).map_err(|e| ApiError::new("internal", format!("cannot read {}: {e}", module.model)))?;
    let mut engine = yolo::Engine::load(&path, 1).map_err(|e| bad(format!("not a loadable ONNX detector ({}): {e}", module.model)))?;
    let (_rows, cols) = engine.probe().map_err(|e| bad(format!("model failed to run: {e}")))?;
    let (schema, full) = match engine.layout() {
        // End-to-end / DETR heads: class count is not in the width, so COCO-80 unless a full custom list is given.
        l @ ("e2e" | "detr") => {
            let given: Vec<String> = module.labels.iter().filter(|s| !s.is_empty()).cloned().collect();
            let coco_filter = given.iter().all(|g| labels::COCO.contains(&g.as_str()));
            if (l == "detr" && (cols == 80 || cols == 91) && coco_filter) || (l == "e2e" && coco_filter) {
                (l.to_string(), labels::coco80())
            } else if l == "detr" && given.len() == cols {
                (l.to_string(), given)
            } else if l == "e2e" && !given.is_empty() {
                (l.to_string(), given)
            } else {
                return Err(bad(format!("{l} model with {cols} classes needs its full ordered label list in the manifest")));
            }
        }
        _ => resolve_layout(cols, &module.labels)?,
    };
    let validation = if repo_root().join("vision/eval/results").join(format!("{}.json", module.id)).is_file() {
        format!("evaluated:vision/eval/results/{}.json", module.id)
    } else {
        "unvalidated".into()
    };
    let mut thresholds = BTreeMap::new();
    thresholds.insert("*".to_string(), module.conf);
    Ok(ModelVersion {
        id: format!("mv_{}_{}", module.id, &hash[..8]),
        version: format!("sha256:{}", &hash[..16]),
        artifact_hash: hash,
        adapter: AdapterKind::Object,
        output_schema: schema,
        labels: full,
        supported_relations: vec![],
        thresholds,
        provider: effective_provider().into(),
        input_size: Some(engine.input_size() as u32),
        validation,
    })
}

/// Pixel `yolo::Det` -> domain `Detection`, normalized to the analyzed frame.
/// `tracks` (same order) are scoped string track ids from `Tracker::update_scoped`.
pub fn to_detections(obs_id: &str, mv: &ModelVersion, dets: &[yolo::Det], w: u32, h: u32, tracks: Option<&[String]>) -> Vec<Detection> {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    dets.iter()
        .enumerate()
        .map(|(i, d)| Detection {
            detection_id: detection_id(obs_id, &mv.id, i),
            observation_id: obs_id.to_string(),
            class: d.class.clone(),
            confidence: d.conf,
            bbox: [d.xyxy[0] / w, d.xyxy[1] / h, d.xyxy[2] / w, d.xyxy[3] / h],
            model_version: mv.id.clone(),
            track_id: tracks.and_then(|t| t.get(i).cloned()),
            provenance: "model".into(),
            color: None,
        })
        .collect()
}

fn fused_ids(d: &Detection) -> BTreeSet<String> {
    match d.provenance.strip_prefix("fused:") {
        Some(rest) => rest.split(',').map(|s| s.to_string()).collect(),
        None => std::iter::once(d.model_version.clone()).collect(),
    }
}

/// Class-aware cross-module dedup. A box from module B is merged into a kept box from module A
/// when classes match and IoU >= `iou`; the higher-confidence one survives, provenance becomes
/// `fused:<a>,<b>`. Boxes from the same module are never fused (NMS already ran per module).
pub fn fuse(dets: Vec<Detection>, iou: f32) -> Vec<Detection> {
    let mut order: Vec<usize> = (0..dets.len()).collect();
    order.sort_by(|a, b| dets[*b].confidence.total_cmp(&dets[*a].confidence).then(a.cmp(b)));
    let mut kept: Vec<(usize, Detection, BTreeSet<String>)> = Vec::new();
    for i in order {
        let d = &dets[i];
        let m = kept.iter_mut().find(|(_, k, ids)| {
            k.class == d.class && !ids.contains(&d.model_version) && crate::watch::track::iou(k.bbox, d.bbox) >= iou
        });
        match m {
            Some((_, k, ids)) => {
                ids.insert(d.model_version.clone());
                k.provenance = format!("fused:{}", ids.iter().cloned().collect::<Vec<_>>().join(","));
                if k.track_id.is_none() {
                    k.track_id = d.track_id.clone();
                }
            }
            None => {
                let mut d = d.clone();
                let ids = fused_ids(&d);
                if ids.len() > 1 {
                    d.provenance = format!("fused:{}", ids.iter().cloned().collect::<Vec<_>>().join(","));
                }
                kept.push((i, d, ids));
            }
        }
    }
    kept.sort_by_key(|(i, _, _)| *i);
    kept.into_iter().map(|(_, d, _)| d).collect()
}

fn centre_in(obj: [f32; 4], p: [f32; 4]) -> bool {
    let (x, y) = ((obj[0] + obj[2]) / 2.0, (obj[1] + obj[3]) / 2.0);
    x >= p[0] && x <= p[2] && y >= p[1] && y <= p[3]
}

/// `near`: an object whose centre lies inside a person box, attached to the smallest such person.
/// Approximate spatial heuristic; it does not establish holding. `holding` is never produced here:
/// it needs a Relation-adapter model that can be evaluated, and none is installed (see `support`).
pub fn relate(obs_id: &str, dets: &[Detection], installed: &[ModelVersion]) -> Vec<Relation> {
    let _ = installed;
    let area = |b: [f32; 4]| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let persons: Vec<&Detection> = dets.iter().filter(|d| d.class == "person").collect();
    let mut out = Vec::new();
    for o in dets.iter().filter(|d| d.class != "person") {
        let best = persons
            .iter()
            .filter(|p| centre_in(o.bbox, p.bbox))
            .min_by(|a, b| area(a.bbox).total_cmp(&area(b.bbox)).then(a.detection_id.cmp(&b.detection_id)));
        if let Some(p) = best {
            out.push(Relation {
                relation_id: stable_id("rel", &[obs_id, &p.detection_id, &o.detection_id, "near"]),
                observation_id: obs_id.to_string(),
                subject_detection_id: p.detection_id.clone(),
                subject_track_id: p.track_id.clone(),
                predicate: Predicate::Near,
                object_detection_id: o.detection_id.clone(),
                object_track_id: o.track_id.clone(),
                object_class: o.class.clone(),
                confidence: p.confidence.min(o.confidence),
                module_version: PROXIMITY_ID.into(),
                method: "bbox-proximity".into(),
            });
        }
    }
    out
}

/// Can the installed modules answer this predicate at all?
pub fn support(installed: &[ModelVersion], predicate: Predicate) -> Support {
    match predicate {
        Predicate::Near => Support::Supported, // built-in bbox-proximity, approximate
        Predicate::Holding => {
            if installed.iter().any(|m| m.adapter == AdapterKind::Relation && m.supported_relations.contains(&Predicate::Holding)) {
                Support::Supported
            } else {
                Support::Unsupported
            }
        }
    }
}

/// Distinct counts for one frame. Detections should already be fused. Distinctness is by track id
/// when present (one track seen twice in a frame is one object), else by detection id.
/// Classes not in `analyzed_classes` are absent, which readers treat as Unknown (not zero).
pub fn summarize(obs: &FrameObservation, dets: &[Detection], rels: &[Relation], analyzed_classes: &[String], min_conf: f32) -> FrameSummary {
    let mut class_counts = BTreeMap::new();
    for c in analyzed_classes {
        let n: BTreeSet<&str> = dets
            .iter()
            .filter(|d| &d.class == c && d.confidence >= min_conf)
            .map(|d| d.track_id.as_deref().unwrap_or(d.detection_id.as_str()))
            .collect();
        class_counts.insert(c.clone(), Count::Known(n.len() as u32));
    }
    let mut subjects: HashMap<String, BTreeSet<String>> = HashMap::new();
    for r in rels.iter().filter(|r| r.confidence >= min_conf) {
        subjects
            .entry(relation_key(r.predicate, &r.object_class))
            .or_default()
            .insert(r.subject_track_id.clone().unwrap_or_else(|| r.subject_detection_id.clone()));
    }
    let mut relation_counts = BTreeMap::new();
    // `near` is computable for every analyzed object class when persons are analyzed: 0 is a real zero.
    if analyzed_classes.iter().any(|c| c == "person") {
        for c in analyzed_classes.iter().filter(|c| *c != "person") {
            relation_counts.insert(relation_key(Predicate::Near, c), Count::Known(0));
        }
    }
    for (k, v) in subjects {
        relation_counts.insert(k, Count::Known(v.len() as u32));
    }
    FrameSummary {
        observation_id: obs.observation_id.clone(),
        source_id: obs.source_id.clone(),
        media_id: obs.media_id.clone(),
        media_time_us: obs.media_time_us,
        capture_time_utc_us: obs.capture_time_utc_us,
        zone: None,
        min_confidence: min_conf,
        class_counts,
        relation_counts,
        quality: obs.quality,
    }
}

#[cfg(test)]
mod tests;
