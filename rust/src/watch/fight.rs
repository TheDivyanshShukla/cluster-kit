//! A fight is not a class in the everyday object model. Two people close together,
//! with fast motion between them, counts as one `fight` for the rule line `seen("fight")`.
//! The rule's hold is what confirms it. This function only answers the current frame.

use crate::watch::track::iou;

pub fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    if iou(a, b) > 0.02 {
        return true;
    }
    let (ax, ay) = ((a[0] + a[2]) * 0.5, (a[1] + a[3]) * 0.5);
    let (bx, by) = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
    let dist = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt();
    let size = (((a[2] - a[0]) + (b[2] - b[0])) * 0.5).max(8.0);
    dist < size * 0.85
}

pub fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// `pair_motion(i, j)` is the motion inside the union of those two boxes.
/// 22 is a mean absolute difference on the small grey image: walking past is quieter.
pub fn fight_frame(people: &[[f32; 4]], mut pair_motion: impl FnMut(usize, usize) -> f32) -> bool {
    for i in 0..people.len() {
        for j in (i + 1)..people.len() {
            if close(people[i], people[j]) && pair_motion(i, j) >= 22.0 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_fast_people_are_a_fight_frame() {
        let people = [[0.0, 0.0, 40.0, 80.0], [20.0, 10.0, 60.0, 90.0]];
        assert!(fight_frame(&people, |_, _| 30.0));
        assert!(!fight_frame(&people, |_, _| 4.0));
    }

    #[test]
    fn far_people_are_not() {
        let people = [[0.0, 0.0, 20.0, 40.0], [400.0, 400.0, 440.0, 480.0]];
        assert!(!fight_frame(&people, |_, _| 40.0));
    }
}
