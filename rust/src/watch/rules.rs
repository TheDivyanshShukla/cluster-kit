//! Rules sit after the detector. A line is the whole configuration:
//! `count("cell phone") > 40`, `count("cell phone") < 5`, `seen("pistol") or seen("knife")`.
//!
//! A "less than" rule is frozen on frames the detector did not run, so a blurry
//! classroom is not reported as "no phones".

use crate::watch::domain::{Count, FrameSummary};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Compiled {
    expr: Expr,
    pub needs_look: bool,
    pub classes: Vec<String>,
}

#[derive(Clone, Debug)]
enum Expr {
    Seen(String),
    Ratio {
        numerator: String,
        denominator: String,
        op: Op,
        value: f64,
    },
    Lua(String),
    Count {
        class: String,
        op: Op,
        n: u32,
    },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

#[derive(Clone, Debug, Default)]
pub struct Streak {
    pub n: u32,
    announced: u8,
}

impl Compiled {
    pub fn eval(&self, counts: &HashMap<String, u32>) -> bool {
        eval(&self.expr, counts)
    }
}

impl Compiled {
    /// Three-valued evaluation over a typed summary: `Ok(None)` is Unknown. A class the
    /// frame was not analyzed for (or is unsupported) is never read as zero, and a ratio with a
    /// zero denominator is Unknown. Kleene logic: `false and unknown` = false, `true or unknown` = true.
    pub fn eval_summary(&self, s: &FrameSummary) -> Result<Option<bool>, String> {
        eval3(&self.expr, s, &self.classes)
    }
}

fn eval3(expr: &Expr, s: &FrameSummary, classes: &[String]) -> Result<Option<bool>, String> {
    let known = |c: &str| match s.class(c) {
        Count::Known(n) => Some(n),
        _ => None,
    };
    Ok(match expr {
        Expr::Seen(c) => known(c).map(|n| n >= 1),
        Expr::Count { class, op, n } => known(class).map(|v| compare(v as f64, *op, *n as f64)),
        Expr::Ratio { numerator, denominator, op, value } => match (known(numerator), known(denominator)) {
            (Some(a), Some(d)) if d > 0 => Some(compare(a as f64 / d as f64, *op, *value)),
            _ => None,
        },
        Expr::And(a, b) => match (eval3(a, s, classes)?, eval3(b, s, classes)?) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Expr::Or(a, b) => match (eval3(a, s, classes)?, eval3(b, s, classes)?) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        // Legacy `lua:` line: count()/seen() globals; every referenced class must be known.
        Expr::Lua(code) => {
            let mut counts = HashMap::new();
            for c in classes {
                match known(c) {
                    Some(n) => {
                        counts.insert(c.clone(), n);
                    }
                    None => return Ok(None),
                }
            }
            Some(lua_eval(code, &counts)?)
        }
    })
}

pub fn compile(line: &str) -> Result<Compiled, String> {
    if line.len() > 4096 {
        return Err("rule exceeds 4096 bytes".into());
    }
    if let Some(code) = line.trim().strip_prefix("lua:") {
        let code = code.trim();
        if code.is_empty() || code.len() > 4096 {
            return Err("Lua rule must contain 1..4096 bytes".into());
        }
        let mut classes = Vec::new();
        // Literal class names make model capability gating possible before evaluation.
        for (i, _) in code.char_indices() {
            let tail = &code[i..];
            if (tail.starts_with("count") || tail.starts_with("seen"))
                && (i == 0 || !code.as_bytes()[i - 1].is_ascii_alphanumeric())
            {
                let len = if tail.starts_with("count") { 5 } else { 4 };
                let mut p = Parser {
                    s: tail.as_bytes(),
                    i: len,
                    depth: 0,
                };
                let class = p
                    .quoted_arg()
                    .map_err(|_| "Lua count/seen requires a literal double-quoted class name")?;
                push_class(&mut classes, &class);
            }
        }
        if classes.is_empty() {
            return Err("Lua rule must reference count(\"class\") or seen(\"class\")".into());
        }
        lua_eval(code, &HashMap::new())?;
        return Ok(Compiled {
            expr: Expr::Lua(code.into()),
            needs_look: true,
            classes,
        });
    }
    let mut p = Parser {
        s: line.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.skip();
    if p.eof() {
        return Err("rule is empty".into());
    }
    let expr = p.parse_or()?;
    p.skip();
    if !p.eof() {
        return Err(format!("unexpected text near '{}'", p.rest()));
    }
    let mut classes = Vec::new();
    let mut needs_look = false;
    walk(&expr, &mut classes, &mut needs_look);
    Ok(Compiled {
        expr,
        needs_look,
        classes,
    })
}

pub fn line_for(class: &str, op: &str, n: u32) -> String {
    let class = serde_json::to_string(class).expect("string serialization cannot fail");
    match op {
        "seen" => format!("seen({class})"),
        "lt" => format!("count({class}) < {n}"),
        "le" => format!("count({class}) <= {n}"),
        "ge" => format!("count({class}) >= {n}"),
        "eq" => format!("count({class}) == {n}"),
        _ => format!("count({class}) > {n}"),
    }
}

/// `Some("checking")` once the condition has lasted 2 frames, `Some("agreed")` when it
/// has lasted `hold` frames. A frozen frame (blur, blank, detector busy) does neither.
pub fn step(s: &mut Streak, on: bool, freeze: bool, hold: u32) -> Option<&'static str> {
    let hold = hold.max(1);
    if freeze {
        return None;
    }
    if !on {
        *s = Streak::default();
        return None;
    }
    s.n = s.n.saturating_add(1);
    if s.n >= hold && s.announced < 2 {
        s.announced = 2;
        return Some("agreed");
    }
    if hold > 2 && s.n >= 2 && s.announced < 1 {
        s.announced = 1;
        return Some("checking");
    }
    None
}

fn eval(expr: &Expr, counts: &HashMap<String, u32>) -> bool {
    match expr {
        Expr::Lua(code) => lua_eval(code, counts).unwrap_or(false),
        Expr::Ratio {
            numerator,
            denominator,
            op,
            value,
        } => {
            let den = counts.get(denominator).copied().unwrap_or(0);
            den > 0
                && compare(
                    counts.get(numerator).copied().unwrap_or(0) as f64 / den as f64,
                    *op,
                    *value,
                )
        }
        Expr::Seen(c) => counts.get(c).copied().unwrap_or(0) >= 1,
        Expr::Count { class, op, n } => {
            let v = counts.get(class).copied().unwrap_or(0);
            match op {
                Op::Gt => v > *n,
                Op::Ge => v >= *n,
                Op::Lt => v < *n,
                Op::Le => v <= *n,
                Op::Eq => v == *n,
            }
        }
        Expr::And(a, b) => eval(a, counts) && eval(b, counts),
        Expr::Or(a, b) => eval(a, counts) || eval(b, counts),
    }
}

fn walk(expr: &Expr, classes: &mut Vec<String>, needs_look: &mut bool) {
    match expr {
        Expr::Lua(_) => *needs_look = true,
        Expr::Ratio {
            numerator,
            denominator,
            ..
        } => {
            push_class(classes, numerator);
            push_class(classes, denominator);
            *needs_look = true;
        }
        Expr::Seen(c) => push_class(classes, c),
        Expr::Count { class, op, .. } => {
            push_class(classes, class);
            if matches!(op, Op::Lt | Op::Le | Op::Eq) {
                *needs_look = true;
            }
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            walk(a, classes, needs_look);
            walk(b, classes, needs_look);
        }
    }
}

fn compare(a: f64, op: Op, b: f64) -> bool {
    match op {
        Op::Gt => a > b,
        Op::Ge => a >= b,
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Eq => a == b,
    }
}

fn lua_eval(code: &str, counts: &HashMap<String, u32>) -> Result<bool, String> {
    use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value};
    let lua = Lua::new_with(
        StdLib::MATH | StdLib::STRING | StdLib::TABLE,
        LuaOptions::new(),
    )
    .map_err(|e| e.to_string())?;
    lua.set_memory_limit(2 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    lua.set_hook(
        HookTriggers {
            every_nth_instruction: Some(10_000),
            ..HookTriggers::default()
        },
        |_, _| {
            Err(mlua::Error::external(
                "Lua rule exceeded instruction budget",
            ))
        },
    );
    let data = counts.clone();
    let count = lua
        .create_function(move |_, class: String| Ok(data.get(&class).copied().unwrap_or(0)))
        .map_err(|e| e.to_string())?;
    lua.globals()
        .set("count", count)
        .map_err(|e| e.to_string())?;
    let data = counts.clone();
    let seen = lua
        .create_function(move |_, class: String| Ok(data.get(&class).copied().unwrap_or(0) > 0))
        .map_err(|e| e.to_string())?;
    lua.globals().set("seen", seen).map_err(|e| e.to_string())?;
    let wrapped = if code.starts_with("return ") {
        code.to_owned()
    } else {
        format!("return ({code})")
    };
    let result: Value = lua.load(&wrapped).eval().map_err(|e| e.to_string())?;
    match result {
        Value::Boolean(v) => Ok(v),
        _ => Err("Lua rule must return a boolean".into()),
    }
}

fn push_class(classes: &mut Vec<String>, c: &str) {
    if !classes.iter().any(|x| x == c) {
        classes.push(c.to_string());
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
    fn rest(&self) -> String {
        String::from_utf8_lossy(&self.s[self.i..])
            .chars()
            .take(24)
            .collect()
    }
    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.i += 1;
            } else {
                break;
            }
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        loop {
            self.skip();
            if self.word("or") {
                let right = self.parse_and()?;
                left = Expr::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_primary()?;
        loop {
            self.skip();
            if self.word("and") {
                let right = self.parse_primary()?;
                left = Expr::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }
    fn word(&mut self, w: &str) -> bool {
        let b = w.as_bytes();
        if self.s[self.i..].starts_with(b) {
            let after = self.i + b.len();
            let boundary = self.s.get(after).copied().unwrap_or(b' ');
            if boundary.is_ascii_alphanumeric() || boundary == b'_' {
                return false;
            }
            self.i = after;
            true
        } else {
            false
        }
    }
    fn parse_primary(&mut self) -> Result<Expr, String> {
        self.skip();
        if self.peek() == Some(b'(') {
            self.depth += 1;
            if self.depth > 24 {
                return Err("rule nesting exceeds 24 levels".into());
            }
            self.bump();
            let e = self.parse_or()?;
            self.depth -= 1;
            self.skip();
            if self.bump() != Some(b')') {
                return Err("missing )".into());
            }
            return Ok(e);
        }
        if self.word("ratio") {
            self.skip();
            if self.bump() != Some(b'(') {
                return Err("expected (".into());
            }
            self.skip();
            let numerator = self.quoted()?;
            self.skip();
            if self.bump() != Some(b',') {
                return Err("ratio needs two quoted class names".into());
            }
            self.skip();
            let denominator = self.quoted()?;
            self.skip();
            if self.bump() != Some(b')') {
                return Err("expected )".into());
            }
            if numerator.is_empty() || denominator.is_empty() {
                return Err("ratio classes cannot be empty".into());
            }
            self.skip();
            let op = self.op()?;
            self.skip();
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
                self.i += 1;
            }
            let value: f64 = std::str::from_utf8(&self.s[start..self.i])
                .unwrap_or("")
                .parse()
                .map_err(|_| "expected a nonnegative ratio")?;
            if !value.is_finite() || value < 0.0 {
                return Err("ratio must be finite and nonnegative".into());
            }
            return Ok(Expr::Ratio {
                numerator,
                denominator,
                op,
                value,
            });
        }
        if self.word("seen") {
            let class = self.quoted_arg()?;
            return Ok(Expr::Seen(class));
        }
        if self.word("count") {
            let class = self.quoted_arg()?;
            self.skip();
            let op = self.op()?;
            self.skip();
            let n = self.number()?;
            return Ok(Expr::Count { class, op, n });
        }
        Err("expected seen(...), count(...) or ratio(...)".into())
    }
    fn quoted_arg(&mut self) -> Result<String, String> {
        self.skip();
        if self.bump() != Some(b'(') {
            return Err("expected (".into());
        }
        self.skip();
        let class = self.quoted()?;
        self.skip();
        if self.bump() != Some(b')') {
            return Err("expected )".into());
        }
        if class.is_empty() {
            return Err("class name is empty".into());
        }
        Ok(class)
    }
    fn quoted(&mut self) -> Result<String, String> {
        if self.bump() != Some(b'"') {
            return Err("class name needs quotes".into());
        }
        let mut out = Vec::new();
        while let Some(c) = self.bump() {
            if c == b'"' {
                return String::from_utf8(out).map_err(|_| "class must be valid UTF-8".into());
            }
            if c == b'\\' {
                if let Some(n) = self.bump() {
                    out.push(n);
                    continue;
                }
            }
            out.push(c);
        }
        Err("missing closing quote".into())
    }
    fn op(&mut self) -> Result<Op, String> {
        let two = self.s.get(self.i..self.i + 2).unwrap_or(&[]);
        let op = if two == b">=" {
            Op::Ge
        } else if two == b"<=" {
            Op::Le
        } else if two == b"==" {
            Op::Eq
        } else if self.peek() == Some(b'>') {
            Op::Gt
        } else if self.peek() == Some(b'<') {
            Op::Lt
        } else {
            return Err("expected >, <, >=, <= or ==".into());
        };
        self.i += if matches!(op, Op::Gt | Op::Lt) { 1 } else { 2 };
        Ok(op)
    }
    fn number(&mut self) -> Result<u32, String> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return Err("expected a number".into());
        }
        std::str::from_utf8(&self.s[start..self.i])
            .unwrap_or("0")
            .parse()
            .map_err(|_| "number too large".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| ((*k).into(), *v)).collect()
    }

    #[test]
    fn phone_rules_match_the_classroom_examples() {
        let many = compile(r#"count("cell phone") > 40"#).unwrap();
        let few = compile(r#"count("cell phone") < 5"#).unwrap();
        assert!(!many.needs_look);
        assert!(few.needs_look);
        assert_eq!(few.classes, vec!["cell phone".to_string()]);

        let busy = counts(&[("cell phone", 41)]);
        assert!(many.eval(&busy));
        assert!(!few.eval(&busy));

        let quiet = counts(&[("cell phone", 3)]);
        assert!(few.eval(&quiet));
        assert!(!many.eval(&quiet));
    }

    #[test]
    fn weapon_line_is_any_of_the_names() {
        let w = compile(r#"seen("pistol") or seen("knife") or seen("gun")"#).unwrap();
        assert!(!w.needs_look);
        assert!(!w.eval(&counts(&[])));
        assert!(w.eval(&counts(&[("knife", 1)])));
        assert_eq!(w.classes.len(), 3);
    }

    #[test]
    fn blur_does_not_advance_or_clear_a_less_than_rule() {
        let mut s = Streak { n: 3, announced: 1 };
        assert!(step(&mut s, true, true, 8).is_none());
        assert_eq!(s.n, 3);
        assert_eq!(step(&mut s, true, false, 8), None);
        assert_eq!(s.n, 4);
        assert_eq!(step(&mut s, false, false, 8), None);
        assert_eq!(s.n, 0);
    }

    #[test]
    fn hold_fires_once() {
        let mut s = Streak::default();
        assert_eq!(step(&mut s, true, false, 3), None);
        assert_eq!(step(&mut s, true, false, 3), Some("checking"));
        assert_eq!(step(&mut s, true, false, 3), Some("agreed"));
        assert_eq!(step(&mut s, true, false, 3), None);
    }

    #[test]
    fn form_line_and_bad_line() {
        assert_eq!(
            line_for("cell phone", "lt", 5),
            r#"count("cell phone") < 5"#
        );
        assert_eq!(line_for("pistol", "seen", 0), r#"seen("pistol")"#);
        assert!(compile("count(phone) > 1").is_err());
    }
    #[test]
    fn ratio_supports_classroom_threshold_without_dividing_by_zero() {
        let r = compile(r#"ratio("cell phone", "person") >= 0.2"#).unwrap();
        assert!(r.needs_look);
        assert!(r.eval(&counts(&[("cell phone", 12), ("person", 60)])));
        assert!(!r.eval(&counts(&[("cell phone", 11), ("person", 60)])));
        assert!(!r.eval(&counts(&[])));
        assert_eq!(r.classes, vec!["cell phone", "person"]);
    }

    #[test]
    fn zero_equality_requires_detector_evidence() {
        assert!(compile(r#"count("cell phone") == 0"#).unwrap().needs_look);
    }

    #[test]
    fn lua_alert_is_boolean_sandboxed_and_model_gated() {
        let r = compile(r#"lua: count("cell phone") >= 4 and count("person") > 0"#).unwrap();
        assert!(r.eval(&counts(&[("cell phone", 4), ("person", 10)])));
        assert!(!r.eval(&counts(&[("cell phone", 3), ("person", 10)])));
        assert!(r.needs_look);
        assert_eq!(r.classes, vec!["cell phone", "person"]);
        assert!(compile(r#"lua: os.execute("true") or seen("person")"#).is_err());
        assert!(compile(r#"lua: count("person")"#).is_err());
        assert!(compile(r#"lua: (function() while true do end end)() or seen("person")"#).is_err());
    }
    #[test]
    fn rules_reject_resource_exhaustion_inputs() {
        assert!(compile(&" ".repeat(4097)).is_err());
        let nested = format!("{}seen(\"person\"){}", "(".repeat(26), ")".repeat(26));
        assert!(compile(&nested).is_err());
    }
}
