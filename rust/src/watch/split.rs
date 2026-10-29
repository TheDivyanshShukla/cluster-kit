//! Each camera stays on one PC. Tracking and a "fewer than" rule need the same machine
//! to see the camera every frame.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub cores: usize,
    pub local: bool,
}

pub fn assign(nodes: &[Node], cameras: &[String], solo: bool) -> BTreeMap<String, String> {
    if nodes.is_empty() || cameras.is_empty() {
        return BTreeMap::new();
    }
    if solo {
        let id = nodes.iter().find(|n| n.local).unwrap_or(&nodes[0]).id.clone();
        return cameras.iter().map(|c| (c.clone(), id.clone())).collect();
    }
    let mut load: BTreeMap<&str, f64> = nodes.iter().map(|n| (n.id.as_str(), 0.0)).collect();
    let mut out = BTreeMap::new();
    for cam in cameras {
        let id = nodes
            .iter()
            .min_by(|a, b| {
                let la = load[a.id.as_str()] / a.cores.max(1) as f64;
                let lb = load[b.id.as_str()] / b.cores.max(1) as f64;
                la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id))
            })
            .map(|n| n.id.as_str())
            .unwrap_or("");
        *load.get_mut(id).unwrap() += 1.0;
        out.insert(cam.clone(), id.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_keeps_every_camera_on_the_head() {
        let nodes = vec![
            Node { id: "head".into(), cores: 6, local: true },
            Node { id: "other".into(), cores: 6, local: false },
        ];
        let cams: Vec<String> = (1..=4).map(|i| format!("c{i}")).collect();
        let m = assign(&nodes, &cams, true);
        assert!(m.values().all(|id| id == "head"));
    }

    #[test]
    fn cluster_splits_by_cores() {
        let nodes = vec![
            Node { id: "a".into(), cores: 2, local: true },
            Node { id: "b".into(), cores: 6, local: false },
        ];
        let cams: Vec<String> = (1..=4).map(|i| format!("c{i}")).collect();
        let m = assign(&nodes, &cams, false);
        let nb = m.values().filter(|id| *id == "b").count();
        assert!(nb >= 3, "{m:?}");
    }
}
