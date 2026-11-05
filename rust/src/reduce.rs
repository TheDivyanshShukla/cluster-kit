//! Result reducers (JOB-07): how the head combines a job's stored parts into one result. Pure. Owner: a2.
//! Contract: `reduce(kind, reducer, parts)`; parts are (start, end, body) ordered by start.
//!   ""/"concat": bodies in order (sha: the "<n> <hex>" lines, sorted by n; synth: the count)
//!   "lines": like concat but every body is split into lines and empty lines dropped
//!   "sum": every line that parses as a number is added (integers stay integers; any float makes a float)
//!   "count": number of non-empty lines        "sort": non-empty lines sorted (numeric when all are numbers)
//!   "top:N": the N largest numeric lines, largest first (N 1..=10000)
//! Unknown reducer -> Err with the list of known ones.
//! For kind sha/synth the line reducers work on the concat text (sha: binary records are rendered first).

pub const REDUCERS: &[&str] = &["concat", "lines", "sum", "count", "sort", "top:N"];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The concat result: today's api::result_text formatting.
fn concat(kind: &str, parts: &[(u64, u64, Vec<u8>)]) -> Vec<u8> {
    match kind {
        "sha" => {
            let mut hits: Vec<(u64, &[u8])> = parts.iter().flat_map(|p| p.2.chunks_exact(40)).map(|r| (u64::from_le_bytes(r[..8].try_into().unwrap()), &r[8..])).collect();
            hits.sort_unstable();
            hits.iter().map(|(n, d)| format!("{n} {}\n", hex(d))).collect::<String>().into_bytes()
        }
        "synth" => format!("{}\n", parts.iter().map(|p| u64::from_le_bytes(p.2.get(..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 8]))).sum::<u64>()).into_bytes(),
        _ => parts.iter().flat_map(|p| p.2.iter().copied()).collect(),
    }
}

fn finite(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|f| f.is_finite())
}

pub fn reduce(kind: &str, reducer: &str, parts: &[(u64, u64, Vec<u8>)]) -> Result<Vec<u8>, String> {
    if !valid(reducer) {
        return Err(format!("unknown reducer '{reducer}'; known: {}", REDUCERS.join(", ")));
    }
    if matches!(reducer, "" | "concat") {
        return Ok(concat(kind, parts));
    }
    let text = if matches!(kind, "sha" | "synth") { concat(kind, parts) } else { parts.iter().flat_map(|p| p.2.iter().copied().chain(std::iter::once(b'\n'))).collect() };
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let join = |v: Vec<&str>| v.iter().map(|l| format!("{l}\n")).collect::<String>().into_bytes();
    Ok(match reducer {
        "lines" => join(lines),
        "count" => format!("{}\n", lines.len()).into_bytes(),
        "sum" => {
            let (mut int, mut flt, mut any_float) = (0i128, 0f64, false);
            for l in &lines {
                let l = l.trim();
                match (l.parse::<i128>(), finite(l)) {
                    (Ok(i), _) if !any_float => match int.checked_add(i) {
                        Some(s) => int = s,
                        None => {
                            any_float = true;
                            flt = int as f64 + i as f64;
                        }
                    },
                    (Ok(i), _) => flt += i as f64,
                    (_, Some(f)) => {
                        if !any_float {
                            flt = int as f64;
                            any_float = true;
                        }
                        flt += f;
                    }
                    _ => {}
                }
            }
            format!("{}\n", if any_float { flt.to_string() } else { int.to_string() }).into_bytes()
        }
        "sort" => {
            let mut v = lines;
            if v.iter().all(|l| finite(l.trim()).is_some()) {
                v.sort_by(|a, b| finite(a.trim()).unwrap().total_cmp(&finite(b.trim()).unwrap()));
            } else {
                v.sort_unstable();
            }
            join(v)
        }
        top => {
            let n: usize = top.strip_prefix("top:").and_then(|n| n.parse().ok()).unwrap_or(0);
            let mut v: Vec<(f64, &str)> = lines.iter().filter_map(|l| finite(l.trim()).map(|f| (f, *l))).collect();
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            join(v.into_iter().take(n).map(|x| x.1).collect())
        }
    })
}

/// Is this a reducer name `reduce` accepts?
pub fn valid(reducer: &str) -> bool {
    matches!(reducer, "" | "concat" | "lines" | "sum" | "count" | "sort") || reducer.strip_prefix("top:").and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| (1..=10000).contains(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(bodies: &[&str]) -> Vec<(u64, u64, Vec<u8>)> {
        bodies.iter().enumerate().map(|(i, b)| (i as u64 * 10, i as u64 * 10 + 10, b.as_bytes().to_vec())).collect()
    }
    fn r(k: &str, red: &str, bodies: &[&str]) -> String {
        String::from_utf8(reduce(k, red, &p(bodies)).unwrap()).unwrap()
    }
    fn rec(n: u64, fill: u8) -> Vec<u8> {
        [n.to_le_bytes().to_vec(), vec![fill; 32]].concat()
    }

    #[test]
    fn concat_text() {
        assert_eq!(r("list", "", &["a\n", "b\n"]), "a\nb\n");
        assert_eq!(r("range", "concat", &["x", "y"]), "xy");
        assert_eq!(reduce("range", "concat", &[]).unwrap(), b"");
    }

    #[test]
    fn concat_sha_sorted() {
        let parts = vec![(0, 10, [rec(9, 0xab), rec(2, 0x01)].concat()), (10, 20, rec(5, 0xff))];
        let out = String::from_utf8(reduce("sha", "concat", &parts).unwrap()).unwrap();
        assert_eq!(out, format!("2 {}\n5 {}\n9 {}\n", "01".repeat(32), "ff".repeat(32), "ab".repeat(32)));
        assert_eq!(reduce("sha", "count", &parts).unwrap(), b"3\n");
    }

    #[test]
    fn concat_synth_sums_counts() {
        let parts = vec![(0, 1, 7u64.to_le_bytes().to_vec()), (1, 2, 5u64.to_le_bytes().to_vec()), (2, 3, vec![])];
        assert_eq!(reduce("synth", "", &parts).unwrap(), b"12\n");
    }

    #[test]
    fn lines_count() {
        assert_eq!(r("list", "lines", &["a\n\nb", "\n\nc\n"]), "a\nb\nc\n");
        assert_eq!(r("list", "count", &["a\n\nb", "c"]), "3\n");
        assert_eq!(r("list", "count", &[]), "0\n");
    }

    #[test]
    fn sum_exact_and_float() {
        assert_eq!(r("list", "sum", &["1\n2\nx\n", "3"]), "6\n");
        assert_eq!(r("list", "sum", &["-5\n2"]), "-3\n");
        assert_eq!(r("list", "sum", &["1\n2.5\n"]), "3.5\n");
        assert_eq!(r("list", "sum", &["2.5\n1\n"]), "3.5\n");
        // above 2^53 stays exact
        assert_eq!(r("list", "sum", &["9007199254740993\n1"]), "9007199254740994\n");
        assert_eq!(r("list", "sum", &["340282366920938463463374607431768211455\n1"]).trim().parse::<f64>().is_ok(), true);
        assert_eq!(r("list", "sum", &["nan\ninf\n4"]), "4\n");
        assert_eq!(r("list", "sum", &[]), "0\n");
    }

    #[test]
    fn sort_numeric_or_text() {
        assert_eq!(r("list", "sort", &["10\n9\n-1\n2.5"]), "-1\n2.5\n9\n10\n");
        assert_eq!(r("list", "sort", &["10\nb\n9\n"]), "10\n9\nb\n");
    }

    #[test]
    fn top_n() {
        assert_eq!(r("list", "top:2", &["5\n1\nx\n9\n", "7"]), "9\n7\n");
        assert_eq!(r("list", "top:10", &["3\n1"]), "3\n1\n");
        assert_eq!(r("list", "top:1", &["a\nb"]), "");
    }

    #[test]
    fn bad_names() {
        for bad in ["avg", "top:", "top:0", "top:10001", "top:x", "SUM", "top:-1"] {
            let e = reduce("list", bad, &[]).unwrap_err();
            assert!(e.contains("top:N") && e.contains("concat"), "{bad}: {e}");
            assert!(!valid(bad));
        }
        assert!(valid("top:10000") && valid("top:1") && valid("") && valid("sort"));
    }
}
