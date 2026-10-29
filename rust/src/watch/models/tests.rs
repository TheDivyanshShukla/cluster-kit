use super::*;
use crate::watch::config::Module;

pub fn repo(p: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(p)
}

/// Decode one RGB frame at `t` seconds with ffmpeg, scaled to w x h.
pub fn ffmpeg_frame(video: &str, t: f32, w: u32, h: u32) -> Vec<u8> {
    let out = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
        .args(["-v", "error", "-ss", &t.to_string(), "-i"])
        .arg(repo(video))
        .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("ffmpeg");
    assert_eq!(out.stdout.len(), (w * h * 3) as usize, "no frame at {t}s in {video}");
    out.stdout
}

fn module(id: &str, kind: &str, model: &str, labels: &[&str]) -> Module {
    Module {
        id: id.into(), name: id.into(), kind: kind.into(), model: model.into(),
        labels: labels.iter().map(|s| s.to_string()).collect(), conf: 0.4, confirm: 3, gflops: 0.0, enabled: true,
    }
}

fn stock() -> Module {
    let all: Vec<Module> = serde_json::from_str(&std::fs::read_to_string(repo("vision/modules.json")).unwrap()).unwrap();
    // The shipped default model changes over time; these tests pin the model they assert on.
    let mut m = all.into_iter().find(|m| m.id == "objects").unwrap();
    m.model = "vision/models/yolov5n.onnx".into();
    m
}

#[test]
fn real_onnx_inference_finds_a_person_in_real_footage() {
    let mut e = yolo::Engine::load(&repo("vision/models/yolov5n.onnx"), 2).unwrap();
    assert_eq!(e.input_size(), 640);
    let interest = vec!["person".to_string()];
    let mut best = 0.0f32;
    for t in [1.0, 3.0, 5.0, 8.0] {
        let rgb = ffmpeg_frame("vision/footage/people-detection.mp4", t, 640, 360);
        let dets = e.run(&rgb, 640, 360, 0.4, &[], &interest).unwrap();
        for d in dets.iter().filter(|d| d.class == "person") {
            assert!(d.xyxy[2] > d.xyxy[0] && d.xyxy[3] > d.xyxy[1]);
            best = best.max(d.conf);
        }
    }
    assert!(best >= 0.4, "no person found in 4 real frames");
}

#[test]
fn validate_yolov5n_reports_real_layout_and_80_labels() {
    let mv = validate(&stock()).unwrap();
    assert_eq!(mv.labels.len(), 80);
    assert_eq!(mv.labels[0], "person");
    assert_eq!(mv.labels[67], "cell phone");
    assert_eq!(mv.output_schema, "yolov5");
    assert_eq!(mv.input_size, Some(640));
    assert_eq!(mv.provider, super::effective_provider());
    assert_eq!(mv.validation, "unvalidated");
    assert_eq!(mv.artifact_hash.len(), 64);
    assert_eq!(mv.adapter, AdapterKind::Object);
}

#[test]
fn short_custom_label_list_is_rejected() {
    // Labels outside COCO on an 85-wide output: not a COCO filter, not 80 labels.
    let m = module("w", "object", "vision/models/yolov5n.onnx", &["pistol", "knife"]);
    let e = validate(&m).unwrap_err();
    assert_eq!(e.code, "invalid_request");
    assert!(e.message.contains("label count 2"), "{}", e.message);
    // Pure layout rules.
    assert!(resolve_layout(15, &["a".into(), "b".into(), "c".into()]).is_err());
    let (s, l) = resolve_layout(8, &["a".into(), "b".into(), "c".into()]).unwrap();
    assert_eq!((s.as_str(), l.len()), ("yolov5", 3));
    assert_eq!(resolve_layout(5, &["pistol".into()]).unwrap().0, "yolov8-single");
    assert_eq!(resolve_layout(84, &[]).unwrap().0, "yolov8");
    assert!(resolve_layout(20, &[]).is_err());
}

#[test]
fn missing_file_and_weapon_without_model() {
    assert_eq!(validate(&module("x", "object", "vision/models/none.onnx", &[])).unwrap_err().code, "not_found");
    let e = validate(&module("weapon", "weapon", "", &["pistol", "knife"])).unwrap_err();
    assert_eq!(e.code, "unsupported");
    assert_eq!(validate(&module("fight", "fight", "", &[])).unwrap().adapter, AdapterKind::Fight);
}

fn det(id: &str, class: &str, conf: f32, bbox: [f32; 4], mv: &str, track: Option<&str>) -> Detection {
    Detection {
        detection_id: id.into(), observation_id: "obs1".into(), class: class.into(), confidence: conf, bbox,
        model_version: mv.into(), track_id: track.map(|s| s.to_string()), provenance: "model".into(), color: None,
    }
}

#[test]
fn fuse_dedups_across_modules_only() {
    let b = [0.1, 0.1, 0.3, 0.5];
    let b2 = [0.11, 0.1, 0.31, 0.5];
    let out = fuse(
        vec![
            det("a", "person", 0.7, b, "mv_a", None),
            det("b", "person", 0.9, b2, "mv_b", None),
            det("c", "car", 0.8, b, "mv_b", None), // different class: kept
            det("d", "person", 0.6, b, "mv_a", None), // same module as "a": not fused
        ],
        0.5,
    );
    assert_eq!(out.len(), 3);
    let p = out.iter().find(|d| d.detection_id == "b").unwrap();
    assert_eq!(p.provenance, "fused:mv_a,mv_b");
    assert!(out.iter().any(|d| d.detection_id == "c" && d.provenance == "model"));
    assert!(out.iter().any(|d| d.detection_id == "d"), "same-module box must survive");
    assert!(!out.iter().any(|d| d.detection_id == "a"));
    assert_eq!(out.iter().filter(|d| d.class == "person").count(), 2);
}

fn obs() -> FrameObservation {
    FrameObservation {
        observation_id: "obs1".into(), source_id: "s".into(), media_id: "m".into(), job_id: "j".into(), frame_index: None,
        pts: None, timebase: None, media_time_us: 1_000_000, capture_time_utc_us: None, indexed_at_utc_us: 0,
        model_versions: vec![], quality: Quality::Ok, thumbnail: None,
    }
}

#[test]
fn sixty_people_twelve_near_phones() {
    let mut dets = Vec::new();
    for i in 0..60 {
        let (cx, cy) = ((i % 10) as f32 * 0.09 + 0.05, (i / 10) as f32 * 0.15 + 0.1);
        dets.push(det(&format!("p{i}"), "person", 0.9, [cx - 0.03, cy - 0.06, cx + 0.03, cy + 0.06], "mv", None));
        if i < 12 {
            dets.push(det(&format!("ph{i}"), "cell phone", 0.8, [cx - 0.005, cy - 0.005, cx + 0.005, cy + 0.005], "mv", None));
        }
    }
    let rels = relate("obs1", &dets, &[]);
    assert_eq!(rels.len(), 12);
    assert!(rels.iter().all(|r| r.method == "bbox-proximity" && r.module_version == PROXIMITY_ID && r.predicate == Predicate::Near));
    let analyzed: Vec<String> = ["person", "cell phone"].iter().map(|s| s.to_string()).collect();
    let s = summarize(&obs(), &dets, &rels, &analyzed, 0.4);
    assert_eq!(s.class("person"), Count::Known(60));
    assert_eq!(s.class("cell phone"), Count::Known(12));
    assert_eq!(s.relation(Predicate::Near, "cell phone"), Count::Known(12));
    assert_eq!(s.class("knife"), Count::Unknown);
    assert_eq!(s.relation(Predicate::Holding, "cell phone"), Count::Unsupported);
}

#[test]
fn two_phones_one_person_is_one_subject_and_same_track_counts_once() {
    let dets = vec![
        det("p1", "person", 0.9, [0.1, 0.1, 0.4, 0.9], "mv", Some("t1")),
        det("p1b", "person", 0.8, [0.1, 0.1, 0.4, 0.9], "mv2", Some("t1")), // same track twice
        det("ph1", "cell phone", 0.8, [0.2, 0.4, 0.22, 0.42], "mv", None),
        det("ph2", "cell phone", 0.7, [0.3, 0.4, 0.32, 0.42], "mv", None),
        det("low", "person", 0.2, [0.6, 0.1, 0.9, 0.9], "mv", None),
    ];
    let rels = relate("obs1", &dets, &[]);
    let analyzed: Vec<String> = ["person", "cell phone"].iter().map(|s| s.to_string()).collect();
    let s = summarize(&obs(), &dets, &rels, &analyzed, 0.4);
    assert_eq!(s.class("person"), Count::Known(1), "same track twice + below-threshold box");
    assert_eq!(s.class("cell phone"), Count::Known(2));
    assert!(rels.len() >= 2);
    assert_eq!(s.relation(Predicate::Near, "cell phone"), Count::Known(1));
}

#[test]
fn holding_is_unsupported_without_a_relation_model() {
    let mv = validate(&stock()).unwrap();
    assert_eq!(support(&[mv.clone()], Predicate::Holding), Support::Unsupported);
    assert_eq!(support(&[mv.clone()], Predicate::Near), Support::Supported);
    let mut rel = mv;
    rel.adapter = AdapterKind::Relation;
    rel.supported_relations = vec![Predicate::Holding];
    assert_eq!(support(&[rel], Predicate::Holding), Support::Supported);
}

/// Latency on real frames with the effective provider. Run:
/// `CW_BENCH=1 cargo test ... --ignored bench_latency -- --nocapture`; writes docs/evidence/c/latency.md.
#[test]
#[ignore]
fn bench_latency() {
    let threads = 4;
    let mut e = yolo::Engine::load(&repo("vision/models/yolov5n.onnx"), threads).unwrap();
    let interest: Vec<String> = stock().labels;
    let mut report = String::new();
    let cpu = String::from_utf8_lossy(&std::process::Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]).output().unwrap().stdout).trim().to_string();
    report += &format!("# Inference latency (measured)\n\nCPU: {cpu}\nProvider (effective): {}\nModel: vision/models/yolov5n.onnx, input {}x{}\nIntra-op threads: {threads}\nWarm-up: 5 runs discarded. Timing covers letterbox + ONNX run + decode/NMS (`Engine::run`); frame decode excluded.\n\n| Footage | Frames | Frame size | p50 ms | p95 ms | mean ms | detections/frame (mean) |\n|---|---|---|---|---|---|---|\n", effective_provider(), e.input_size(), e.input_size());
    for (video, w, h) in [("vision/footage/people-detection.mp4", 640u32, 360u32), ("vision/footage/person-bicycle-car-detection.mp4", 640, 360)] {
        let frames: Vec<Vec<u8>> = (0..30).map(|i| ffmpeg_frame(video, 0.5 + i as f32 * 0.5, w, h)).collect();
        for f in frames.iter().take(5) {
            e.run(f, w, h, 0.4, &[], &interest).unwrap();
        }
        let mut ms = Vec::new();
        let mut ndet = 0usize;
        for f in &frames {
            let t = std::time::Instant::now();
            let d = e.run(f, w, h, 0.4, &[], &interest).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            ndet += d.len();
        }
        ms.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
        let mean = ms.iter().sum::<f64>() / ms.len() as f64;
        report += &format!("| {video} | {} | {w}x{h} | {:.1} | {:.1} | {:.1} | {:.1} |\n", ms.len(), pct(0.5), pct(0.95), mean, ndet as f64 / ms.len() as f64);
    }
    report += "\nNo accuracy is claimed here: there is no labeled ground truth in the repo (see vision/eval/README.md).\n";
    let dir = repo("docs/evidence/c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("latency.md"), &report).unwrap();
    println!("{report}");
}

/// Benchmark + accuracy on the 30 hand-labelled frames. Not part of the normal suite.
/// `MBENCH="name=path,name2=path2" cargo test ... bench_models -- --ignored --nocapture`
/// Output is markdown rows: latency p50/p95 (ms, preprocess + ORT + decode, CPU) and per-conf P/R.
#[test]
#[ignore]
fn bench_models() {
    let spec = std::env::var("MBENCH").expect("MBENCH=name=path,...");
    let threads: usize = std::env::var("MTHREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let labels: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo("vision/eval/labels/sample30.json")).unwrap()).unwrap();
    let frames: Vec<(Vec<u8>, std::collections::BTreeMap<String, u32>)> = labels["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let t = f["media_time_us"].as_i64().unwrap() as f32 / 1e6;
            let rgb = ffmpeg_frame(&format!("vision/footage/{}", f["media"].as_str().unwrap()), t, 640, 360);
            let l = f["labels"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_u64().unwrap() as u32)).collect();
            (rgb, l)
        })
        .collect();
    let interest: Vec<String> = ["person", "bicycle", "car"].iter().map(|s| s.to_string()).collect();
    for item in spec.split(',') {
        let (name, path) = item.split_once('=').unwrap();
        let mut e = yolo::Engine::load(&resolve_path(path), threads).unwrap();
        let mut dets_all = Vec::new();
        let mut ms = Vec::new();
        for pass in 0..6 {
            for (rgb, _) in &frames {
                let t0 = std::time::Instant::now();
                let d = e.run(rgb, 640, 360, 0.15, &[], &interest).unwrap();
                if pass == 0 {
                    dets_all.push(d);
                } else if pass >= 2 {
                    ms.push(t0.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        ms.sort_by(|a, b| a.total_cmp(b));
        let (p50, p95) = (ms[ms.len() / 2], ms[(ms.len() as f32 * 0.95) as usize - 1]);
        let mut cells = Vec::new();
        for conf in [0.25f32, 0.3, 0.4, 0.5, 0.6] {
            let (mut tp, mut fp, mut fnn) = (0u32, 0u32, 0u32);
            for (d, (_, l)) in dets_all.iter().zip(&frames) {
                for c in &interest {
                    let m = d.iter().filter(|x| &x.class == c && x.conf >= conf).count() as u32;
                    let t = *l.get(c).unwrap_or(&0);
                    tp += m.min(t);
                    fp += m.saturating_sub(t);
                    fnn += t.saturating_sub(m);
                }
            }
            cells.push(format!("conf {conf}: TP {tp} FP {fp} FN {fnn} P {:.2} R {:.2}", tp as f32 / (tp + fp).max(1) as f32, tp as f32 / (tp + fnn).max(1) as f32));
        }
        println!("BENCH {name} | in {} | layout {} | threads {threads} | p50 {p50:.1} ms | p95 {p95:.1} ms | min {:.1} ms\n  {}", e.input_size(), e.layout(), ms[0], cells.join("\n  "));
    }
}

/// Every shipped detector family must run for real on a real frame (people_05: two people)
/// and validate() must report its true layout from a forward pass.
#[test]
fn every_shipped_model_family_detects_people_and_validates() {
    let shipped = [
        ("yolov5n.onnx", "yolov5", 640),
        ("yolo11s.onnx", "yolov8", 640),
        ("yolo26n.onnx", "e2e", 640),
        ("rfdetr-nano.onnx", "detr", 384),
    ];
    let rgb = ffmpeg_frame("vision/footage/people-detection.mp4", 17.666667, 640, 360);
    let interest = vec!["person".to_string()];
    for (file, layout, size) in shipped {
        let path = format!("vision/models/{file}");
        if !repo(&path).is_file() {
            panic!("shipped model missing: {path}");
        }
        let mut e = yolo::Engine::load(&repo(&path), 2).unwrap();
        assert_eq!(e.input_size(), size, "{file}");
        let dets = e.run(&rgb, 640, 360, 0.4, &[], &interest).unwrap();
        assert_eq!(e.layout(), layout, "{file}");
        let people = dets.iter().filter(|d| d.class == "person").count();
        assert_eq!(people, 2, "{file}: expected both people, got {dets:?}");
        assert!(dets.iter().all(|d| d.class == "person" && d.xyxy[2] > d.xyxy[0] && d.xyxy[3] > d.xyxy[1] && d.xyxy[2] <= 640.0 && d.xyxy[3] <= 360.0));
        let mv = validate(&module("m", "object", &path, &["person"])).unwrap();
        assert_eq!(mv.labels.len(), 80, "{file}");
        assert_eq!(mv.input_size, Some(size as u32));
        assert_eq!(mv.output_schema, if layout == "yolov5" { "yolov5" } else { layout }, "{file}");
    }
}
