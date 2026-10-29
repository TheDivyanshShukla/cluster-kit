//! YOLOv5 / YOLOv8 ONNX via ONNX Runtime. The runtime ships with the binary.
//! On an Intel CPU, OpenVINO is often faster; it needs its own install on every PC,
//! so a module file can move there later without a rule changing.

use std::path::Path;

pub const COCO: [&str; 80] = [
    "person", "bicycle", "car", "motorcycle", "airplane", "bus", "train", "truck", "boat", "traffic light",
    "fire hydrant", "stop sign", "parking meter", "bench", "bird", "cat", "dog", "horse", "sheep", "cow",
    "elephant", "bear", "zebra", "giraffe", "backpack", "umbrella", "handbag", "tie", "suitcase", "frisbee",
    "skis", "snowboard", "sports ball", "kite", "baseball bat", "baseball glove", "skateboard", "surfboard",
    "tennis racket", "bottle", "wine glass", "cup", "fork", "knife", "spoon", "bowl", "banana", "apple",
    "sandwich", "orange", "broccoli", "carrot", "hot dog", "pizza", "donut", "cake", "chair", "couch",
    "potted plant", "bed", "dining table", "toilet", "tv", "laptop", "mouse", "remote", "keyboard",
    "cell phone", "microwave", "oven", "toaster", "sink", "refrigerator", "book", "clock", "vase",
    "scissors", "teddy bear", "hair drier", "toothbrush",
];

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
    for y in 0..h {
        let dy = (fit.pad_y + y as f32 * fit.scale).round() as i32;
        if dy < 0 || dy >= size as i32 {
            continue;
        }
        for x in 0..w {
            let dx = (fit.pad_x + x as f32 * fit.scale).round() as i32;
            if dx < 0 || dx >= size as i32 {
                continue;
            }
            let s = ((y * w + x) * 3) as usize;
            let d = ((dy as u32 * size + dx as u32) * 3) as usize;
            if s + 2 < rgb.len() && d + 2 < canvas.len() {
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
    if cols < 6 || rows == 0 || data.len() < rows * cols {
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
            if kept.len() == 40 {
                break;
            }
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

pub struct Engine {
    session: ort::session::Session,
    size: usize,
    input: String,
    fp16: bool,
}

impl Engine {
    pub fn load(path: &Path, threads: usize) -> Result<Self, String> {
        static START: std::sync::Once = std::sync::Once::new();
        START.call_once(|| {
            let _ = ort::init().commit();
        });
        let session = ort::session::Session::builder()
            .map_err(|e| e.to_string())?
            .with_intra_threads(threads)
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| e.to_string())?;
        let input = session.inputs[0].name.clone();
        let fp16 = matches!(session.inputs[0].input_type, ort::value::ValueType::Tensor { ty: ort::tensor::TensorElementType::Float16, .. });
        let size = input_size(&session).unwrap_or(640);
        Ok(Engine { session, size, input, fp16 })
    }

    pub fn run(&mut self, rgb: &[u8], w: u32, h: u32, conf: f32, labels: &[String], interest: &[String]) -> Result<Vec<Det>, String> {
        let (canvas, fit) = letterbox(rgb, w, h, self.size as u32);
        let n = self.size * self.size;
        let input = self.input.clone();
        let outputs = if self.fp16 {
            let mut data = vec![half::f16::ZERO; 3 * n];
            for i in 0..n {
                data[i] = half::f16::from_f32(canvas[i * 3] as f32 / 255.0);
                data[n + i] = half::f16::from_f32(canvas[i * 3 + 1] as f32 / 255.0);
                data[2 * n + i] = half::f16::from_f32(canvas[i * 3 + 2] as f32 / 255.0);
            }
            let array = ndarray::Array4::from_shape_vec((1, 3, self.size, self.size), data).map_err(|e| e.to_string())?;
            let tensor = ort::value::Tensor::from_array(array).map_err(|e| e.to_string())?;
            self.session.run(ort::inputs![input.as_str() => tensor]).map_err(|e| e.to_string())?
        } else {
            let mut data = vec![0f32; 3 * n];
            for i in 0..n {
                data[i] = canvas[i * 3] as f32 / 255.0;
                data[n + i] = canvas[i * 3 + 1] as f32 / 255.0;
                data[2 * n + i] = canvas[i * 3 + 2] as f32 / 255.0;
            }
            let array = ndarray::Array4::from_shape_vec((1, 3, self.size, self.size), data).map_err(|e| e.to_string())?;
            let tensor = ort::value::Tensor::from_array(array).map_err(|e| e.to_string())?;
            self.session.run(ort::inputs![input.as_str() => tensor]).map_err(|e| e.to_string())?
        };
        let (dims, mut flat) = if self.fp16 {
            let (shape, slice) = outputs[0].try_extract_tensor::<half::f16>().map_err(|e| e.to_string())?;
            let dims: Vec<usize> = shape.iter().map(|d| *d as usize).filter(|d| *d > 0).collect();
            (dims, slice.iter().map(|v| v.to_f32()).collect::<Vec<f32>>())
        } else {
            let (shape, slice) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            let dims: Vec<usize> = shape.iter().map(|d| *d as usize).filter(|d| *d > 0).collect();
            (dims, slice.to_vec())
        };
        let (rows, cols) = match dims.as_slice() {
            [_, a, b] if *a > *b && *b >= 6 => (*a, *b),
            [_, a, b] if *b > *a && *a >= 6 => (*b, *a),
            [a, b] if *a > *b => (*a, *b),
            [a, b] => (*b, *a),
            _ => return Ok(Vec::new()),
        };
        if dims.len() == 3 && dims[1] < dims[2] && dims[1] >= 6 {
            // (1, cols, rows) -> (rows, cols)
            let cols_m = dims[1];
            let rows_m = dims[2];
            let mut flipped = vec![0f32; flat.len()];
            for r in 0..rows_m {
                for c in 0..cols_m {
                    flipped[r * cols_m + c] = flat[c * rows_m + r];
                }
            }
            flat = flipped;
            return Ok(parse(&flat, rows_m, cols_m, conf, labels, interest, fit, w as f32, h as f32));
        }
        Ok(parse(&flat, rows, cols, conf, labels, interest, fit, w as f32, h as f32))
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
}
