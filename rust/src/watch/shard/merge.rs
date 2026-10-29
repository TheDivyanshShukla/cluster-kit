//! k-way merge of per-node pages and the opaque federated cursor.
//!
//! Every node returns rows already ordered by (media_id, media_time_us, id). The merge keeps that order, so a
//! cursor is just "the key of the last row consumed from each node" and pages are stable.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};

use super::Row;
use crate::watch::domain::ApiError;

pub type Key = (String, i64, String);

/// Position of every node in a traversal. Encoded as hex JSON so clients treat it as opaque.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    /// Vector snapshot: node -> commit sequence this traversal reads at or below.
    #[serde(default)]
    pub v: BTreeMap<String, u64>,
    /// node -> key of the last row consumed (SQLite cursor format).
    #[serde(default)]
    pub p: BTreeMap<String, String>,
    /// Nodes with nothing left.
    #[serde(default)]
    pub d: BTreeSet<String>,
    /// Nodes given up on in this traversal (down or timed out); the response is flagged partial.
    #[serde(default)]
    pub x: BTreeSet<String>,
}

impl Cursor {
    pub fn encode(&self) -> String {
        serde_json::to_vec(self).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(s: &str) -> Result<Cursor, ApiError> {
        let bad = || ApiError::new("invalid_request", "invalid cursor");
        if s.len() % 2 != 0 || !s.is_ascii() {
            return Err(bad());
        }
        let bytes: Result<Vec<u8>, _> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16)).collect();
        serde_json::from_slice(&bytes.map_err(|_| bad())?).map_err(|_| bad())
    }
}

/// Same format as the SQLite store's keyset cursor (`media \x1f time \x1f id`, hex), so a node accepts it as `after`.
pub fn key_cursor(k: &Key) -> String {
    format!("{}\u{1f}{}\u{1f}{}", k.0, k.1, k.2).bytes().map(|b| format!("{b:02x}")).collect()
}

pub struct Merged {
    /// Emitted rows in global order, tagged with the node they came from.
    pub rows: Vec<(String, Row)>,
    /// Key of the last row consumed per node (duplicates dropped from the output still count as consumed).
    pub consumed: BTreeMap<String, Key>,
    /// Nodes that still have fetched-but-unconsumed rows.
    pub leftover: BTreeSet<String>,
}

/// Merge sorted per-node lists into at most `limit` rows. Rows with an equal key on two nodes (the same
/// observation replayed on a second computer after a failover) are emitted once.
pub fn kmerge(lists: Vec<(String, Vec<Row>)>, limit: usize) -> Merged {
    let mut its: Vec<(String, std::vec::IntoIter<Row>)> = lists.into_iter().map(|(n, v)| (n, v.into_iter())).collect();
    let mut heap: BinaryHeap<Reverse<(Key, usize)>> = BinaryHeap::new();
    let mut heads: Vec<Option<Row>> = vec![None; its.len()];
    for (i, (_, it)) in its.iter_mut().enumerate() {
        if let Some(r) = it.next() {
            heap.push(Reverse((r.k.clone(), i)));
            heads[i] = Some(r);
        }
    }
    let (mut rows, mut consumed): (Vec<(String, Row)>, BTreeMap<String, Key>) = (Vec::new(), BTreeMap::new());
    let mut last: Option<Key> = None;
    while let Some(Reverse((k, i))) = heap.peek().cloned() {
        // Stop once full, but finish any duplicates of the last emitted key so they are not served again.
        if rows.len() >= limit && last.as_ref() != Some(&k) {
            break;
        }
        heap.pop();
        let row = heads[i].take().expect("head row");
        consumed.insert(its[i].0.clone(), k.clone());
        if last.as_ref() != Some(&k) {
            last = Some(k);
            rows.push((its[i].0.clone(), row));
        }
        if let Some(r) = its[i].1.next() {
            heap.push(Reverse((r.k.clone(), i)));
            heads[i] = Some(r);
        }
    }
    let leftover = heads.iter().enumerate().filter(|(_, h)| h.is_some()).map(|(i, _)| its[i].0.clone()).collect();
    Merged { rows, consumed, leftover }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(m: &str, t: i64, id: &str) -> Row {
        Row { k: (m.into(), t, id.into()), v: serde_json::Value::Null }
    }

    #[test]
    fn merges_in_key_order_and_drops_duplicates() {
        let a = vec![r("m1", 1, "a"), r("m1", 3, "c"), r("m2", 1, "x")];
        let b = vec![r("m1", 2, "b"), r("m1", 3, "c"), r("m3", 0, "z")];
        let m = kmerge(vec![("A".into(), a), ("B".into(), b)], 100);
        let ids: Vec<_> = m.rows.iter().map(|(_, r)| r.k.2.clone()).collect();
        assert_eq!(ids, ["a", "b", "c", "x", "z"]);
        assert!(m.leftover.is_empty());
    }

    #[test]
    fn limit_leaves_the_rest_and_consumes_trailing_duplicates() {
        let a = vec![r("m", 1, "a"), r("m", 2, "b"), r("m", 4, "d")];
        let b = vec![r("m", 2, "b"), r("m", 3, "c")];
        let m = kmerge(vec![("A".into(), a), ("B".into(), b)], 2);
        let ids: Vec<_> = m.rows.iter().map(|(_, r)| r.k.2.clone()).collect();
        assert_eq!(ids, ["a", "b"]);
        // B's duplicate of "b" is consumed, so the next page does not repeat it.
        assert_eq!(m.consumed["B"].2, "b");
        assert_eq!(m.consumed["A"].2, "b");
        assert_eq!(m.leftover.len(), 2);
    }

    #[test]
    fn cursor_round_trips() {
        let mut c = Cursor::default();
        c.v.insert("n1".into(), 7);
        c.p.insert("n1".into(), key_cursor(&("m".into(), 5, "id".into())));
        c.d.insert("n2".into());
        assert_eq!(Cursor::decode(&c.encode()).unwrap(), c);
        assert!(Cursor::decode("zz").is_err());
    }
}
