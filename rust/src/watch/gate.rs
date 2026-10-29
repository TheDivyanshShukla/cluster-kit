//! Cheap checks that run on every frame. A blurry or blank frame never reaches a detector.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Sharp,
    Blur,
    Blank,
}

#[derive(Clone)]
pub struct Gray {
    pub px: Vec<u8>,
    pub w: usize,
    pub h: usize,
}

/// Nearest-neighbour grey image, about `tw` wide. The gate does not need the full frame.
pub fn shrink_gray(rgb: &[u8], w: u32, h: u32, tw: u32) -> Gray {
    let w = w.max(1) as usize;
    let h = h.max(1) as usize;
    let tw = (tw as usize).clamp(8, w.max(8));
    let th = ((h * tw) / w).clamp(8, h.max(8));
    let mut px = vec![0u8; tw * th];
    for y in 0..th {
        let sy = y * h / th;
        for x in 0..tw {
            let sx = x * w / tw;
            let i = (sy * w + sx) * 3;
            if i + 2 < rgb.len() {
                let r = rgb[i] as u16;
                let g = rgb[i + 1] as u16;
                let b = rgb[i + 2] as u16;
                px[y * tw + x] = ((r + g * 2 + b) / 4) as u8;
            }
        }
    }
    Gray { px, w: tw, h: th }
}

pub fn judge(gray: &Gray, blur_below: f32) -> (Gate, f32) {
    let (mean, std) = mean_std(&gray.px);
    if mean < 14.0 || mean > 245.0 || std < 8.0 {
        return (Gate::Blank, 0.0);
    }
    let var = laplacian_var(&gray.px, gray.w, gray.h);
    if var < blur_below {
        (Gate::Blur, var)
    } else {
        (Gate::Sharp, var)
    }
}

/// Mean absolute difference of two small grey frames. A still camera sits near zero.
pub fn changed(prev: &Gray, cur: &Gray) -> f32 {
    if prev.px.len() != cur.px.len() || cur.px.is_empty() {
        return 255.0;
    }
    let mut s = 0u64;
    for (a, b) in prev.px.iter().zip(cur.px.iter()) {
        s += u64::from(a.abs_diff(*b));
    }
    s as f32 / cur.px.len() as f32
}

/// Motion inside a source-pixel box, measured on the small grey image.
pub fn motion_box(prev: &[u8], cur: &[u8], gw: usize, gh: usize, xyxy: [f32; 4], src_w: f32, src_h: f32) -> f32 {
    if prev.len() != cur.len() || gw == 0 || gh == 0 || src_w < 1.0 || src_h < 1.0 {
        return 0.0;
    }
    let x0 = ((xyxy[0] / src_w) * gw as f32).floor().clamp(0.0, gw as f32 - 1.0) as usize;
    let y0 = ((xyxy[1] / src_h) * gh as f32).floor().clamp(0.0, gh as f32 - 1.0) as usize;
    let x1 = ((xyxy[2] / src_w) * gw as f32).ceil().clamp(x0 as f32 + 1.0, gw as f32) as usize;
    let y1 = ((xyxy[3] / src_h) * gh as f32).ceil().clamp(y0 as f32 + 1.0, gh as f32) as usize;
    let mut s = 0u64;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * gw + x;
            if i < cur.len() {
                s += prev[i].abs_diff(cur[i]) as u64;
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { s as f32 / n as f32 }
}

fn mean_std(px: &[u8]) -> (f32, f32) {
    if px.is_empty() {
        return (0.0, 0.0);
    }
    let n = px.len() as f32;
    let sum: u64 = px.iter().map(|p| *p as u64).sum();
    let mean = sum as f32 / n;
    let mut v = 0.0;
    for p in px {
        let d = *p as f32 - mean;
        v += d * d;
    }
    (mean, (v / n).sqrt())
}

fn laplacian_var(px: &[u8], w: usize, h: usize) -> f32 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut sum2 = 0.0;
    let mut n = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let v = px[i - w] as f32 + px[i + w] as f32 + px[i - 1] as f32 + px[i + 1] as f32
                - 4.0 * px[i] as f32;
            sum += v;
            sum2 += v * v;
            n += 1.0;
        }
    }
    if n == 0.0 {
        return 0.0;
    }
    let mean = sum / n;
    sum2 / n - mean * mean
}

pub fn color_name(rgb: &[u8], w: u32, h: u32, xyxy: [f32; 4]) -> String {
    let w = w as i32;
    let h = h as i32;
    let x1 = xyxy[0].max(0.0) as i32;
    let y1 = xyxy[1].max(0.0) as i32;
    let x2 = (xyxy[2] as i32).min(w);
    let y2 = (xyxy[3] as i32).min(h);
    if x2 - x1 < 2 || y2 - y1 < 2 {
        return "gray".into();
    }
    let cx1 = x1 + (x2 - x1) / 4;
    let cx2 = x2 - (x2 - x1) / 4;
    let cy1 = y1 + (y2 - y1) / 4;
    let cy2 = y2 - (y2 - y1) / 4;
    let mut rs = Vec::new();
    let mut gs = Vec::new();
    let mut bs = Vec::new();
    let step_x = ((cx2 - cx1) / 8).max(1);
    let step_y = ((cy2 - cy1) / 8).max(1);
    let mut y = cy1;
    while y < cy2.max(cy1 + 1) {
        let mut x = cx1;
        while x < cx2.max(cx1 + 1) {
            let i = ((y as u32 * w as u32 + x as u32) * 3) as usize;
            if i + 2 < rgb.len() {
                rs.push(rgb[i]);
                gs.push(rgb[i + 1]);
                bs.push(rgb[i + 2]);
            }
            x += step_x;
        }
        y += step_y;
    }
    if rs.is_empty() {
        return "gray".into();
    }
    rs.sort_unstable();
    gs.sort_unstable();
    bs.sort_unstable();
    let mid = rs.len() / 2;
    let r = rs[mid] as f32 / 255.0;
    let g = gs[mid] as f32 / 255.0;
    let b = bs[mid] as f32 / 255.0;
    let (hue, sat, val) = hsv(r, g, b);
    if val < 0.18 {
        return "black".into();
    }
    if sat < 0.16 {
        return if val > 0.75 { "white" } else { "gray" }.into();
    }
    if hue < 25.0 && val < 0.45 {
        return "brown".into();
    }
    if hue < 15.0 || hue >= 345.0 {
        "red"
    } else if hue < 40.0 {
        "orange"
    } else if hue < 70.0 {
        "yellow"
    } else if hue < 170.0 {
        "green"
    } else if hue < 255.0 {
        "blue"
    } else {
        "purple"
    }
    .into()
}

fn hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let s = if mx == 0.0 { 0.0 } else { d / mx };
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, mx)
}

// ---- Motion gate and detector regions (the Frigate / DeepStream approach) ----

/// Box-averaged grey image about `tw` wide. Unlike `shrink_gray` (nearest neighbour, fast, aliases), averaging keeps
/// sensor noise down so a small pixel-difference threshold stays quiet on a still scene.
pub fn shrink_gray_box(rgb: &[u8], w: u32, h: u32, tw: u32) -> Gray {
    let w = w.max(1) as usize;
    let h = h.max(1) as usize;
    let tw = (tw as usize).clamp(8, w.max(8));
    let th = ((h * tw) / w).clamp(8, h.max(8));
    let mut px = vec![0u8; tw * th];
    if rgb.len() < w * h * 3 {
        return Gray { px, w: tw, h: th };
    }
    for y in 0..th {
        let (y0, y1) = (y * h / th, ((y + 1) * h / th).max(y * h / th + 1).min(h));
        for x in 0..tw {
            let (x0, x1) = (x * w / tw, ((x + 1) * w / tw).max(x * w / tw + 1).min(w));
            let mut sum = 0u32;
            for sy in y0..y1 {
                let row = &rgb[(sy * w + x0) * 3..(sy * w + x1) * 3];
                for p in row.chunks_exact(3) {
                    sum += p[0] as u32 + 2 * p[1] as u32 + p[2] as u32;
                }
            }
            px[y * tw + x] = (sum / (4 * ((y1 - y0) * (x1 - x0)) as u32)) as u8;
        }
    }
    Gray { px, w: tw, h: th }
}

/// Motion found on one frame. `boxes` are normalised [x0, y0, x1, y1].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Motion {
    pub boxes: Vec<[f32; 4]>,
    /// Most of the picture changed (camera moved, scene switched, first frame): the background was reset and the
    /// scheduler must run a full-picture pass.
    pub cut: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MotionParams {
    /// Grey levels a pixel must differ from the background (after removing a global brightness shift).
    pub threshold: f32,
    /// Smallest blob, as a fraction of the picture.
    pub min_area: f32,
}

impl MotionParams {
    /// sensitivity 0 -> 40 grey levels, 1 -> 8 grey levels.
    pub fn new(sensitivity: f32, min_area: f32) -> Self {
        MotionParams { threshold: 40.0 - 32.0 * sensitivity.clamp(0.0, 1.0), min_area }
    }
}

/// Slowly updated running-average background. Slow lighting drift is absorbed by the average and by removing the
/// median brightness shift, so only real local change shows up. Feed it EVERY decoded frame, not only sampled ones.
#[derive(Default)]
pub struct MotionModel {
    bg: Vec<f32>,
    w: usize,
    h: usize,
}

const BG_ALPHA: f32 = 0.05;

impl MotionModel {
    pub fn update(&mut self, g: &Gray, p: &MotionParams) -> Motion {
        let n = g.px.len();
        if self.bg.len() != n || self.w != g.w || n == 0 {
            self.bg = g.px.iter().map(|v| *v as f32).collect();
            self.w = g.w;
            self.h = g.h;
            return Motion { boxes: Vec::new(), cut: true };
        }
        // Median of (frame - background): a local mover covers < half the picture, so it does not move the median.
        let mut hist = [0u32; 511];
        for (c, b) in g.px.iter().zip(&self.bg) {
            hist[((*c as f32 - b).round() as i32 + 255).clamp(0, 510) as usize] += 1;
        }
        let (mut acc, mut median) = (0u32, 0i32);
        for (i, k) in hist.iter().enumerate() {
            acc += k;
            if acc * 2 >= n as u32 {
                median = i as i32 - 255;
                break;
            }
        }
        let mut mask: Vec<bool> = g.px.iter().zip(&self.bg).map(|(c, b)| (*c as f32 - b - median as f32).abs() > p.threshold).collect();
        let active = mask.iter().filter(|m| **m).count();
        for (b, c) in self.bg.iter_mut().zip(&g.px) {
            *b += BG_ALPHA * (*c as f32 - *b);
        }
        if active * 5 > n * 2 {
            self.bg = g.px.iter().map(|v| *v as f32).collect();
            return Motion { boxes: Vec::new(), cut: true };
        }
        mask = dilate(&mask, g.w, g.h);
        let min_px = ((p.min_area * n as f32).ceil() as usize).max(2);
        Motion { boxes: components(&mask, g.w, g.h, min_px), cut: false }
    }
}

fn dilate(m: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut out = vec![false; m.len()];
    for y in 0..h {
        for x in 0..w {
            if m[y * w + x] {
                for yy in y.saturating_sub(1)..(y + 2).min(h) {
                    for xx in x.saturating_sub(1)..(x + 2).min(w) {
                        out[yy * w + xx] = true;
                    }
                }
            }
        }
    }
    out
}

/// 4-connected blobs of at least `min_px` pixels, as normalised boxes, in scan order.
fn components(m: &[bool], w: usize, h: usize, min_px: usize) -> Vec<[f32; 4]> {
    let mut seen = vec![false; m.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..m.len() {
        if !m[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1, mut area) = (w, h, 0usize, 0usize, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            area += 1;
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            let mut push = |j: usize| {
                if m[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 { push(i - 1); }
            if x + 1 < w { push(i + 1); }
            if y > 0 { push(i - w); }
            if y + 1 < h { push(i + w); }
        }
        if area >= min_px {
            out.push([x0 as f32 / w as f32, y0 as f32 / h as f32, (x1 + 1) as f32 / w as f32, (y1 + 1) as f32 / h as f32]);
        }
    }
    out
}

/// Smallest detector crop, as a fraction of the picture's shorter side (a tiny crop would be blown up to model size).
const MIN_REGION: f32 = 0.3;
/// Crops this close to the shorter side (or larger) are not worth cutting: run the whole picture.
const FULL_SIDE: f32 = 0.9;
/// Several crops covering this share of the picture cost more than one full pass.
const FULL_AREA: f32 = 0.5;

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn bbox(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Square crop around a pixel box: side = longest side x (1 + 2 margin), at least `MIN_REGION` of the short side,
/// centred, then slid (never shrunk) to stay inside the picture. `None` = too large for a crop; use the full picture.
pub fn square_region(b: [f32; 4], margin: f32, fw: f32, fh: f32) -> Option<[f32; 4]> {
    let short = fw.min(fh);
    let side = ((b[2] - b[0]).max(b[3] - b[1]) * (1.0 + 2.0 * margin)).max(short * MIN_REGION);
    if side >= short * FULL_SIDE {
        return None;
    }
    let x0 = ((b[0] + b[2]) * 0.5 - side * 0.5).clamp(0.0, fw - side);
    let y0 = ((b[1] + b[3]) * 0.5 - side * 0.5).clamp(0.0, fh - side);
    Some([x0, y0, x0 + side, y0 + side])
}

/// Group pixel boxes (motion blobs plus predicted track boxes) into square crops. Boxes whose squares overlap are
/// merged and re-squared from the ORIGINAL boxes, so margin does not compound. `None` = run the full picture instead
/// (a crop too large, more than `max_regions` crops, or crops covering more than half the picture).
pub fn regions_for(boxes: &[[f32; 4]], margin: f32, fw: f32, fh: f32, max_regions: usize) -> Option<Vec<[f32; 4]>> {
    let mut groups: Vec<[f32; 4]> = boxes.to_vec();
    let squares = loop {
        let sq: Vec<[f32; 4]> = groups.iter().map(|g| square_region(*g, margin, fw, fh)).collect::<Option<_>>()?;
        let pair = (0..sq.len()).flat_map(|i| (i + 1..sq.len()).map(move |j| (i, j))).find(|(i, j)| overlaps(sq[*i], sq[*j]));
        match pair {
            Some((i, j)) => {
                let u = bbox(groups[i], groups[j]);
                groups.remove(j);
                groups[i] = u;
            }
            None => break sq,
        }
    };
    let area: f32 = squares.iter().map(|r| (r[2] - r[0]) * (r[3] - r[1])).sum();
    if squares.is_empty() || squares.len() > max_regions || area >= FULL_AREA * fw * fh {
        return None;
    }
    Some(squares)
}

/// A rectangle cut out of a decoded frame, remembering where it came from.
pub struct Crop {
    pub rgb: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub x0: u32,
    pub y0: u32,
}

pub fn crop_rgb(rgb: &[u8], w: u32, h: u32, r: [f32; 4]) -> Crop {
    let x0 = (r[0].floor().max(0.0) as u32).min(w.saturating_sub(1));
    let y0 = (r[1].floor().max(0.0) as u32).min(h.saturating_sub(1));
    let x1 = (r[2].ceil() as u32).clamp(x0 + 1, w);
    let y1 = (r[3].ceil() as u32).clamp(y0 + 1, h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity((cw * ch * 3) as usize);
    for y in y0..y1 {
        let s = ((y * w + x0) * 3) as usize;
        out.extend_from_slice(&rgb[s..s + (cw * 3) as usize]);
    }
    Crop { rgb: out, w: cw, h: ch, x0, y0 }
}

impl Crop {
    /// Detector box in crop pixels -> frame pixels.
    pub fn to_frame(&self, b: [f32; 4]) -> [f32; 4] {
        let (dx, dy) = (self.x0 as f32, self.y0 as f32);
        [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy]
    }
}

/// Class-aware non-maximum suppression across crops that overlap: the higher confidence wins.
pub fn nms(mut dets: Vec<crate::watch::yolo::Det>, iou_thr: f32) -> Vec<crate::watch::yolo::Det> {
    dets.sort_by(|a, b| b.conf.total_cmp(&a.conf));
    let mut kept: Vec<crate::watch::yolo::Det> = Vec::with_capacity(dets.len());
    for d in dets {
        if !kept.iter().any(|k| k.class == d.class && crate::watch::track::iou(k.xyxy, d.xyxy) >= iou_thr) {
            kept.push(d);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_is_blank_and_checker_is_sharp() {
        let black = vec![0u8; 32 * 32 * 3];
        let g = shrink_gray(&black, 32, 32, 32);
        assert_eq!(judge(&g, 40.0).0, Gate::Blank);

        let mut sharp = vec![0u8; 32 * 32 * 3];
        for y in 0..32 {
            for x in 0..32 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                let i = (y * 32 + x) * 3;
                sharp[i] = v;
                sharp[i + 1] = v;
                sharp[i + 2] = v;
            }
        }
        let g = shrink_gray(&sharp, 32, 32, 32);
        assert_eq!(judge(&g, 40.0).0, Gate::Sharp);
    }

    #[test]
    fn flat_gray_is_blank() {
        let img = vec![140u8; 24 * 24 * 3];
        let g = shrink_gray(&img, 24, 24, 16);
        assert_eq!(judge(&g, 40.0).0, Gate::Blank);
    }

    #[test]
    fn names_a_red_square() {
        let mut img = vec![0u8; 8 * 8 * 3];
        for px in img.chunks_mut(3) {
            px[0] = 200;
            px[1] = 20;
            px[2] = 20;
        }
        assert_eq!(color_name(&img, 8, 8, [1.0, 1.0, 7.0, 7.0]), "red");
    }

    #[test]
    fn a_still_frame_barely_changes() {
        let g = Gray { px: vec![10, 12, 10, 12], w: 2, h: 2 };
        assert!(changed(&g, &g) < 0.1);
        let moved = Gray { px: vec![40, 42, 40, 42], w: 2, h: 2 };
        assert!(changed(&g, &moved) > 20.0);
    }

    // ---- motion / regions ----
    fn frame(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Gray {
        Gray { px: (0..w * h).map(|i| f(i % w, i / w)).collect(), w, h }
    }
    fn textured(x: usize, y: usize) -> u8 {
        (60 + ((x * 7 + y * 13) % 40)) as u8
    }
    const P: fn() -> MotionParams = || MotionParams::new(0.6, 0.001);

    #[test]
    fn moving_square_makes_one_box_near_it_and_static_makes_none() {
        let mut m = MotionModel::default();
        let bgf = frame(128, 72, textured);
        assert!(m.update(&bgf, &P()).cut, "first frame resets");
        for _ in 0..5 {
            let r = m.update(&bgf, &P());
            assert!(r.boxes.is_empty() && !r.cut, "static scene -> no motion");
        }
        let with = frame(128, 72, |x, y| if (40..56).contains(&x) && (20..36).contains(&y) { 230 } else { textured(x, y) });
        let r = m.update(&with, &P());
        assert_eq!(r.boxes.len(), 1, "{:?}", r.boxes);
        let b = r.boxes[0];
        let (cx, cy) = ((b[0] + b[2]) / 2.0 * 128.0, (b[1] + b[3]) / 2.0 * 72.0);
        assert!((cx - 48.0).abs() < 3.0 && (cy - 28.0).abs() < 3.0, "{b:?}");
    }

    #[test]
    fn slow_lighting_ramp_makes_no_motion() {
        let mut m = MotionModel::default();
        m.update(&frame(128, 72, textured), &P());
        for k in 1..80usize {
            let r = m.update(&frame(128, 72, |x, y| textured(x, y).saturating_add((k / 2) as u8)), &P());
            assert!(r.boxes.is_empty() && !r.cut, "frame {k}: {:?}", r.boxes);
        }
    }

    #[test]
    fn scene_cut_resets_the_background() {
        let mut m = MotionModel::default();
        m.update(&frame(64, 36, |_, _| 50), &P());
        let r = m.update(&frame(64, 36, |x, _| if x % 2 == 0 { 220 } else { 10 }), &P());
        assert!(r.cut && r.boxes.is_empty());
        assert!(!m.update(&frame(64, 36, |x, _| if x % 2 == 0 { 220 } else { 10 }), &P()).cut, "new scene is the background now");
    }

    #[test]
    fn box_gray_averages_a_block() {
        let mut rgb = vec![0u8; 16 * 16 * 3];
        for (i, v) in rgb.iter_mut().enumerate() { *v = if (i / 3) % 2 == 0 { 200 } else { 0 }; }
        assert_eq!(shrink_gray_box(&rgb, 16, 16, 8).w, 8);
        assert_eq!(shrink_gray_box(&rgb, 16, 16, 8).px[0], 100);
    }

    #[test]
    fn regions_are_squares_inside_the_frame_with_margin() {
        // 1280x720, a 100x200 person at the far left edge.
        let r = square_region([0.0, 300.0, 100.0, 500.0], 0.25, 1280.0, 720.0).unwrap();
        assert!(((r[2] - r[0]) - (r[3] - r[1])).abs() < 1e-3, "square");
        assert!((r[2] - r[0]) >= 300.0 - 1e-3, "200 * 1.5");
        assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[2] <= 1280.0 && r[3] <= 720.0);
        assert_eq!(r[0], 0.0, "slid to the edge, not shrunk");
        // a tiny blob still gets a usable crop; a huge one asks for the full frame
        let t = square_region([600.0, 300.0, 610.0, 310.0], 0.25, 1280.0, 720.0).unwrap();
        assert!((t[2] - t[0] - 216.0).abs() < 1e-3);
        assert!(square_region([0.0, 0.0, 700.0, 400.0], 0.25, 1280.0, 720.0).is_none());
    }

    #[test]
    fn nearby_boxes_merge_and_far_boxes_split_and_overflow_goes_full() {
        let near = [[100.0, 100.0, 140.0, 180.0], [160.0, 110.0, 200.0, 190.0]];
        assert_eq!(regions_for(&near, 0.25, 1280.0, 720.0, 4).unwrap().len(), 1);
        let far = [[50.0, 50.0, 90.0, 120.0], [1100.0, 500.0, 1150.0, 600.0]];
        assert_eq!(regions_for(&far, 0.25, 1280.0, 720.0, 4).unwrap().len(), 2);
        assert!(regions_for(&far, 0.25, 1280.0, 720.0, 1).is_none(), "more crops than max_regions");
        assert!(regions_for(&[], 0.25, 1280.0, 720.0, 4).is_none());
    }

    #[test]
    fn crop_to_frame_mapping_round_trips() {
        let (w, h) = (64u32, 48u32);
        let rgb: Vec<u8> = (0..(w * h * 3) as usize).map(|i| (i / 3 % 251) as u8).collect();
        let c = crop_rgb(&rgb, w, h, [10.4, 20.0, 40.0, 44.2]);
        assert_eq!((c.x0, c.y0, c.w, c.h), (10, 20, 30, 25));
        assert_eq!(c.rgb.len(), (c.w * c.h * 3) as usize);
        assert_eq!(c.rgb[0], rgb[((20 * w + 10) * 3) as usize]);
        // a detector box at crop (5,6)-(15,16) is frame (15,26)-(25,36); mapping back subtracts the origin
        let f = c.to_frame([5.0, 6.0, 15.0, 16.0]);
        assert_eq!(f, [15.0, 26.0, 25.0, 36.0]);
        assert_eq!([f[0] - c.x0 as f32, f[1] - c.y0 as f32, f[2] - c.x0 as f32, f[3] - c.y0 as f32], [5.0, 6.0, 15.0, 16.0]);
    }

    #[test]
    fn nms_is_class_aware_across_regions() {
        use crate::watch::yolo::Det;
        let d = |c: &str, x: f32, conf: f32| Det { xyxy: [x, 0.0, x + 10.0, 10.0], class: c.into(), conf };
        let kept = nms(vec![d("person", 0.0, 0.5), d("person", 1.0, 0.9), d("car", 0.0, 0.4), d("person", 30.0, 0.3)], 0.6);
        assert_eq!(kept.len(), 3);
        assert!(kept.iter().any(|k| k.class == "person" && k.conf == 0.9));
        assert!(!kept.iter().any(|k| k.conf == 0.5), "the weaker duplicate goes");
        assert!(kept.iter().any(|k| k.class == "car"), "different class survives the overlap");
    }
}
