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

struct Track {
    n: u64,
    class: String,
    xy: [f32; 4],
    /// Per-frame centre velocity (dx, dy), used to predict the next box through short occlusion.
    vel: [f32; 2],
    age: u32,
}

/// Short-term IoU association, one tracker per (source, media, run). IDs are not identities:
/// they expire after `max_age` missed inference frames and mean nothing across cameras.
/// Association is greedy by IoU against the velocity-predicted box, class-matched, one-to-one.
pub struct Tracker {
    scope: String,
    next: u64,
    tracks: Vec<Track>,
    pub max_age: u32,
    pub min_iou: f32,
}

impl Default for Tracker {
    fn default() -> Self {
        // Legacy: numeric ids seeded from the clock so restarts do not reuse them.
        Tracker { scope: String::new(), next: 0, tracks: Vec::new(), max_age: 5, min_iou: 0.3 }
    }
}

fn shifted(b: [f32; 4], v: [f32; 2], k: f32) -> [f32; 4] {
    [b[0] + v[0] * k, b[1] + v[1] * k, b[2] + v[0] * k, b[3] + v[1] * k]
}

impl Tracker {
    /// Deterministic tracker: ids are `"<scope>:t<n>"` from 1. Scope = `source/media/run`.
    pub fn scoped(scope: &str) -> Self {
        Tracker { scope: scope.to_string(), next: 0, tracks: Vec::new(), max_age: 5, min_iou: 0.3 }
    }

    /// Scoped string ids, same order as `detections`.
    pub fn update_scoped(&mut self, detections: &[(String, [f32; 4])]) -> Vec<String> {
        let ids = self.step(detections);
        ids.into_iter().map(|n| format!("{}:t{}", self.scope, n)).collect()
    }

    /// Legacy numeric ids.
    pub fn update(&mut self, detections: &[(String, [f32; 4])]) -> Vec<u64> {
        if self.next == 0 && self.scope.is_empty() {
            self.next = (crate::watch::now_secs() * 1_000_000.0) as u64;
        }
        self.step(detections)
    }

    /// Tracks still alive: (number, class, box predicted for the NEXT pass, passes missed so far).
    /// Constant-velocity: the last centre velocity, per pass, carried forward `age + 1` passes.
    pub fn live(&self) -> Vec<(u64, String, [f32; 4], u32)> {
        self.tracks.iter().map(|t| (t.n, t.class.clone(), shifted(t.xy, t.vel, t.age as f32 + 1.0), t.age)).collect()
    }

    /// Per-pass centre velocity of a live track, for drawing it between detector passes.
    pub fn velocity(&self, n: u64) -> Option<[f32; 2]> {
        self.tracks.iter().find(|t| t.n == n).map(|t| t.vel)
    }

    fn step(&mut self, detections: &[(String, [f32; 4])]) -> Vec<u64> {
        for t in &mut self.tracks {
            t.age += 1;
        }
        let mut ids = vec![0u64; detections.len()];
        let mut pairs = Vec::new();
        for (di, (class, xy)) in detections.iter().enumerate() {
            for (ti, t) in self.tracks.iter().enumerate() {
                if t.class != *class {
                    continue;
                }
                let predicted = shifted(t.xy, t.vel, t.age as f32);
                let overlap = iou(*xy, predicted).max(iou(*xy, t.xy));
                if overlap >= self.min_iou {
                    pairs.push((overlap, di, ti));
                }
            }
        }
        // Ties break by (detection, track) index so output is deterministic.
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut used = vec![false; self.tracks.len()];
        for (_, di, ti) in pairs {
            if ids[di] != 0 || used[ti] {
                continue;
            }
            used[ti] = true;
            let t = &mut self.tracks[ti];
            let (ox, oy) = ((t.xy[0] + t.xy[2]) * 0.5, (t.xy[1] + t.xy[3]) * 0.5);
            let nb = detections[di].1;
            let (nx, ny) = ((nb[0] + nb[2]) * 0.5, (nb[1] + nb[3]) * 0.5);
            let k = t.age.max(1) as f32;
            t.vel = [(nx - ox) / k, (ny - oy) / k];
            t.xy = nb;
            t.age = 0;
            ids[di] = t.n;
        }
        for (i, (class, xy)) in detections.iter().enumerate() {
            if ids[i] == 0 {
                self.next += 1;
                ids[i] = self.next;
                self.tracks.push(Track { n: self.next, class: class.clone(), xy: *xy, vel: [0.0, 0.0], age: 0 });
            }
        }
        let max_age = self.max_age;
        self.tracks.retain(|t| t.age <= max_age);
        ids
    }
}

/// A phone whose centre lies inside a person box is spatially associated only.
/// This cannot establish that a hand is holding the phone.
pub fn near_person(phone: [f32; 4], persons: &[(u64, [f32; 4])]) -> Option<u64> {
    let (x, y) = ((phone[0] + phone[2]) / 2.0, (phone[1] + phone[3]) / 2.0);
    persons.iter().filter(|(_, p)| x >= p[0] && x <= p[2] && y >= p[1] && y <= p[3])
        .min_by(|a, b| {
            let area = |p: [f32; 4]| (p[2]-p[0]).max(0.0)*(p[3]-p[1]).max(0.0);
            area(a.1).total_cmp(&area(b.1))
        }).map(|p| p.0)
}

#[cfg(test)]
mod association_tests {
    use super::*;
    #[test]
    fn association_is_one_to_one_and_class_scoped() {
        let mut t = Tracker::default();
        let first = t.update(&[("person".into(), [0., 0., 10., 10.])]);
        let second = t.update(&[("person".into(), [1., 0., 11., 10.]), ("person".into(), [0., 0., 10., 10.]), ("cell phone".into(), [0., 0., 10., 10.])]);
        assert_eq!(second.iter().filter(|id| **id == first[0]).count(), 1);
        assert_ne!(second[2], first[0]);
    }
    #[test]
    fn scoped_ids_are_deterministic_and_survive_short_occlusion() {
        let mut t = Tracker::scoped("s1/m1/j1");
        let a = t.update_scoped(&[("person".into(), [0., 0., 10., 10.])]);
        assert_eq!(a, vec!["s1/m1/j1:t1".to_string()]);
        assert!(t.update_scoped(&[]).is_empty());
        assert!(t.update_scoped(&[]).is_empty());
        let b = t.update_scoped(&[("person".into(), [1., 0., 11., 10.])]);
        assert_eq!(b, a, "same person after 2 missed frames keeps its id");
        let mut u = Tracker::scoped("s1/m1/j1");
        assert_eq!(u.update_scoped(&[("person".into(), [0., 0., 10., 10.])]), a);
    }
    #[test]
    fn velocity_prediction_follows_a_fast_mover() {
        let mut t = Tracker::scoped("x");
        let mut id = String::new();
        for i in 0..4 {
            let x = i as f32 * 4.0; // box width 10, moves 4px per frame, then a 2-frame gap
            id = t.update_scoped(&[("car".into(), [x, 0., x + 10., 10.])])[0].clone();
        }
        t.update_scoped(&[]);
        t.update_scoped(&[]);
        let x = 12.0 + 4.0 * 3.0; // 24: unpredicted IoU with last box [12..22] is 0, prediction hits
        assert_eq!(t.update_scoped(&[("car".into(), [x, 0., x + 10., 10.])])[0], id);
    }
    #[test]
    fn live_tracks_predict_the_next_pass_and_expire_after_max_age() {
        let mut t = Tracker::scoped("x");
        t.update_scoped(&[("car".into(), [0., 0., 10., 10.])]);
        t.update_scoped(&[("car".into(), [4., 0., 14., 10.])]);
        let live = t.live();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].2, [8., 0., 18., 10.]);
        assert_eq!(t.velocity(live[0].0), Some([4., 0.]));
        for _ in 0..6 { t.update_scoped(&[]); }
        assert!(t.live().is_empty(), "gone after max_age passes");
    }
    #[test]
    fn expired_track_gets_a_new_id() {
        let mut t = Tracker::scoped("x");
        let a = t.update_scoped(&[("person".into(), [0., 0., 10., 10.])]);
        for _ in 0..7 { t.update_scoped(&[]); }
        assert_ne!(t.update_scoped(&[("person".into(), [0., 0., 10., 10.])]), a);
    }
    #[test]
    fn proximity_does_not_associate_an_outside_phone() {
        assert_eq!(near_person([20., 20., 21., 21.], &[(7, [0., 0., 10., 10.])]), None);
        assert_eq!(near_person([2., 2., 3., 3.], &[(7, [0., 0., 10., 10.])]), Some(7));
    }
}
