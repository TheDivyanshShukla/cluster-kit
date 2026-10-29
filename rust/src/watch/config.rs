use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::watch::repo_root;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub model: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default = "default_conf")]
    pub conf: f32,
    #[serde(default = "default_confirm")]
    pub confirm: u32,
    #[serde(default)]
    pub gflops: f32,
    #[serde(default)]
    pub enabled: bool,
}

fn default_conf() -> f32 {
    0.4
}
fn default_confirm() -> u32 {
    3
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub camera: String,
    pub line: String,
    #[serde(default = "default_hold")]
    pub hold: u32,
    #[serde(default)]
    pub enabled: bool,
}

fn default_hold() -> u32 {
    5
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Zone {
    pub name: String,
    pub rect: [f32; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub start: f64,
    pub kind: String,
    #[serde(default)]
    pub zones: Vec<Zone>,
    /// `false`: do not run the detector on this source. For recordings that are already fully indexed and are
    /// only replayed on the wall, which draws the stored detections. Default `true`.
    #[serde(default = "default_true")]
    pub analyze: bool,
}

/// How a PC spends its cores. A mode is a starting point. Each switch can be changed on its own.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Process {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_true")]
    pub blur_on: bool,
    #[serde(default = "default_blur")]
    pub blur: f32,
    #[serde(default = "default_true")]
    pub blank_on: bool,
    #[serde(default = "default_true")]
    pub same_on: bool,
    #[serde(default = "default_same")]
    pub same: f32,
    #[serde(default = "default_every")]
    pub every: u32,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_rate")]
    pub rate: u32,
    /// "motion" (default): the detector only sees what changed, the way Frigate gates detection on motion and
    /// DeepStream runs it every N frames with a tracker in between. "full": every sampled frame, whole picture (old behaviour).
    #[serde(default = "default_detect_mode")]
    pub detect_mode: String,
    /// 0..1. Higher notices smaller brightness changes (pixel-difference threshold 40 grey levels at 0, 8 at 1).
    #[serde(default = "default_motion_sensitivity")]
    pub motion_sensitivity: f32,
    /// Smallest moving blob that counts, as a fraction of the picture area (0.001 = 0.1 %). Filters sensor noise.
    #[serde(default = "default_motion_min_area")]
    pub motion_min_area: f32,
    /// Extra room around a moving thing when its detector crop is cut, as a fraction of the object size on each side.
    #[serde(default = "default_region_margin")]
    pub region_margin: f32,
    /// While objects are tracked, the detector runs on every Nth sampled frame; the tracker predicts the frames between
    /// (shown live, never indexed). New motion outside tracked boxes triggers the detector at once. 1 = every sampled frame.
    #[serde(default = "default_detect_interval")]
    pub detect_interval: u32,
    /// Seconds between full-picture passes (also forced at start and after a scene cut). Catches anything motion missed.
    #[serde(default = "default_keyframe_secs")]
    pub keyframe_secs: f32,
    /// More separate motion crops than this on one frame: run one full-picture pass instead.
    #[serde(default = "default_max_regions")]
    pub max_regions: u32,
    /// Archives longer than this many seconds are split into segments that different workers take at the same time. 0 = never split.
    #[serde(default = "default_segment_secs")]
    pub segment_secs: u32,
}

impl Process {
    /// Width FFmpeg decodes to. Motion mode cuts crops out of this picture, so it never drops below 960 px.
    pub fn decode_width(&self) -> u32 {
        if self.detect_mode == "motion" { self.width.max(960) } else { self.width }
    }
}

fn default_detect_mode() -> String {
    "motion".into()
}
fn default_motion_sensitivity() -> f32 {
    0.6
}
fn default_motion_min_area() -> f32 {
    0.001
}
fn default_region_margin() -> f32 {
    0.25
}
fn default_detect_interval() -> u32 {
    2
}
fn default_keyframe_secs() -> f32 {
    2.0
}
fn default_max_regions() -> u32 {
    4
}
fn default_segment_secs() -> u32 {
    120
}

fn default_mode() -> String {
    "balanced".into()
}
fn default_true() -> bool {
    true
}
fn default_blur() -> f32 {
    40.0
}
fn default_same() -> f32 {
    3.5
}
fn default_every() -> u32 {
    2
}
fn default_width() -> u32 {
    512
}
fn default_rate() -> u32 {
    60
}

#[derive(Clone, Debug)]
pub struct WatchCfg {
    pub blur: f32,
    pub copies: usize,
    pub process: Process,
    pub cameras: Vec<Camera>,
}

pub fn modules_path() -> PathBuf {
    repo_root().join("vision/modules.json")
}
pub fn rules_path() -> PathBuf {
    repo_root().join("vision/rules.json")
}
pub fn watch_path() -> PathBuf {
    repo_root().join("vision/watch.json")
}

pub fn load_modules() -> Vec<Module> {
    read_json(&modules_path()).unwrap_or_default()
}

pub fn load_rules() -> Vec<Rule> {
    read_json(&rules_path()).unwrap_or_default()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    // Write beside the destination so rename stays on the same filesystem.
    // Unique temporary names also prevent concurrent requests clobbering each other.
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all((text + "\n").as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result.map_err(|e| e.to_string())
}

pub fn preset(mode: &str) -> Process {
    match mode {
        "speed" => Process {
            mode: "speed".into(),
            blur_on: true,
            blur: 70.0,
            blank_on: true,
            same_on: true,
            same: 6.0,
            every: 3,
            width: 960,
            rate: 60,
            detect_mode: "motion".into(),
            motion_sensitivity: 0.5,
            motion_min_area: 0.002,
            region_margin: 0.2,
            detect_interval: 3,
            keyframe_secs: 3.0,
            max_regions: 3,
            segment_secs: default_segment_secs(),
        },
        "accuracy" => Process {
            mode: "accuracy".into(),
            blur_on: true,
            blur: 18.0,
            blank_on: true,
            same_on: true,
            same: 1.2,
            every: 1,
            width: 1280,
            rate: 60,
            detect_mode: "motion".into(),
            motion_sensitivity: 0.75,
            motion_min_area: 0.0005,
            region_margin: 0.35,
            detect_interval: 1,
            keyframe_secs: 1.0,
            max_regions: 6,
            segment_secs: default_segment_secs(),
        },
        _ => Process {
            mode: "balanced".into(),
            blur_on: true,
            blur: 40.0,
            blank_on: true,
            same_on: true,
            same: 3.5,
            every: 2,
            width: 1280,
            rate: 60,
            detect_mode: default_detect_mode(),
            motion_sensitivity: default_motion_sensitivity(),
            motion_min_area: default_motion_min_area(),
            region_margin: default_region_margin(),
            detect_interval: default_detect_interval(),
            keyframe_secs: default_keyframe_secs(),
            max_regions: default_max_regions(),
            segment_secs: default_segment_secs(),
        },
    }
}

pub fn clamp_process(mut p: Process) -> Process {
    if !matches!(p.mode.as_str(), "speed" | "balanced" | "accuracy" | "custom") {
        p.mode = "balanced".into();
    }
    p.blur = p.blur.clamp(1.0, 400.0);
    p.same = p.same.clamp(0.2, 40.0);
    p.every = p.every.clamp(1, 4);
    // Preview frame rate for the wall's live-preview mode. Low values spare the Mac when many cameras run.
    p.rate = if p.rate >= 45 { 60 } else if p.rate >= 22 { 30 } else if p.rate >= 12 { 15 } else { 10 };
    p.width = match p.width {
        w if w <= 368 => 320,
        w if w <= 464 => 416,
        w if w <= 576 => 512,
        w if w <= 800 => 640,
        w if w <= 1100 => 960,
        _ => 1280,
    };
    if !matches!(p.detect_mode.as_str(), "motion" | "full") {
        p.detect_mode = default_detect_mode();
    }
    p.motion_sensitivity = if p.motion_sensitivity.is_finite() { p.motion_sensitivity.clamp(0.0, 1.0) } else { default_motion_sensitivity() };
    p.motion_min_area = if p.motion_min_area.is_finite() { p.motion_min_area.clamp(0.0001, 0.2) } else { default_motion_min_area() };
    p.region_margin = if p.region_margin.is_finite() { p.region_margin.clamp(0.0, 1.0) } else { default_region_margin() };
    p.detect_interval = p.detect_interval.clamp(1, 10);
    p.keyframe_secs = if p.keyframe_secs.is_finite() { p.keyframe_secs.clamp(0.5, 30.0) } else { default_keyframe_secs() };
    p.max_regions = p.max_regions.clamp(1, 8);
    p.segment_secs = if p.segment_secs == 0 { 0 } else { p.segment_secs.clamp(5, 3600) };
    p
}

pub fn process_line(p: &Process) -> String {
    let mode = match p.mode.as_str() {
        "speed" => "Speed",
        "accuracy" => "Accuracy",
        "custom" => "Custom",
        _ => "Balanced",
    };
    let blur = if p.blur_on { format!("blur filter at {}", p.blur.round() as i32) } else { "blur filter off".into() };
    let blank = if p.blank_on { "blank pictures skipped" } else { "blank pictures still looked at" };
    let same = if p.same_on { "unchanged pictures skipped" } else { "still pictures still looked at" };
    let every = if p.every <= 1 { "every sharp frame".into() } else { format!("every {} sharp frames", p.every) };
    let detect = if p.detect_mode == "motion" {
        format!("Detector on motion only (sensitivity {:.2}), every {} pass{} while tracking, full picture every {} s.", p.motion_sensitivity, p.detect_interval, if p.detect_interval == 1 { "" } else { "es" }, p.keyframe_secs)
    } else {
        "Detector on every sampled frame, whole picture.".into()
    };
    format!("{mode}. {} px. Picture {} fps. {blur}. {blank}. {same}. {every}. {detect}", p.decode_width(), p.rate)
}

pub fn load_watch() -> WatchCfg {
    let mut copies = 8usize;
    let mut process = preset("balanced");
    let mut cameras: Vec<Value> = Vec::new();
    let mut configured = false;
    if let Ok(text) = fs::read_to_string(watch_path()) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            configured = v.get("configured").and_then(Value::as_bool).unwrap_or(false);
            if let Some(n) = v.get("copies").and_then(|x| x.as_u64()) {
                copies = n.max(1) as usize;
            }
            if let Some(p) = v.get("process") {
                if let Ok(parsed) = serde_json::from_value::<Process>(p.clone()) {
                    process = parsed;
                }
            } else if let Some(n) = v.get("blur").and_then(|x| x.as_f64()) {
                process.blur = n as f32;
            }
            if let Some(arr) = v.get("cameras").and_then(|x| x.as_array()) {
                cameras = arr.clone();
            }
        }
    }
    let process = clamp_process(process);
    WatchCfg {
        blur: process.blur,
        copies,
        process,
        cameras: if configured && cameras.is_empty() { Vec::new() } else { expand(&cameras, copies) },
    }
}

pub fn save_process(process: &Process, copies: usize) -> Result<(), String> {
    let process = clamp_process(process.clone());
    let path = watch_path();
    let mut v = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("process".into(), serde_json::to_value(&process).map_err(|e| e.to_string())?);
        obj.insert("blur".into(), serde_json::json!(process.blur));
        obj.insert("expect_pcs".into(), serde_json::json!(0));
        obj.insert("copies".into(), serde_json::json!(copies));
        if !obj.contains_key("cameras") {
            obj.insert("cameras".into(), serde_json::json!([]));
        }
    }
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(path, text + "\n").map_err(|e| e.to_string())
}

fn expand(raw: &[Value], copies: usize) -> Vec<Camera> {
    let mut out = Vec::new();
    for (i, c) in raw.iter().enumerate() {
        let Some(url) = c.get("url").and_then(|u| u.as_str()) else { continue };
        if url.is_empty() {
            continue;
        }
        let n = i + 1;
        // A folder becomes independently schedulable still-image sources, with
        // deterministic IDs so repeated imports address the same images.
        let resolved = resolve_url(url);
        if Path::new(&resolved).is_dir() {
            let mut files: Vec<_> = fs::read_dir(&resolved).ok().into_iter().flatten().filter_map(Result::ok)
                .map(|e| e.path()).filter(|p| p.is_file() && is_image(p)).collect();
            files.sort();
            for file in files {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                file.to_string_lossy().hash(&mut hash);
                out.push(Camera {
                    id: format!("{}-{:016x}", c.get("id").and_then(|v| v.as_str()).unwrap_or("images"), hash.finish()),
                    name: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                    url: file.to_string_lossy().into_owned(), start: 0.0, kind: "image".into(), zones: Vec::new(), analyze: true,
                });
            }
            continue;
        }
        out.push(Camera {
            id: c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            url: url.to_string(),
            start: c.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0),
            kind: c.get("kind").and_then(|v| v.as_str()).filter(|k| matches!(*k, "archive" | "image" | "file" | "live" | "webcam")).map(str::to_string).unwrap_or_else(|| kind_of(url)),
            zones: c.get("zones").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
            analyze: c.get("analyze").and_then(|v| v.as_bool()).unwrap_or(true),
        });
        let cam = out.last_mut().unwrap();
        if cam.id.is_empty() {
            cam.id = format!("c{n}");
        }
        if cam.name.is_empty() {
            cam.name = format!("Camera {n}");
        }
    }
    if !out.is_empty() {
        return out;
    }
    let root = repo_root();
    let footage = root.join("vision/footage");
    let mut files: Vec<PathBuf> = fs::read_dir(&footage)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("mp4"))
        .collect();
    files.sort();
    if files.is_empty() {
        return out;
    }
    for i in 0..copies {
        let f = &files[i % files.len()];
        let rel = f.strip_prefix(&root).unwrap_or(f);
        out.push(Camera {
            id: format!("c{}", i + 1),
            name: format!("Camera {}", i + 1),
            url: rel.to_string_lossy().replace('\\', "/"),
            start: ((i as f64) * 0.13) % 0.85,
            kind: "file".into(),
            zones: Vec::new(),
            analyze: true,
        });
    }
    out
}

fn is_image(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp"))
}

pub fn kind_of(url: &str) -> String {
    if is_image(Path::new(url)) { return "image".into(); }
    if url.chars().all(|c| c.is_ascii_digit()) {
        "webcam".into()
    } else if url.contains("://") {
        "live".into()
    } else {
        "file".into()
    }
}

pub fn resolve_url(url: &str) -> String {
    if url.chars().all(|c| c.is_ascii_digit()) || url.contains("://") {
        return url.to_string();
    }
    let p = Path::new(url);
    if p.is_absolute() {
        url.to_string()
    } else {
        repo_root().join(p).to_string_lossy().into_owned()
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub words: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub camera: String,
    #[serde(default)]
    pub from: f64,
    #[serde(default)]
    pub to: f64,
    #[serde(default)]
    pub lua: String,
}

pub fn searches_path() -> PathBuf {
    repo_root().join("vision/searches.json")
}

pub fn load_searches() -> Vec<SavedSearch> {
    read_json(&searches_path()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_archive_kind_survives_source_expansion() {
        let cams = expand(&[serde_json::json!({"id":"recording", "url":"recording.mp4", "kind":"archive"})], 1);
        assert_eq!(cams[0].kind, "archive");
        assert_eq!(kind_of("image.JPG"), "image");
    }

    #[test]
    fn image_folders_expand_deterministically() {
        let dir = std::env::temp_dir().join(format!("cw-sources-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("b.jpg"), []).unwrap();
        fs::write(dir.join("a.PNG"), []).unwrap();
        fs::write(dir.join("other.txt"), []).unwrap();
        let raw = [serde_json::json!({"id":"folder", "url":dir.to_string_lossy()})];
        let first = expand(&raw, 1);
        let second = expand(&raw, 1);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].kind, "image");
        assert_eq!(first[0].id, second[0].id);
        assert_eq!(first[0].name, "a.PNG");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn speed_skips_more_than_accuracy() {
        let speed = preset("speed");
        let accuracy = preset("accuracy");
        assert!(speed.every > accuracy.every);
        assert!(speed.width < accuracy.width);
        assert!(speed.same > accuracy.same);
        assert!(speed.blur > accuracy.blur);
        let odd = clamp_process(Process { width: 333, every: 9, same: 0.0, blur: 0.0, mode: "nope".into(), ..speed });
        assert_eq!(odd.width, 320);
        assert_eq!(odd.every, 4);
        assert_eq!(odd.mode, "balanced");
    }

    #[test]
    fn old_settings_load_with_motion_defaults_and_clamp() {
        let p: Process = serde_json::from_str(r#"{"mode":"custom","every":2,"width":512}"#).unwrap();
        assert_eq!(p.detect_mode, "motion");
        assert_eq!((p.detect_interval, p.max_regions), (2, 4));
        assert_eq!(p.decode_width(), 960, "motion crops need more than 512 px");
        let wild = clamp_process(Process { detect_mode: "x".into(), motion_sensitivity: 9.0, motion_min_area: -1.0, region_margin: f32::NAN, detect_interval: 0, keyframe_secs: 0.0, max_regions: 99, width: 5000, ..p });
        assert_eq!((wild.detect_mode.as_str(), wild.motion_sensitivity, wild.detect_interval, wild.max_regions, wild.width), ("motion", 1.0, 1, 8, 1280));
        assert!(wild.motion_min_area > 0.0 && wild.region_margin == 0.25 && wild.keyframe_secs == 0.5);
        let full = Process { detect_mode: "full".into(), width: 416, ..preset("balanced") };
        assert_eq!(full.decode_width(), 416);
    }
}
