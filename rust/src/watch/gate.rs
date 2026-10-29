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
}
