//! YOLOv5 / YOLOv8 ONNX via ONNX Runtime. The runtime ships with the binary.
//! On an Intel CPU, OpenVINO is often faster; it needs its own install on every PC,
//! so a module file can move there later without a rule changing.

use std::path::Path;

pub use crate::watch::models::labels::COCO;

#[derive(Clone, Debug)]
pub struct Det {
    pub xyxy: [f32; 4],
    pub class: String,
    pub conf: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Fit {
    pub size: f32,
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

pub fn plan(w: u32, h: u32, size: u32) -> Fit {
    let size = size.max(1) as f32;
    let scale = if w == 0 || h == 0 { 1.0 } else { (size / w as f32).min(size / h as f32) };
    let nw = w as f32 * scale;
    let nh = h as f32 * scale;
    Fit { size, scale, pad_x: ((size - nw) / 2.0).max(0.0), pad_y: ((size - nh) / 2.0).max(0.0) }
}

pub fn letterbox(rgb: &[u8], w: u32, h: u32, size: u32) -> (Vec<u8>, Fit) {
    let fit = plan(w, h, size);
    let size = size.max(1);
    let mut canvas = vec![114u8; (size * size * 3) as usize];
    if w == 0 || h == 0 {
        return (canvas, fit);
    }
    let row = (w as usize) * 3;
    let dst_stride = (size as usize) * 3;
    let pad = fit.pad_y.round() as u32;
    if w == size && (fit.scale - 1.0).abs() < 0.01 {
        for y in 0..h.min(size.saturating_sub(pad)) {
            let s = (y as usize) * row;
            let d = ((pad + y) as usize) * dst_stride + (fit.pad_x.round() as usize) * 3;
            let n = row.min(canvas.len().saturating_sub(d));
            if s + n <= rgb.len() {
                canvas[d..d + n].copy_from_slice(&rgb[s..s + n]);
            }
        }
        return (canvas, fit);
    }
    // Sample the destination grid. Forward-mapping source pixels left holes whenever
    // a camera frame was smaller than the detector input.
    let nw = (w as f32 * fit.scale).round() as u32;
    let nh = (h as f32 * fit.scale).round() as u32;
    let px = fit.pad_x.round() as u32;
    let py = fit.pad_y.round() as u32;
    for dy in 0..nh.min(size.saturating_sub(py)) {
        let sy = ((dy as f32 / fit.scale) as u32).min(h - 1);
        for dx in 0..nw.min(size.saturating_sub(px)) {
            let sx = ((dx as f32 / fit.scale) as u32).min(w - 1);
            let s = ((sy * w + sx) * 3) as usize;
            let d = (((py + dy) * size + px + dx) * 3) as usize;
            if s + 2 < rgb.len() {
                canvas[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
            }
        }
    }
    (canvas, fit)
}

pub fn parse(
    data: &[f32],
    rows: usize,
    cols: usize,
    conf: f32,
    labels: &[String],
    interest: &[String],
    fit: Fit,
    src_w: f32,
    src_h: f32,
) -> Vec<Det> {
    if cols < 5 || rows == 0 || data.len() < rows * cols {
        return Vec::new();
    }
    let (class_at, n_classes, objectness) = layout(cols, labels.len());
    if n_classes == 0 {
        return Vec::new();
    }
    let custom = !labels.is_empty() && labels.len() == n_classes;
    let mut cand: Vec<(f32, usize, [f32; 4])> = Vec::new();
    for r in 0..rows {
        let row = &data[r * cols..(r + 1) * cols];
        let mut best_c = 0usize;
        let mut best_s = 0.0f32;
        for c in 0..n_classes {
            let s = row[class_at + c];
            if s > best_s {
                best_s = s;
                best_c = c;
            }
        }
        let score = if objectness { row[4] * best_s } else { best_s };
        if score < conf {
            continue;
        }
        let Some(name) = class_name(best_c, n_classes, labels, interest, custom) else { continue };
        cand.push((score, r, [row[0], row[1], row[2], row[3]]));
        let _ = name;
    }
    // names again at the end; store them
    let mut named: Vec<(f32, String, [f32; 4])> = Vec::new();
    let norm = cand.iter().map(|(_, _, b)| b[2].max(b[3])).fold(0.0f32, f32::max) <= 2.0;
    for (score, r, mut b) in cand {
        let row = &data[r * cols..(r + 1) * cols];
        let mut best_c = 0usize;
        let mut best_s = 0.0f32;
        for c in 0..n_classes {
            let s = row[class_at + c];
            if s > best_s {
                best_s = s;
                best_c = c;
            }
        }
        let Some(name) = class_name(best_c, n_classes, labels, interest, custom) else { continue };
        if norm {
            b = [b[0] * fit.size, b[1] * fit.size, b[2] * fit.size, b[3] * fit.size];
        }
        named.push((score, name, b));
    }
    let mut by_class: Vec<(String, Vec<(f32, [f32; 4])>)> = Vec::new();
    for (score, name, b) in named {
        if let Some(g) = by_class.iter_mut().find(|g| g.0 == name) {
            g.1.push((score, unmap(fit, b, src_w, src_h)));
        } else {
            by_class.push((name, vec![(score, unmap(fit, b, src_w, src_h))]));
        }
    }
    let mut out = Vec::new();
    for (name, mut boxes) in by_class {
        boxes.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut kept: Vec<(f32, [f32; 4])> = Vec::new();
        for (score, xy) in boxes {
            if kept.iter().any(|k| crate::watch::track::iou(k.1, xy) > 0.45) {
                continue;
            }
            kept.push((score, xy));
        }
        for (score, xyxy) in kept {
            out.push(Det { xyxy, class: name.clone(), conf: score });
        }
    }
    out
}

fn unmap(fit: Fit, b: [f32; 4], src_w: f32, src_h: f32) -> [f32; 4] {
    let scale = fit.scale.max(1e-6);
    let x1 = ((b[0] - b[2] * 0.5) - fit.pad_x) / scale;
    let y1 = ((b[1] - b[3] * 0.5) - fit.pad_y) / scale;
    let x2 = ((b[0] + b[2] * 0.5) - fit.pad_x) / scale;
    let y2 = ((b[1] + b[3] * 0.5) - fit.pad_y) / scale;
    [x1.clamp(0.0, src_w), y1.clamp(0.0, src_h), x2.clamp(0.0, src_w), y2.clamp(0.0, src_h)]
}

fn layout(cols: usize, n_labels: usize) -> (usize, usize, bool) {
    if n_labels > 0 && cols == n_labels + 5 {
        return (5, n_labels, true);
    }
    if n_labels > 0 && cols == n_labels + 4 {
        return (4, n_labels, false);
    }
    if cols == 85 {
        return (5, 80, true);
    }
    if cols == 84 {
        return (4, 80, false);
    }
    (4, cols.saturating_sub(4), false)
}

fn class_name(cls: usize, n_classes: usize, labels: &[String], interest: &[String], custom: bool) -> Option<String> {
    if custom {
        return labels.get(cls).cloned().filter(|s| !s.is_empty());
    }
    let coco = COCO.get(cls).copied()?;
    if n_classes != 80 {
        return labels.get(cls).cloned().or_else(|| Some(format!("class{cls}")));
    }
    if interest.iter().any(|c| c == coco) || labels.iter().any(|c| c == coco) {
        Some(coco.to_string())
    } else {
        None
    }
}

/// COCO category id (1..=90, with gaps) of each of the 80 classes, in COCO-80 order.
/// DETR-family COCO checkpoints (RF-DETR, RT-DETR, D-FINE) index their 91 logits by this id.
pub const COCO91_IDS: [usize; 80] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27, 28, 31, 32, 33, 34, 35, 36, 37,
    38, 39, 40, 41, 42, 43, 44, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 67, 70, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 84, 85, 86, 87, 88, 89, 90,
];

/// Which family an engine speaks: decides preprocessing and decoding.
/// `Yolo`: letterbox (grey 114), /255; output yolov5 `[1,N,85]`, yolov8/11 `[1,84,N]` or end-to-end `[1,K,6]`.
/// `Detr`: stretch-resize, ImageNet mean/std; two outputs, boxes `[1,Q,4]` (cxcywh, 0..1) and logits `[1,Q,C]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    Yolo,
    Detr,
}

/// What the last forward pass looked like.
enum Raw {
    Rows(usize, usize, Vec<f32>),
    E2e(usize, Vec<f32>),
    Detr { q: usize, c: usize, boxes: Vec<f32>, logits: Vec<f32> },
}

pub struct Engine {
    session: ort::session::Session,
    size: usize,
    input: String,
    fp16: bool,
    family: Family,
    layout: &'static str,
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Bilinear stretch of an RGB frame to size x size (no aspect preservation, as DETR models are trained).
pub fn stretch(rgb: &[u8], w: u32, h: u32, size: u32) -> Vec<u8> {
    let s = size.max(1) as usize;
    let mut out = vec![114u8; s * s * 3];
    if w == 0 || h == 0 || rgb.len() < (w * h * 3) as usize {
        return out;
    }
    let (w, h) = (w as usize, h as usize);
    let (fx, fy) = (w as f32 / s as f32, h as f32 / s as f32);
    for dy in 0..s {
        let sy = ((dy as f32 + 0.5) * fy - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, wy) = (sy as usize, sy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for dx in 0..s {
            let sx = ((dx as f32 + 0.5) * fx - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, wx) = (sx as usize, sx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..3 {
                let p = |y: usize, x: usize| rgb[(y * w + x) * 3 + c] as f32;
                let top = p(y0, x0) * (1.0 - wx) + p(y0, x1) * wx;
                let bot = p(y1, x0) * (1.0 - wx) + p(y1, x1) * wx;
                out[(dy * s + dx) * 3 + c] = (top * (1.0 - wy) + bot * wy + 0.5) as u8;
            }
        }
    }
    out
}

fn decode_names(n_classes: usize, labels: &[String]) -> (usize, bool) {
    // Interest filter (all COCO names) vs full custom list.
    let filter = labels.iter().all(|l| COCO.contains(&l.as_str()));
    if labels.is_empty() || (filter && labels.len() != 80) { (n_classes, false) } else { (labels.len(), true) }
}

/// End-to-end (NMS-free) rows: x1,y1,x2,y2,score,class in letterboxed input pixels.
pub fn parse_e2e(data: &[f32], rows: usize, conf: f32, labels: &[String], interest: &[String], fit: Fit, src_w: f32, src_h: f32) -> Vec<Det> {
    let (n, custom) = decode_names(80, labels);
    let mut out = Vec::new();
    for r in 0..rows.min(data.len() / 6) {
        let row = &data[r * 6..r * 6 + 6];
        if row[4] < conf || row[5] < 0.0 {
            continue;
        }
        let Some(class) = class_name(row[5] as usize, n, labels, interest, custom) else { continue };
        let s = fit.scale.max(1e-6);
        let cl = |v: f32, m: f32| v.clamp(0.0, m);
        out.push(Det {
            xyxy: [
                cl((row[0] - fit.pad_x) / s, src_w),
                cl((row[1] - fit.pad_y) / s, src_h),
                cl((row[2] - fit.pad_x) / s, src_w),
                cl((row[3] - fit.pad_y) / s, src_h),
            ],
            class,
            conf: row[4],
        });
    }
    out
}

/// DETR decode: one best class per query, sigmoid scores, boxes cxcywh normalised to the stretched frame.
/// `c == 91` means COCO category ids (gaps); `c == 80` means COCO-80 order.
pub fn parse_detr(boxes: &[f32], logits: &[f32], q: usize, c: usize, conf: f32, labels: &[String], interest: &[String], src_w: f32, src_h: f32) -> Vec<Det> {
    if c == 0 || boxes.len() < q * 4 || logits.len() < q * c {
        return Vec::new();
    }
    let (n, custom) = decode_names(if c == 91 { 80 } else { c }, labels);
    let mut out = Vec::new();
    for i in 0..q {
        let row = &logits[i * c..(i + 1) * c];
        let (mut bi, mut bl) = (0usize, f32::MIN);
        for (k, v) in row.iter().enumerate() {
            if *v > bl && !(c == 91 && !COCO91_IDS.contains(&k)) {
                bl = *v;
                bi = k;
            }
        }
        let score = sigmoid(bl);
        if score < conf {
            continue;
        }
        let cls = if c == 91 { COCO91_IDS.iter().position(|id| *id == bi) } else { Some(bi) };
        let Some(class) = cls.and_then(|k| class_name(k, n, labels, interest, custom)) else { continue };
        let b = &boxes[i * 4..i * 4 + 4];
        let cl = |v: f32, m: f32| v.clamp(0.0, m);
        out.push(Det {
            xyxy: [
                cl((b[0] - b[2] * 0.5) * src_w, src_w),
                cl((b[1] - b[3] * 0.5) * src_h, src_h),
                cl((b[0] + b[2] * 0.5) * src_w, src_w),
                cl((b[1] + b[3] * 0.5) * src_h, src_h),
            ],
            class,
            conf: score,
        });
    }
    out
}

fn tensor_f32(v: &ort::value::DynValue) -> Result<(Vec<usize>, Vec<f32>), String> {
    if let Ok((shape, s)) = v.try_extract_tensor::<f32>() {
        return Ok((shape.iter().map(|d| *d as usize).filter(|d| *d > 0).collect(), s.to_vec()));
    }
    let (shape, s) = v.try_extract_tensor::<half::f16>().map_err(|e| e.to_string())?;
    Ok((shape.iter().map(|d| *d as usize).filter(|d| *d > 0).collect(), s.iter().map(|x| x.to_f32()).collect()))
}

#[allow(unexpected_cfgs)]
impl Engine {
    pub fn load(path: &Path, threads: usize) -> Result<Self, String> {
        static START: std::sync::Once = std::sync::Once::new();
        START.call_once(|| {
            let _ = ort::init().commit();
        });
        let builder = ort::session::Session::builder().map_err(|e| e.to_string())?;
        // Apple Neural Engine / GPU via CoreML. Needs `coreml = ["ort/coreml"]` in Cargo.toml (measured 10-15x
        // faster than the bundled CPU kernels on an M5, docs/evidence/m/models_bench.md). Falls back to CPU per-op.
        #[cfg(feature = "coreml")]
        let builder = builder
            .with_execution_providers([ort::execution_providers::CoreMLExecutionProvider::default().build()])
            .map_err(|e| e.to_string())?;
        let session = builder
            .with_intra_threads(threads)
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| e.to_string())?;
        let input = session.inputs[0].name.clone();
        let fp16 = matches!(session.inputs[0].input_type, ort::value::ValueType::Tensor { ty: ort::tensor::TensorElementType::Float16, .. });
        let size = input_size(&session).unwrap_or(640);
        let family = if session.outputs.len() >= 2 { Family::Detr } else { Family::Yolo };
        Ok(Engine { session, size, input, fp16, family, layout: "" })
    }

    /// Square model input edge in pixels. Frames of any size are letterboxed (YOLO) or
    /// stretched (DETR) to this; preview resolution does not change what the detector sees.
    pub fn input_size(&self) -> usize {
        self.size
    }

    #[allow(dead_code)]
    pub fn family(&self) -> Family {
        self.family
    }

    /// Output layout seen on the last forward pass: "yolov5", "yolov8", "e2e" or "detr" ("" before any run).
    pub fn layout(&self) -> &'static str {
        self.layout
    }

    /// Run the model on a blank canvas and return (rows, cols) of the oriented output,
    /// i.e. one candidate per row. For `e2e` cols is 6; for `detr` cols is the logit width.
    pub fn probe(&mut self) -> Result<(usize, usize), String> {
        let canvas = vec![114u8; self.size * self.size * 3];
        Ok(match self.infer(&canvas)? {
            Raw::Rows(r, c, _) => (r, c),
            Raw::E2e(r, _) => (r, 6),
            Raw::Detr { q, c, .. } => (q, c),
        })
    }

    fn infer(&mut self, canvas: &[u8]) -> Result<Raw, String> {
        let n = self.size * self.size;
        let input = self.input.clone();
        let (mean, std) = match self.family {
            Family::Yolo => ([0.0f32; 3], [1.0f32; 3]),
            Family::Detr => ([0.485, 0.456, 0.406], [0.229, 0.224, 0.225]),
        };
        let mut data = vec![0f32; 3 * n];
        for i in 0..n {
            for c in 0..3 {
                data[c * n + i] = (canvas[i * 3 + c] as f32 / 255.0 - mean[c]) / std[c];
            }
        }
        let shape = (1, 3, self.size, self.size);
        let outputs = if self.fp16 {
            let d: Vec<half::f16> = data.iter().map(|v| half::f16::from_f32(*v)).collect();
            let tensor = ort::value::Tensor::from_array(ndarray::Array4::from_shape_vec(shape, d).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            self.session.run(ort::inputs![input.as_str() => tensor]).map_err(|e| e.to_string())?
        } else {
            let tensor = ort::value::Tensor::from_array(ndarray::Array4::from_shape_vec(shape, data).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            self.session.run(ort::inputs![input.as_str() => tensor]).map_err(|e| e.to_string())?
        };
        if self.family == Family::Detr {
            let mut boxes = None;
            let mut logits = None;
            for i in 0..outputs.len() {
                let (d, v) = tensor_f32(&outputs[i])?;
                match d.as_slice() {
                    [.., q, 4] => boxes = Some((*q, v)),
                    [.., q, c] => logits = Some((*q, *c, v)),
                    _ => {}
                }
            }
            let ((_, boxes), (q, c, logits)) = boxes.zip(logits).ok_or("DETR model needs a [Q,4] box output and a [Q,C] logit output")?;
            self.layout = "detr";
            return Ok(Raw::Detr { q, c, boxes, logits });
        }
        let (dims, flat) = tensor_f32(&outputs[0])?;
        // Last two dims are (candidates, width) or (width, candidates); width is the smaller.
        let (d0, d1) = match dims.as_slice() {
            [.., a, b] => (*a, *b),
            _ => return Err(format!("unexpected output rank {:?}", dims)),
        };
        // End-to-end head: a few hundred rows of exactly 6 (x1,y1,x2,y2,score,class). A one-class
        // yolov5 has 6 columns too but thousands of rows, so row count tells them apart.
        if d1 == 6 && d0 > 6 && d0 <= 1000 {
            self.layout = "e2e";
            return Ok(Raw::E2e(d0, flat));
        }
        if d0 >= d1 {
            self.layout = "yolov5";
            return Ok(Raw::Rows(d0, d1, flat));
        }
        let mut flipped = vec![0f32; flat.len()];
        for r in 0..d1 {
            for c in 0..d0 {
                flipped[r * d0 + c] = flat[c * d1 + r];
            }
        }
        self.layout = "yolov8";
        Ok(Raw::Rows(d1, d0, flipped))
    }

    pub fn run(&mut self, rgb: &[u8], w: u32, h: u32, conf: f32, labels: &[String], interest: &[String]) -> Result<Vec<Det>, String> {
        let (sw, sh) = (w as f32, h as f32);
        if self.family == Family::Detr {
            let canvas = stretch(rgb, w, h, self.size as u32);
            return match self.infer(&canvas)? {
                Raw::Detr { q, c, boxes, logits } => Ok(parse_detr(&boxes, &logits, q, c, conf, labels, interest, sw, sh)),
                _ => Err("unexpected DETR output".into()),
            };
        }
        let (canvas, fit) = letterbox(rgb, w, h, self.size as u32);
        Ok(match self.infer(&canvas)? {
            Raw::Rows(rows, cols, flat) => parse(&flat, rows, cols, conf, labels, interest, fit, sw, sh),
            Raw::E2e(rows, flat) => parse_e2e(&flat, rows, conf, labels, interest, fit, sw, sh),
            Raw::Detr { .. } => Vec::new(),
        })
    }
}

fn input_size(session: &ort::session::Session) -> Option<usize> {
    let ort::value::ValueType::Tensor { shape, .. } = &session.inputs.first()?.input_type else { return None };
    shape.iter().rev().find(|d| **d > 1).map(|d| *d as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upscaling_fills_every_pixel() {
        let rgb = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
        let (canvas, _) = letterbox(&rgb, 2, 2, 8);
        assert!(canvas.chunks_exact(3).all(|p| p == [255, 0, 0]));
    }

    #[test]
    fn classroom_can_contain_more_than_forty_people() {
        let mut rows = Vec::new();
        for i in 0..60 {
            rows.extend([10.0 + (i % 10) as f32 * 60.0, 10.0 + (i / 10) as f32 * 60.0, 8.0, 8.0, 0.9]);
        }
        let labels = vec!["person".into()];
        let dets = parse(&rows, 60, 5, 0.4, &labels, &labels, plan(640, 640, 640), 640.0, 640.0);
        assert_eq!(dets.len(), 60);
    }

    #[test]
    fn one_person_row_survives_nms() {
        let mut row = vec![0f32; 85];
        row[0] = 320.0;
        row[1] = 240.0;
        row[2] = 40.0;
        row[3] = 80.0;
        row[4] = 0.9;
        row[5] = 0.8; // person
        let fit = plan(640, 360, 640);
        let labels = vec!["person".into()];
        let dets = parse(&row, 1, 85, 0.4, &labels, &labels, fit, 640.0, 360.0);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].class, "person");
        assert!(dets[0].conf > 0.5);
        assert!(dets[0].xyxy[2] > dets[0].xyxy[0]);
    }

    #[test]
    fn a_class_outside_the_rules_is_dropped() {
        let mut row = vec![0f32; 85];
        row[0] = 100.0;
        row[1] = 100.0;
        row[2] = 20.0;
        row[3] = 20.0;
        row[4] = 0.9;
        row[5 + 14] = 0.9; // bird
        let fit = plan(640, 360, 640);
        let interest = vec!["person".into()];
        let dets = parse(&row, 1, 85, 0.4, &[], &interest, fit, 640.0, 360.0);
        assert!(dets.is_empty());
    }

    #[test]
    fn coco91_table_has_80_ascending_ids() {
        assert_eq!(COCO91_IDS.len(), 80);
        assert!(COCO91_IDS.windows(2).all(|w| w[0] < w[1]) && COCO91_IDS[79] == 90);
        assert_eq!(COCO91_IDS[COCO.iter().position(|c| *c == "cell phone").unwrap()], 77);
    }

    #[test]
    fn e2e_rows_unmap_and_filter() {
        // person (class 0) in the letterboxed frame of a 640x360 source at size 640: pad_y = 140.
        let rows = [100.0, 240.0, 200.0, 440.0, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2, 0.0];
        let f = plan(640, 360, 640);
        let dets = parse_e2e(&rows, 2, 0.4, &[], &["person".into()], f, 640.0, 360.0);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].xyxy, [100.0, 100.0, 200.0, 300.0]);
    }

    #[test]
    fn detr_maps_coco91_ids_and_sigmoid() {
        // one query: box centre (0.5,0.5) size 0.2x0.4; logit peak at COCO id 77 = cell phone.
        let mut logits = vec![-8.0f32; 91];
        logits[77] = 3.0;
        let boxes = [0.5, 0.5, 0.2, 0.4];
        let d = parse_detr(&boxes, &logits, 1, 91, 0.5, &[], &["cell phone".into()], 100.0, 200.0);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].class, "cell phone");
        assert!((d[0].conf - 0.9526).abs() < 1e-3);
        assert!(d[0].xyxy.iter().zip([40.0, 60.0, 60.0, 140.0]).all(|(a, b)| (a - b).abs() < 1e-3));
        logits[77] = -8.0;
        logits[12] = 9.0; // id 12 does not exist in COCO-91
        assert!(parse_detr(&boxes, &logits, 1, 91, 0.5, &[], &["person".into()], 100.0, 200.0).is_empty());
    }

    #[test]
    fn stretch_fills_whole_square() {
        let rgb = vec![10u8, 20, 30].repeat(6 * 3);
        let out = stretch(&rgb, 6, 3, 8);
        assert!(out.chunks_exact(3).all(|p| p == [10, 20, 30]));
    }
}
