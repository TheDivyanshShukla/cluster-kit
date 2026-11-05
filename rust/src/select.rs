//! Node selector language (ORG-04). Pure, no I/O. Owner: a2. Contract: keep the pub items.
//!
//! A selector is space-separated terms, all of which must hold (AND). Empty selector = every node.
//!   key=value   key!=value   key>=N  key<=N  key>N  key<N   (numeric compare when both sides are numbers)
//!   key         the attribute exists and is not "false"/"0"/""     !key   it is absent or "false"/"0"/""
//!   value may be quoted "two words"; key=a,b,c means any of a, b, c; key=lab-a* allows one trailing * (prefix)
//! Attribute keys (see `attrs`): node, host, addr, state, online, lab, x, y, pool (one key per pool: pool=NAME matches
//! membership), cores, threads, ram_gb, cpu, arch, os, class, virtual, and every free label (label keys are lower case).
//! Decisions: Ge/Le/Gt/Lt need a numeric value (parse error otherwise) and are false when the node's value is not a number;
//! Eq/Ne compare numerically when both sides are numbers, else case-insensitively; a missing key fails Eq and passes Ne.
//! Examples: `lab=A state=active cores>=8`   `pool=night !broken`   `class="Intel i5-10400F/12c/16G"`   `host=lab-a*`

use std::collections::BTreeMap;

use crate::jobs::NodeRow;

/// Attribute map of one node. Multi-valued keys (pool) are stored as a comma list and match any member.
pub type Attrs = BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Has,
    Not,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub key: String,
    pub op: Op,
    /// alternatives for Eq/Ne (key=a,b); one value for comparisons; empty for Has/Not
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selector {
    pub terms: Vec<Term>,
    /// the text it was parsed from (for display and storage)
    pub text: String,
}

/// Split on whitespace outside double quotes.
fn tokens(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut q) = (vec![], String::new(), false);
    for c in s.chars() {
        match c {
            '"' => {
                q = !q;
                cur.push(c);
            }
            c if c.is_whitespace() && !q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if q {
        return Err(format!("unclosed quote in `{}`", s.trim()));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// `a,"b c",d` -> ["a","b c","d"]
fn values_of(v: &str) -> Vec<String> {
    let (mut out, mut cur, mut q) = (vec![], String::new(), false);
    for c in v.chars() {
        match c {
            '"' => q = !q,
            ',' if !q => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Parse; a readable error names the bad term ("bad term `cores>>8`").
pub fn parse(s: &str) -> Result<Selector, String> {
    let mut terms = vec![];
    for tok in tokens(s)? {
        let bad = |why: &str| format!("bad term `{tok}`: {why}");
        let Some(p) = tok.find(['=', '!', '<', '>']) else {
            terms.push(Term { key: tok.to_lowercase(), op: Op::Has, values: vec![] });
            continue;
        };
        if p == 0 && tok.starts_with('!') && !tok[1..].contains(['=', '!', '<', '>']) {
            let key = tok[1..].to_lowercase();
            if key.is_empty() {
                return Err(bad("missing key"));
            }
            terms.push(Term { key, op: Op::Not, values: vec![] });
            continue;
        }
        let key = tok[..p].to_lowercase();
        if key.is_empty() || key.contains('"') {
            return Err(bad("missing key"));
        }
        let rest = &tok[p..];
        let (op, n) = match rest {
            r if r.starts_with("!=") => (Op::Ne, 2),
            r if r.starts_with(">=") => (Op::Ge, 2),
            r if r.starts_with("<=") => (Op::Le, 2),
            r if r.starts_with('=') => (Op::Eq, 1),
            r if r.starts_with('>') => (Op::Gt, 1),
            r if r.starts_with('<') => (Op::Lt, 1),
            _ => return Err(bad("unknown operator")),
        };
        let raw = &rest[n..];
        if raw.is_empty() {
            return Err(bad("missing value"));
        }
        if raw.starts_with(['=', '!', '<', '>']) {
            return Err(bad("unknown operator"));
        }
        let values = values_of(raw);
        if values.iter().any(|v| v.is_empty()) {
            return Err(bad("empty value"));
        }
        if !matches!(op, Op::Eq | Op::Ne) && (values.len() != 1 || num(&values[0]).is_none()) {
            return Err(bad("needs one number"));
        }
        terms.push(Term { key, op, values });
    }
    Ok(Selector { terms, text: s.trim().to_string() })
}

fn truthy(v: &str) -> bool {
    !matches!(v.trim().to_lowercase().as_str(), "" | "false" | "0")
}

/// Does attribute value `have` (a comma list for pool) equal pattern `want` (numeric, trailing-* prefix, or case-insensitive)?
fn eq1(key: &str, have: &str, want: &str) -> bool {
    let one = |h: &str| {
        if let (Some(a), Some(b)) = (num(h), num(want)) {
            return a == b;
        }
        let (h, w) = (h.to_lowercase(), want.to_lowercase());
        match w.strip_suffix('*') {
            Some(p) => h.starts_with(p),
            None => h == w,
        }
    };
    if key == "pool" {
        have.split(',').any(|h| one(h))
    } else {
        one(have)
    }
}

impl Term {
    fn matches(&self, a: &Attrs) -> bool {
        let v = a.get(&self.key);
        match &self.op {
            Op::Has => v.is_some_and(|v| truthy(v)),
            Op::Not => !v.is_some_and(|v| truthy(v)),
            Op::Eq => v.is_some_and(|v| self.values.iter().any(|w| eq1(&self.key, v, w))),
            Op::Ne => !v.is_some_and(|v| self.values.iter().any(|w| eq1(&self.key, v, w))),
            op => {
                let (Some(x), Some(y)) = (v.and_then(|v| num(v)), num(&self.values[0])) else { return false };
                match op {
                    Op::Ge => x >= y,
                    Op::Le => x <= y,
                    Op::Gt => x > y,
                    _ => x < y,
                }
            }
        }
    }
}

impl Selector {
    pub fn matches(&self, a: &Attrs) -> bool {
        self.terms.iter().all(|t| t.matches(a))
    }
}

/// Build a node's attributes. `labels` = free labels (key, value); `pools` = pool names it belongs to; `lab_name` = its lab;
/// `live` = (threads, cpu %) when connected (online=true). class = "<cpu_model>/<cores_logical>c/<ram GiB rounded>G" (FLT-03).
pub fn attrs(node: &NodeRow, labels: &[(String, String)], pools: &[String], lab_name: Option<&str>, live: Option<(u32, f64)>) -> Attrs {
    let mut a = Attrs::new();
    let mut put = |k: &str, v: &str| {
        if !v.is_empty() {
            a.insert(k.to_string(), v.to_string());
        }
    };
    put("node", &node.node_id);
    put("host", &node.host);
    put("addr", &node.addr);
    put("state", &node.state);
    put("lab", lab_name.unwrap_or(""));
    put("pool", &pools.join(","));
    put("class", &class_of(&node.inventory));
    put("virtual", if node.node_id.contains("/v") { "true" } else { "false" });
    put("online", if live.is_some() { "true" } else { "false" });
    if let Some(x) = node.x {
        put("x", &x.to_string());
    }
    if let Some(y) = node.y {
        put("y", &y.to_string());
    }
    if let Some((t, c)) = live {
        put("threads", &t.to_string());
        put("cpu", &format!("{c:.1}"));
    }
    if let Ok(inv) = serde_json::from_str::<crate::proto::Inventory>(&node.inventory) {
        put("cores", &inv.cores_logical.to_string());
        put("ram_gb", &gib(inv.ram_bytes).to_string());
        put("arch", &inv.arch);
        put("os", &inv.os);
    }
    for (k, v) in labels {
        a.entry(k.to_lowercase()).or_insert_with(|| v.clone()); // built-ins win
    }
    a
}

fn gib(bytes: u64) -> u64 {
    (bytes as f64 / (1u64 << 30) as f64).round() as u64
}

/// Hardware class string of an inventory JSON (proto::Inventory as text), "" when unknown (FLT-03).
pub fn class_of(inventory_json: &str) -> String {
    match serde_json::from_str::<crate::proto::Inventory>(inventory_json) {
        Ok(i) if !i.cpu_model.is_empty() => format!("{}/{}c/{}G", i.cpu_model.trim(), i.cores_logical, gib(i.ram_bytes)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(kv: &[(&str, &str)]) -> Attrs {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
    fn m(sel: &str, a: &Attrs) -> bool {
        parse(sel).unwrap().matches(a)
    }

    #[test]
    fn operators() {
        let a = at(&[("lab", "A"), ("cores", "8"), ("cpu", "12.5"), ("flag", "false"), ("on", "true")]);
        for (s, want) in [
            ("", true),
            ("lab=A", true),
            ("lab=a", true),
            ("lab=B", false),
            ("lab!=B", true),
            ("lab!=a", false),
            ("cores>=8", true),
            ("cores>8", false),
            ("cores<=8", true),
            ("cores<9", true),
            ("cores<8", false),
            ("cores=8.0", true),
            ("cores=8 lab=A", true),
            ("cores=8 lab=B", false),
            ("cpu<50 cpu>10", true),
            ("on", true),
            ("flag", false),
            ("nokey", false),
            ("!flag", true),
            ("!nokey", true),
            ("!on", false),
        ] {
            assert_eq!(m(s, &a), want, "selector `{s}`");
        }
    }

    #[test]
    fn alternatives_prefix_quotes() {
        let a = at(&[("host", "lab-a-03"), ("class", "Intel i5-10400F/12c/16G"), ("os", "Ubuntu")]);
        assert!(m("os=Windows,ubuntu", &a));
        assert!(!m("os=Windows,macos", &a));
        assert!(!m("os!=Windows,ubuntu", &a));
        assert!(m("os!=Windows,macos", &a));
        assert!(m("host=lab-a*", &a));
        assert!(m("host=LAB-A*", &a));
        assert!(!m("host=lab-b*", &a));
        assert!(m("host!=lab-b*", &a));
        assert!(m(r#"class="Intel i5-10400F/12c/16G""#, &a));
        assert!(m(r#"class="x y",  "Intel i5-10400F/12c/16G""#.replace("  ", "").as_str(), &a));
        assert!(m(r#"class="Intel i5*" host=lab-a*"#, &a));
        assert!(!m(r#"class="Intel i7""#, &a));
    }

    #[test]
    fn missing_keys() {
        let a = at(&[("lab", "A")]);
        assert!(!m("x=1", &a));
        assert!(m("x!=1", &a));
        assert!(!m("x>=1", &a));
        assert!(!m("x<1", &a));
        let nan = at(&[("x", "abc")]);
        assert!(!m("x>=1", &nan));
    }

    #[test]
    fn pools() {
        let a = at(&[("pool", "night,Lab-A")]);
        assert!(m("pool=night", &a));
        assert!(m("pool=lab-a", &a));
        assert!(m("pool=day,night", &a));
        assert!(!m("pool=day", &a));
        assert!(m("pool!=day", &a));
        assert!(!m("pool!=night", &a));
        assert!(m("pool=lab*", &a));
        assert!(m("pool", &a));
        assert!(m("!pool", &at(&[])));
    }

    #[test]
    fn keys_lower_cased_and_text_kept() {
        let s = parse("  LAB=A   Cores>=4 ").unwrap();
        assert_eq!(s.terms[0].key, "lab");
        assert_eq!(s.terms[1].key, "cores");
        assert_eq!(s.text, "LAB=A   Cores>=4");
        assert_eq!(s.terms[1].op, Op::Ge);
        assert_eq!(parse("!broken").unwrap().terms[0], Term { key: "broken".into(), op: Op::Not, values: vec![] });
        assert_eq!(parse("k=a,b,c").unwrap().terms[0].values, vec!["a", "b", "c"]);
        assert!(parse("").unwrap().terms.is_empty());
        assert!(parse("   ").unwrap().matches(&at(&[])));
    }

    #[test]
    fn bad_input() {
        for (s, frag) in [
            ("cores>>8", "cores>>8"),
            ("cores>=abc", "cores>=abc"),
            ("lab=", "lab="),
            ("=x", "=x"),
            ("lab==A", "lab==A"),
            (r#"class="oops"#, "unclosed"),
            ("cores>=1,2", "cores>=1,2"),
            ("a=b,,c", "a=b,,c"),
            ("!", "`!`"),
            ("ok=1 bad=<", "bad=<"),
        ] {
            let e = parse(s).unwrap_err();
            assert!(e.contains(frag), "`{s}` -> {e}");
        }
    }

    fn node(inv: &str) -> NodeRow {
        NodeRow {
            node_id: "n1".into(),
            host: "lab-a-01".into(),
            addr: "10.0.0.5".into(),
            inventory: inv.into(),
            first_seen_ms: 0,
            last_seen_ms: 0,
            state: "active".into(),
            state_reason: String::new(),
            state_by: String::new(),
            lab_id: Some(1),
            x: Some(3),
            y: Some(0),
            calib_mhs: None,
            net_mbps: None,
            rtt_ms: None,
            probed_ms: None,
        }
    }
    const INV: &str = r#"{"cpu_model":"Intel i5-10400F","cores_physical":6,"cores_logical":12,"ram_bytes":17179869184,"os":"Ubuntu 24.04","kernel":"6","arch":"x86_64","ck_version":"2"}"#;

    #[test]
    fn class_and_attrs() {
        assert_eq!(class_of(INV), "Intel i5-10400F/12c/16G");
        assert_eq!(class_of("garbage"), "");
        assert_eq!(class_of(""), "");
        let labels = vec![("Room".to_string(), "204".to_string()), ("state".to_string(), "hacked".to_string())];
        let a = attrs(&node(INV), &labels, &["night".into(), "day".into()], Some("Lab A"), Some((8, 33.33)));
        for (k, v) in [
            ("node", "n1"), ("host", "lab-a-01"), ("addr", "10.0.0.5"), ("state", "active"), ("online", "true"), ("lab", "Lab A"),
            ("x", "3"), ("y", "0"), ("pool", "night,day"), ("cores", "12"), ("threads", "8"), ("ram_gb", "16"), ("cpu", "33.3"),
            ("arch", "x86_64"), ("os", "Ubuntu 24.04"), ("class", "Intel i5-10400F/12c/16G"), ("virtual", "false"), ("room", "204"),
        ] {
            assert_eq!(a.get(k).map(String::as_str), Some(v), "attr {k}");
        }
        assert!(m("lab=\"lab a\" pool=night room=204 cores>=8 online", &a));
        let off = attrs(&node("bad"), &[], &[], None, None);
        assert_eq!(off["online"], "false");
        assert!(!off.contains_key("cores") && !off.contains_key("threads") && !off.contains_key("lab") && !off.contains_key("pool"));
        let mut v = node(INV);
        v.node_id = "n1/v3".into();
        assert_eq!(attrs(&v, &[], &[], None, None)["virtual"], "true");
    }
}
