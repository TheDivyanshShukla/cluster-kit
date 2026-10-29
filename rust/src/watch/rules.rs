//! Rules sit after the detector. A line is the whole configuration:
//! `count("cell phone") > 40`, `count("cell phone") < 5`, `seen("pistol") or seen("knife")`.
//!
//! A "less than" rule is frozen on frames the detector did not run, so a blurry
//! classroom is not reported as "no phones".

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
    Count { class: String, op: Op, n: u32 },
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

pub fn compile(line: &str) -> Result<Compiled, String> {
    let mut p = Parser { s: line.as_bytes(), i: 0 };
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
    Ok(Compiled { expr, needs_look, classes })
}

pub fn line_for(class: &str, op: &str, n: u32) -> String {
    match op {
        "seen" => format!("seen(\"{class}\")"),
        "lt" => format!("count(\"{class}\") < {n}"),
        "le" => format!("count(\"{class}\") <= {n}"),
        "ge" => format!("count(\"{class}\") >= {n}"),
        "eq" => format!("count(\"{class}\") == {n}"),
        _ => format!("count(\"{class}\") > {n}"),
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
        Expr::Seen(c) => push_class(classes, c),
        Expr::Count { class, op, .. } => {
            push_class(classes, class);
            if matches!(op, Op::Lt | Op::Le) {
                *needs_look = true;
            }
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            walk(a, classes, needs_look);
            walk(b, classes, needs_look);
        }
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
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
    fn rest(&self) -> String {
        String::from_utf8_lossy(&self.s[self.i..]).chars().take(24).collect()
    }
    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' {
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
            if boundary.is_ascii_alphanumeric() {
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
            self.bump();
            let e = self.parse_or()?;
            self.skip();
            if self.bump() != Some(b')') {
                return Err("missing )".into());
            }
            return Ok(e);
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
        Err("expected seen(...) or count(...)".into())
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
        let mut out = String::new();
        while let Some(c) = self.bump() {
            if c == b'"' {
                return Ok(out);
            }
            if c == b'\\' {
                if let Some(n) = self.bump() {
                    out.push(n as char);
                    continue;
                }
            }
            out.push(c as char);
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
        std::str::from_utf8(&self.s[start..self.i]).unwrap_or("0").parse().map_err(|_| "number too large".into())
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
        assert_eq!(line_for("cell phone", "lt", 5), r#"count("cell phone") < 5"#);
        assert_eq!(line_for("pistol", "seen", 0), r#"seen("pistol")"#);
        assert!(compile("count(phone) > 1").is_err());
    }
}
