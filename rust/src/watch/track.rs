pub fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);
    let w = (x2 - x1).max(0.0);
    let h = (y2 - y1).max(0.0);
    let inter = w * h;
    if inter <= 0.0 {
        return 0.0;
    }
    let area = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0) + (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0) - inter;
    if area <= 0.0 { 0.0 } else { inter / area }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_boxes_overlap_fully() {
        let b = [0.0, 0.0, 10.0, 10.0];
        assert!((iou(b, b) - 1.0).abs() < 1e-4);
    }
}
