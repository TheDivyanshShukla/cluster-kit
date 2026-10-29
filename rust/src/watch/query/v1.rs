//! Typed query layer for `/api/v1/queries`.
//!
//! SQL text is parsed into a small AST over logical tables (`detections`, `tracks`,
//! `relations`, `frames`, `sightings` = detections). It is never executed against storage:
//! safe conjuncts are pushed down as a `DetectionFilter` (a superset), and the full predicate
//! is then evaluated in memory on bounded pages. Units: `COUNT(*)` counts rows of the table
//! (observations for detections); `COUNT(DISTINCT track_id)` counts scoped tracks.

#![allow(dead_code)]

use crate::watch::domain::*;
use crate::watch::matches::{self, Point};
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

pub const MAX_SQL_LEN: usize = 16_384;
pub const MAX_TOKENS: usize = 1024;
pub const MAX_DEPTH: usize = 24;
pub const MAX_IN: usize = 256;
pub const MAX_ROWS: u32 = 100_000;

#[derive(Clone, Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub sql: Option<String>,
    /// Sandboxed Lua script (mutually exclusive with `sql`); see docs/QUERY_LUA.md.
    #[serde(default)]
    pub lua: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub min_confidence: Option<f32>,
    #[serde(default = "empty_scope")]
    pub scope: QueryScope,
    #[serde(default)]
    pub gap_us: Option<i64>,
    #[serde(default)]
    pub pre_us: Option<i64>,
    #[serde(default)]
    pub post_us: Option<i64>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub actor: Option<String>,
}

fn empty_scope() -> QueryScope {
    QueryScope { source_ids: vec![], media_ids: vec![], from_utc_us: None, to_utc_us: None }
}

/// Execution bounds. Exceeding them returns partial results plus a warning.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_scan: u64,
    pub deadline: Duration,
    pub page: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_scan: 200_000, deadline: Duration::from_secs(5), page: 2000 }
    }
}

pub struct Outcome {
    pub run: QueryRun,
    pub matches: Vec<ReviewMatch>,
    /// What `run.result_count` counts: observations | tracks | relations | frames |
    /// distinct_tracks | distinct_values.
    pub unit: String,
    /// Distinct scoped tracks among matched rows (detections / tracks only).
    pub distinct_tracks: Option<u64>,
    /// Values of `SELECT DISTINCT col` (capped by LIMIT).
    pub values: Vec<Value>,
}

// ---------------------------------------------------------------- errors / lexer

fn perr(msg: impl std::fmt::Display, pos: usize) -> ApiError {
    ApiError::new("parse_error", format!("{msg} at position {pos}"))
}
fn too_large(msg: impl std::fmt::Display) -> ApiError {
    ApiError::new("too_large", msg.to_string())
}

#[derive(Clone, Debug, PartialEq)]
enum K {
    Word(String),
    Text(String),
    Num(f64),
    Sym(char),
    Op(String),
}
#[derive(Clone, Debug)]
struct Tok {
    k: K,
    pos: usize,
}

fn lex(s: &str) -> Result<Vec<Tok>, ApiError> {
    if s.len() > MAX_SQL_LEN {
        return Err(too_large(format!("SQL exceeds {MAX_SQL_LEN} bytes")));
    }
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        let pos = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let k = if c == '\'' {
            i += 1;
            let mut t = String::new();
            loop {
                match cs.get(i) {
                    Some('\'') if cs.get(i + 1) == Some(&'\'') => {
                        t.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(&c) => {
                        t.push(c);
                        i += 1;
                    }
                    None => return Err(perr("unterminated quoted string", pos)),
                }
            }
            K::Text(t)
        } else if c.is_ascii_digit() || ((c == '.' || c == '-') && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '.')) {
            i += 1;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            if i < cs.len() && (cs[i] == 'e' || cs[i] == 'E') {
                let mut j = i + 1;
                if j < cs.len() && (cs[j] == '+' || cs[j] == '-') {
                    j += 1;
                }
                if j < cs.len() && cs[j].is_ascii_digit() {
                    i = j;
                    while i < cs.len() && cs[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let txt: String = cs[pos..i].iter().collect();
            let n: f64 = txt.parse().map_err(|_| perr(format!("invalid number {txt}"), pos))?;
            if !n.is_finite() {
                return Err(perr("numbers must be finite", pos));
            }
            K::Num(n)
        } else if c.is_ascii_alphabetic() || c == '_' {
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            K::Word(cs[pos..i].iter().collect())
        } else if matches!(c, '=' | '!' | '<' | '>') {
            let mut op = String::from(c);
            i += 1;
            if cs.get(i) == Some(&'=') {
                op.push('=');
                i += 1;
            }
            if !matches!(op.as_str(), "=" | "!=" | "<" | "<=" | ">" | ">=") {
                return Err(perr(format!("unsupported operator {op}"), pos));
            }
            K::Op(op)
        } else if matches!(c, '*' | '/' | '(' | ')' | ',' | ';') {
            i += 1;
            K::Sym(c)
        } else {
            return Err(perr(format!("unsupported character '{c}'"), pos));
        };
        out.push(Tok { k, pos });
        if out.len() > MAX_TOKENS {
            return Err(too_large(format!("query has more than {MAX_TOKENS} tokens")));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- AST

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    Num,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Table {
    Detections,
    Tracks,
    Relations,
    Frames,
}
impl Table {
    fn name(self) -> &'static str {
        match self {
            Table::Detections => "detections",
            Table::Tracks => "tracks",
            Table::Relations => "relations",
            Table::Frames => "frames",
        }
    }
}
#[derive(Clone, Debug)]
enum E {
    Field(String),
    CountOf(String),
    Num(f64),
    Text(String),
    Mul(Box<E>, Box<E>),
    Div(Box<E>, Box<E>),
}
#[derive(Clone, Debug)]
enum X {
    Cmp(E, String, E),
    In(E, Vec<E>, bool),
    And(Box<X>, Box<X>),
    Or(Box<X>, Box<X>),
    Not(Box<X>),
}
#[derive(Clone, Debug)]
enum Sel {
    All,
    Count,
    CountDistinct(String),
    Distinct(String),
}
#[derive(Clone, Debug)]
struct Stmt {
    sel: Sel,
    table: Table,
    filter: Option<X>,
    /// Some(desc) when ORDER BY media_time_us.
    order: Option<bool>,
    limit: Option<u32>,
}

enum FrameQ {
    Class(String),
    Rel(Predicate, String),
}
fn frame_q(f: &str) -> Option<FrameQ> {
    let n = f.strip_suffix("_count").filter(|n| !n.is_empty())?;
    Some(match n {
        "people" => FrameQ::Class("person".into()),
        "phone_holders" => FrameQ::Rel(Predicate::Holding, "cell phone".into()),
        "near_phone" => FrameQ::Rel(Predicate::Near, "cell phone".into()),
        _ => FrameQ::Class(n.replace('_', " ")),
    })
}

fn field_ty(t: Table, f: &str) -> Option<Ty> {
    use Ty::*;
    if t != Table::Relations && matches!(f, "source_id" | "media_id") {
        return Some(Text);
    }
    match t {
        Table::Detections => match f {
            "class" | "model_version" | "track_id" | "observation_id" => Some(Text),
            "confidence" | "media_time_us" | "capture_time_utc_us" => Some(Num),
            _ => None,
        },
        Table::Tracks => match f {
            "class" | "model_version" | "track_id" => Some(Text),
            "confidence" | "media_time_us" | "start_us" | "end_us" | "observations" => Some(Num),
            _ => None,
        },
        Table::Relations => match f {
            "predicate" | "object_class" | "subject_track_id" | "object_track_id" | "module_version" | "observation_id" => Some(Text),
            "confidence" => Some(Num),
            _ => None,
        },
        Table::Frames => match f {
            "media_time_us" | "capture_time_utc_us" | "min_confidence" => Some(Num),
            f if frame_q(f).is_some() => Some(Num),
            _ => None,
        },
    }
}

fn ty(e: &E, t: Table) -> Ty {
    match e {
        E::Field(f) => field_ty(t, f).unwrap_or(Ty::Num),
        E::Text(_) => Ty::Text,
        _ => Ty::Num,
    }
}

// ---------------------------------------------------------------- parser

struct P {
    toks: Vec<Tok>,
    at: usize,
    end: usize,
    table: Table,
}

impl P {
    fn pos(&self) -> usize {
        self.toks.get(self.at).map_or(self.end, |t| t.pos)
    }
    fn word(&mut self, s: &str) -> bool {
        if matches!(self.toks.get(self.at), Some(Tok { k: K::Word(w), .. }) if w.eq_ignore_ascii_case(s)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn need(&mut self, s: &str) -> Result<(), ApiError> {
        if self.word(s) {
            Ok(())
        } else {
            Err(perr(format!("expected {s}"), self.pos()))
        }
    }
    fn sym(&mut self, c: char) -> bool {
        if matches!(self.toks.get(self.at), Some(Tok { k: K::Sym(x), .. }) if *x == c) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn need_sym(&mut self, c: char) -> Result<(), ApiError> {
        if self.sym(c) {
            Ok(())
        } else {
            Err(perr(format!("expected '{c}'"), self.pos()))
        }
    }
    fn name(&mut self, what: &str) -> Result<(String, usize), ApiError> {
        match self.toks.get(self.at) {
            Some(Tok { k: K::Word(w), pos }) => {
                self.at += 1;
                Ok((w.to_ascii_lowercase(), *pos))
            }
            _ => Err(perr(format!("expected {what}"), self.pos())),
        }
    }
    fn depth(&self, d: usize) -> Result<(), ApiError> {
        if d > MAX_DEPTH {
            Err(too_large(format!("query nesting exceeds {MAX_DEPTH} levels")))
        } else {
            Ok(())
        }
    }

    fn or(&mut self, d: usize) -> Result<X, ApiError> {
        let mut x = self.and(d)?;
        while self.word("OR") {
            x = X::Or(Box::new(x), Box::new(self.and(d)?));
        }
        Ok(x)
    }
    fn and(&mut self, d: usize) -> Result<X, ApiError> {
        let mut x = self.not(d)?;
        while self.word("AND") {
            x = X::And(Box::new(x), Box::new(self.not(d)?));
        }
        Ok(x)
    }
    fn not(&mut self, d: usize) -> Result<X, ApiError> {
        self.depth(d)?;
        if self.word("NOT") {
            return Ok(X::Not(Box::new(self.not(d + 1)?)));
        }
        if self.sym('(') {
            let x = self.or(d + 1)?;
            self.need_sym(')')?;
            return Ok(x);
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<X, ApiError> {
        let l = self.arith()?;
        let lt = ty(&l, self.table);
        let neg = self.word("NOT");
        if neg || self.word("IN") {
            if neg {
                self.need("IN")?;
            }
            self.need_sym('(')?;
            let mut lits = Vec::new();
            loop {
                let p = self.pos();
                let lit = match self.toks.get(self.at).map(|t| t.k.clone()) {
                    Some(K::Num(n)) => E::Num(n),
                    Some(K::Text(s)) => E::Text(s),
                    _ => return Err(perr("IN lists take quoted strings or numbers", p)),
                };
                self.at += 1;
                if ty(&lit, self.table) != lt {
                    return Err(perr("IN value type does not match the field", p));
                }
                lits.push(lit);
                if lits.len() > MAX_IN {
                    return Err(too_large(format!("IN list has more than {MAX_IN} values")));
                }
                if !self.sym(',') {
                    break;
                }
            }
            self.need_sym(')')?;
            return Ok(X::In(l, lits, neg));
        }
        let (op, opos) = match self.toks.get(self.at) {
            Some(Tok { k: K::Op(o), pos }) => (o.clone(), *pos),
            _ => return Err(perr("expected =, !=, <, <=, >, >= or IN", self.pos())),
        };
        self.at += 1;
        let r = self.arith()?;
        if ty(&r, self.table) != lt {
            return Err(perr("cannot compare a number with text", opos));
        }
        Ok(X::Cmp(l, op, r))
    }
    fn arith(&mut self) -> Result<E, ApiError> {
        let mut e = self.atom()?;
        loop {
            let mul = if self.sym('*') {
                true
            } else if self.sym('/') {
                false
            } else {
                return Ok(e);
            };
            let p = self.pos();
            let r = self.atom()?;
            if ty(&e, self.table) != Ty::Num || ty(&r, self.table) != Ty::Num {
                return Err(perr("arithmetic needs numbers", p));
            }
            e = if mul { E::Mul(Box::new(e), Box::new(r)) } else { E::Div(Box::new(e), Box::new(r)) };
        }
    }
    fn atom(&mut self) -> Result<E, ApiError> {
        let p = self.pos();
        match self.toks.get(self.at).map(|t| t.k.clone()) {
            Some(K::Num(n)) => {
                self.at += 1;
                Ok(E::Num(n))
            }
            Some(K::Text(s)) => {
                self.at += 1;
                Ok(E::Text(s))
            }
            Some(K::Word(w)) => {
                self.at += 1;
                let w = w.to_ascii_lowercase();
                if w == "count" && matches!(self.toks.get(self.at), Some(Tok { k: K::Sym('('), .. })) {
                    if self.table != Table::Frames {
                        return Err(perr("count('class') is only available on the frames table", p));
                    }
                    self.at += 1;
                    let c = match self.toks.get(self.at).map(|t| t.k.clone()) {
                        Some(K::Text(s)) => s,
                        _ => return Err(perr("count() takes a quoted class name", self.pos())),
                    };
                    self.at += 1;
                    self.need_sym(')')?;
                    return Ok(E::CountOf(c.to_ascii_lowercase()));
                }
                if field_ty(self.table, &w).is_none() {
                    return Err(perr(format!("unknown field '{w}' for table '{}'", self.table.name()), p));
                }
                Ok(E::Field(w))
            }
            _ => Err(perr("expected a field, number or quoted string", p)),
        }
    }
}

fn parse(sql: &str) -> Result<Stmt, ApiError> {
    let toks = lex(sql)?;
    let mut p = P { toks, at: 0, end: sql.chars().count(), table: Table::Detections };
    p.need("SELECT")?;
    enum RawSel {
        All,
        Count,
        CountDistinct(String, usize),
        Distinct(String, usize),
    }
    let raw = if p.sym('*') {
        RawSel::All
    } else if p.word("DISTINCT") {
        let (n, pos) = p.name("a column")?;
        RawSel::Distinct(n, pos)
    } else if p.word("COUNT") {
        p.need_sym('(')?;
        let r = if p.sym('*') {
            RawSel::Count
        } else {
            p.need("DISTINCT")?;
            let (n, pos) = p.name("a column")?;
            RawSel::CountDistinct(n, pos)
        };
        p.need_sym(')')?;
        r
    } else {
        return Err(perr("expected *, DISTINCT <column>, COUNT(*) or COUNT(DISTINCT <column>)", p.pos()));
    };
    p.need("FROM")?;
    let (tname, tpos) = p.name("a table name")?;
    let table = match tname.as_str() {
        "detections" | "sightings" => Table::Detections,
        "tracks" => Table::Tracks,
        "relations" => Table::Relations,
        "frames" => Table::Frames,
        "events" => {
            return Err(ApiError::new(
                "unsupported",
                "table 'events' is not available yet: temporal events, persistent conditions and DURING/NOW need alert semantics; query detections, tracks, relations or frames",
            ))
        }
        _ => return Err(perr(format!("unknown table '{tname}' (use detections, tracks, relations, frames, events or sightings)"), tpos)),
    };
    p.table = table;
    let check = |n: &str, pos: usize| {
        if field_ty(table, n).is_some() {
            Ok(())
        } else {
            Err(perr(format!("unknown column '{n}' for table '{}'", table.name()), pos))
        }
    };
    let sel = match raw {
        RawSel::All => Sel::All,
        RawSel::Count => Sel::Count,
        RawSel::Distinct(n, pos) => {
            check(&n, pos)?;
            Sel::Distinct(n)
        }
        RawSel::CountDistinct(n, pos) => {
            check(&n, pos)?;
            Sel::CountDistinct(n)
        }
    };
    let filter = if p.word("WHERE") { Some(p.or(0)?) } else { None };
    let mut order = None;
    if p.word("ORDER") {
        p.need("BY")?;
        let (n, pos) = p.name("media_time_us")?;
        if n != "media_time_us" || table == Table::Relations {
            return Err(perr("ORDER BY supports only media_time_us (not on relations)", pos));
        }
        order = Some(if p.word("DESC") {
            true
        } else {
            p.word("ASC");
            false
        });
    }
    let mut limit = None;
    if p.word("LIMIT") {
        let pos = p.pos();
        match p.toks.get(p.at).map(|t| t.k.clone()) {
            Some(K::Num(n)) if n >= 0.0 && n.fract() == 0.0 && n <= MAX_ROWS as f64 => limit = Some(n as u32),
            _ => return Err(perr(format!("LIMIT must be an integer from 0 to {MAX_ROWS}"), pos)),
        }
        p.at += 1;
    }
    p.sym(';');
    if p.at != p.toks.len() {
        return Err(perr("unsupported SQL clause or extra statement", p.pos()));
    }
    Ok(Stmt { sel, table, filter, order, limit })
}

// ---------------------------------------------------------------- rendering (normalized form)

fn r_e(e: &E) -> String {
    match e {
        E::Field(f) => f.clone(),
        E::CountOf(c) => format!("count('{}')", c.replace('\'', "''")),
        E::Num(n) => format!("{n}"),
        E::Text(s) => format!("'{}'", s.replace('\'', "''")),
        E::Mul(a, b) => format!("{} * {}", r_e(a), r_e(b)),
        E::Div(a, b) => format!("{} / {}", r_e(a), r_e(b)),
    }
}
fn r_x(x: &X) -> String {
    match x {
        X::Cmp(l, o, r) => format!("{} {o} {}", r_e(l), r_e(r)),
        X::In(l, v, n) => format!("{} {}IN ({})", r_e(l), if *n { "NOT " } else { "" }, v.iter().map(r_e).collect::<Vec<_>>().join(", ")),
        X::And(a, b) => format!("({} AND {})", r_x(a), r_x(b)),
        X::Or(a, b) => format!("({} OR {})", r_x(a), r_x(b)),
        X::Not(a) => format!("NOT {}", r_x(a)),
    }
}
fn render(s: &Stmt) -> String {
    let sel = match &s.sel {
        Sel::All => "*".to_string(),
        Sel::Count => "COUNT(*)".into(),
        Sel::CountDistinct(c) => format!("COUNT(DISTINCT {c})"),
        Sel::Distinct(c) => format!("DISTINCT {c}"),
    };
    let mut out = format!("SELECT {sel} FROM {}", s.table.name());
    if let Some(f) = &s.filter {
        out += &format!(" WHERE {}", r_x(f));
    }
    if let Some(d) = s.order {
        out += &format!(" ORDER BY media_time_us {}", if d { "DESC" } else { "ASC" });
    }
    if let Some(l) = s.limit {
        out += &format!(" LIMIT {l}");
    }
    out
}

// ---------------------------------------------------------------- evaluation

#[derive(Clone, Debug)]
enum V {
    N(f64),
    T(String),
    Null,
}
struct Flags {
    unknown: Cell<bool>,
    unsupported: Cell<bool>,
}
struct TrackRow {
    source_id: String,
    media_id: String,
    track_id: String,
    class: String,
    model_version: String,
    confidence: f32,
    first_us: Us,
    last_us: Us,
    pairs: Vec<(FrameObservation, Detection)>,
}
enum Row<'a> {
    Det(&'a FrameObservation, &'a Detection),
    Track(&'a TrackRow),
    Rel(&'a Relation),
    Frame(&'a FrameSummary),
}

fn conf(c: f32) -> V {
    V::N(c.to_string().parse().unwrap_or(c as f64))
}
fn opt_t(s: &Option<String>) -> V {
    s.clone().map_or(V::Null, V::T)
}
fn cnt(c: Count, fl: &Flags) -> V {
    match c {
        Count::Known(n) => V::N(n as f64),
        Count::Unknown => {
            fl.unknown.set(true);
            V::Null
        }
        Count::Unsupported => {
            fl.unsupported.set(true);
            V::Null
        }
    }
}
fn pred_name(p: Predicate) -> &'static str {
    match p {
        Predicate::Near => "near",
        Predicate::Holding => "holding",
    }
}

fn get(row: &Row, f: &str, fl: &Flags) -> V {
    let n = |x: i64| V::N(x as f64);
    match row {
        Row::Det(o, d) => match f {
            "source_id" => V::T(o.source_id.clone()),
            "media_id" => V::T(o.media_id.clone()),
            "class" => V::T(d.class.clone()),
            "confidence" => conf(d.confidence),
            "model_version" => V::T(d.model_version.clone()),
            "track_id" => opt_t(&d.track_id),
            "observation_id" => V::T(o.observation_id.clone()),
            "media_time_us" => n(o.media_time_us),
            "capture_time_utc_us" => o.capture_time_utc_us.map_or(V::Null, n),
            _ => V::Null,
        },
        Row::Track(t) => match f {
            "source_id" => V::T(t.source_id.clone()),
            "media_id" => V::T(t.media_id.clone()),
            "track_id" => V::T(t.track_id.clone()),
            "class" => V::T(t.class.clone()),
            "model_version" => V::T(t.model_version.clone()),
            "confidence" => conf(t.confidence),
            "media_time_us" | "start_us" => n(t.first_us),
            "end_us" => n(t.last_us),
            "observations" => V::N(t.pairs.iter().map(|(o, _)| &o.observation_id).collect::<HashSet<_>>().len() as f64),
            _ => V::Null,
        },
        Row::Rel(r) => match f {
            "predicate" => V::T(pred_name(r.predicate).into()),
            "object_class" => V::T(r.object_class.clone()),
            "subject_track_id" => opt_t(&r.subject_track_id),
            "object_track_id" => opt_t(&r.object_track_id),
            "module_version" => V::T(r.module_version.clone()),
            "observation_id" => V::T(r.observation_id.clone()),
            "confidence" => conf(r.confidence),
            _ => V::Null,
        },
        Row::Frame(s) => match f {
            "source_id" => V::T(s.source_id.clone()),
            "media_id" => V::T(s.media_id.clone()),
            "media_time_us" => n(s.media_time_us),
            "capture_time_utc_us" => s.capture_time_utc_us.map_or(V::Null, n),
            "min_confidence" => conf(s.min_confidence),
            _ => match frame_q(f) {
                Some(FrameQ::Class(c)) => cnt(s.class(&c), fl),
                Some(FrameQ::Rel(p, c)) => cnt(s.relation(p, &c), fl),
                None => V::Null,
            },
        },
    }
}

fn ev(e: &E, row: &Row, fl: &Flags) -> V {
    match e {
        E::Field(f) => get(row, f, fl),
        E::CountOf(c) => match row {
            Row::Frame(s) => cnt(s.class(c), fl),
            _ => V::Null,
        },
        E::Num(n) => V::N(*n),
        E::Text(s) => V::T(s.clone()),
        E::Mul(a, b) => match (ev(a, row, fl), ev(b, row, fl)) {
            (V::N(a), V::N(b)) => V::N(a * b),
            _ => V::Null,
        },
        E::Div(a, b) => match (ev(a, row, fl), ev(b, row, fl)) {
            (V::N(a), V::N(b)) if b != 0.0 => V::N(a / b),
            _ => V::Null,
        },
    }
}

fn compare(a: &V, op: &str, b: &V) -> Option<bool> {
    use std::cmp::Ordering::*;
    let ord = match (a, b) {
        (V::N(a), V::N(b)) => {
            if (a - b).abs() < 1e-9 {
                Equal
            } else {
                a.partial_cmp(b)?
            }
        }
        (V::T(a), V::T(b)) => a.cmp(b),
        _ => return None,
    };
    Some(match op {
        "=" => ord == Equal,
        "!=" => ord != Equal,
        "<" => ord == Less,
        "<=" => ord != Greater,
        ">" => ord == Greater,
        ">=" => ord != Less,
        _ => return None,
    })
}

/// Three-valued: None = unknown (a NULL or unknown count was involved).
fn evx(x: &X, row: &Row, fl: &Flags) -> Option<bool> {
    match x {
        X::Cmp(l, op, r) => compare(&ev(l, row, fl), op, &ev(r, row, fl)),
        X::In(l, vs, neg) => {
            let lv = ev(l, row, fl);
            if matches!(lv, V::Null) {
                return None;
            }
            let hit = vs.iter().any(|v| compare(&lv, "=", &ev(v, row, fl)) == Some(true));
            Some(hit != *neg)
        }
        X::And(a, b) => match (evx(a, row, fl), evx(b, row, fl)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        X::Or(a, b) => match (evx(a, row, fl), evx(b, row, fl)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        X::Not(a) => evx(a, row, fl).map(|b| !b),
    }
}

// ---------------------------------------------------------------- capability / pushdown

fn e_holding(e: &E, t: Table) -> bool {
    match e {
        E::Field(f) => t == Table::Frames && matches!(frame_q(f), Some(FrameQ::Rel(Predicate::Holding, _))),
        E::Mul(a, b) | E::Div(a, b) => e_holding(a, t) || e_holding(b, t),
        _ => false,
    }
}
fn is_pred(e: &E) -> bool {
    matches!(e, E::Field(f) if f == "predicate")
}
fn is_holding_lit(e: &E) -> bool {
    matches!(e, E::Text(s) if s.eq_ignore_ascii_case("holding"))
}
fn x_holding(x: &X, t: Table) -> bool {
    match x {
        X::Cmp(l, _, r) => e_holding(l, t) || e_holding(r, t) || (is_pred(l) && is_holding_lit(r)) || (is_pred(r) && is_holding_lit(l)),
        X::In(l, vs, _) => e_holding(l, t) || (is_pred(l) && vs.iter().any(is_holding_lit)),
        X::And(a, b) | X::Or(a, b) => x_holding(a, t) || x_holding(b, t),
        X::Not(a) => x_holding(a, t),
    }
}

fn conjuncts<'a>(x: &'a X, out: &mut Vec<&'a X>) {
    if let X::And(a, b) = x {
        conjuncts(a, out);
        conjuncts(b, out);
    } else {
        out.push(x);
    }
}

/// Narrow a string list (empty = unrestricted). Sets `dead` when no value can match.
fn narrow(dst: &mut Vec<String>, vals: Vec<String>, dead: &mut bool) {
    if dst.is_empty() {
        *dst = vals;
    } else {
        dst.retain(|d| vals.contains(d));
        if dst.is_empty() {
            *dead = true;
        }
    }
}

/// Superset pushdown from top-level AND conjuncts; the full predicate is re-evaluated in memory.
/// Returns (filter, relation predicate, contradiction).
fn pushdown(st: &Stmt, scope: &QueryScope) -> (DetectionFilter, Option<Predicate>, bool) {
    let mut f = DetectionFilter {
        source_ids: scope.source_ids.clone(),
        media_ids: scope.media_ids.clone(),
        utc_from_us: scope.from_utc_us,
        utc_to_us: scope.to_utc_us,
        ..Default::default()
    };
    let mut pred = None;
    let mut dead = false;
    let Some(w) = &st.filter else { return (f, pred, dead) };
    let mut cs = Vec::new();
    conjuncts(w, &mut cs);
    let t = st.table;
    for c in cs {
        let (field, op, lits): (&str, &str, Vec<&E>) = match c {
            X::Cmp(E::Field(f), op, r @ (E::Num(_) | E::Text(_))) => (f.as_str(), op.as_str(), vec![r]),
            X::In(E::Field(f), vs, false) => (f.as_str(), "in", vs.iter().collect()),
            _ => continue,
        };
        let texts: Vec<String> = lits.iter().filter_map(|l| if let E::Text(s) = l { Some(s.clone()) } else { None }).collect();
        let num = match lits.first() {
            Some(E::Num(n)) if lits.len() == 1 => Some(*n),
            _ => None,
        };
        let eq = op == "=" || op == "in";
        let det = t == Table::Detections;
        match field {
            "source_id" if eq && t != Table::Relations => narrow(&mut f.source_ids, texts, &mut dead),
            "media_id" if eq && t != Table::Relations => narrow(&mut f.media_ids, texts, &mut dead),
            "class" if eq && (det || t == Table::Tracks) => narrow(&mut f.classes, texts, &mut dead),
            "model_version" if eq && (det || t == Table::Tracks) => narrow(&mut f.model_versions, texts, &mut dead),
            "track_id" if eq && det => narrow(&mut f.track_ids, texts, &mut dead),
            "predicate" if op == "=" && t == Table::Relations => {
                pred = match texts.first().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("near") => Some(Predicate::Near),
                    Some("holding") => Some(Predicate::Holding),
                    _ => pred,
                }
            }
            "confidence" if det => {
                if let (Some(n), true) = (num, matches!(op, ">" | ">=" | "=")) {
                    let n = n as f32;
                    f.min_confidence = Some(f.min_confidence.map_or(n, |m| m.max(n)));
                }
            }
            "media_time_us" | "capture_time_utc_us" if det || t == Table::Frames => {
                if let Some(n) = num {
                    let (from, to) = if field == "media_time_us" { (&mut f.media_from_us, &mut f.media_to_us) } else { (&mut f.utc_from_us, &mut f.utc_to_us) };
                    if matches!(op, ">" | ">=" | "=") {
                        let v = n.floor() as i64;
                        *from = Some(from.map_or(v, |m| m.max(v)));
                    }
                    if matches!(op, "<" | "<=" | "=") {
                        let v = n.ceil() as i64;
                        *to = Some(to.map_or(v, |m| m.min(v)));
                    }
                }
            }
            _ => {}
        }
    }
    (f, pred, dead)
}

// ---------------------------------------------------------------- bounded scanning

struct Scan {
    start: Instant,
    scanned: u64,
    partial: Option<String>,
}

fn paged<T>(
    sc: &mut Scan,
    lim: &Limits,
    mut fetch: impl FnMut(&Page) -> StoreResult<(Vec<T>, Option<String>)>,
    mut each: impl FnMut(T) -> bool,
) -> StoreResult<()> {
    let mut after = None;
    loop {
        if sc.start.elapsed() >= lim.deadline {
            sc.partial = Some("deadline exceeded".into());
            return Ok(());
        }
        if sc.scanned >= lim.max_scan {
            sc.partial = Some(format!("scan limit of {} rows reached", lim.max_scan));
            return Ok(());
        }
        let (rows, next) = fetch(&Page { after: after.clone(), limit: lim.page })?;
        for r in rows {
            sc.scanned += 1;
            if !each(r) {
                return Ok(());
            }
        }
        match next {
            Some(c) => after = Some(c),
            None => return Ok(()),
        }
    }
}

// ---------------------------------------------------------------- run

pub fn run(store: &dyn Store, req: &Request) -> Result<(QueryRun, Vec<ReviewMatch>), ApiError> {
    let o = run_detailed(store, req)?;
    Ok((o.run, o.matches))
}

pub fn run_detailed(store: &dyn Store, req: &Request) -> Result<Outcome, ApiError> {
    run_with(store, req, &Limits::default())
}

fn and_in(st: &mut Stmt, x: X) {
    st.filter = Some(match st.filter.take() {
        Some(w) => X::And(Box::new(w), Box::new(x)),
        None => x,
    });
}

fn media_map(store: &dyn Store, ids: impl Iterator<Item = String>) -> StoreResult<HashMap<String, MediaAsset>> {
    let mut m = HashMap::new();
    for id in ids {
        if !m.contains_key(&id) {
            if let Some(a) = store.media(&id)? {
                m.insert(id, a);
            }
        }
    }
    Ok(m)
}

/// Aggregate coverage from jobs in scope. Returns (coverage, partial_or_unknown).
fn coverage(store: &dyn Store, scope: &QueryScope) -> StoreResult<(Coverage, bool)> {
    let mut media: HashSet<String> = scope.media_ids.iter().cloned().collect();
    for s in &scope.source_ids {
        media.extend(store.media_for_source(s)?.into_iter().map(|m| m.id));
    }
    let all = scope.media_ids.is_empty() && scope.source_ids.is_empty();
    let jobs: Vec<IndexJob> = store.jobs()?.into_iter().filter(|j| all || j.media_ids.iter().any(|m| media.contains(m))).collect();
    let mut c = Coverage::default();
    let mut partial = jobs.is_empty();
    for j in &jobs {
        c.decoded += j.coverage.decoded;
        c.inferred += j.coverage.inferred;
        c.skipped += j.coverage.skipped;
        c.failed += j.coverage.failed;
        partial |= j.state != JobState::Completed || j.coverage.failed > 0 || j.coverage.skipped > 0;
    }
    if jobs.len() == 1 && jobs[0].media_ids.len() == 1 {
        c.analyzed = jobs[0].coverage.analyzed.clone();
    }
    Ok((c, partial))
}

fn distinct_key(row: &Row, col: &str, fl: &Flags) -> Option<(String, Value)> {
    let v = get(row, col, fl);
    let (s, j) = match &v {
        V::N(n) => (n.to_string(), json!(n)),
        V::T(t) => (t.clone(), json!(t)),
        V::Null => return None,
    };
    let scoped = col == "track_id";
    let key = match (scoped, row) {
        (true, Row::Det(o, _)) => format!("{}\0{s}", o.media_id),
        (true, Row::Track(t)) => format!("{}\0{s}", t.media_id),
        _ => s,
    };
    Some((key, j))
}

pub fn run_with(store: &dyn Store, req: &Request, lim: &Limits) -> Result<Outcome, ApiError> {
    if req.lua.is_some() {
        return lua::run(store, req, lim);
    }
    let t0 = Instant::now();
    if req.sql.as_ref().is_some_and(|s| s.len() > MAX_SQL_LEN) {
        return Err(too_large(format!("SQL exceeds {MAX_SQL_LEN} bytes")));
    }
    let sql = req.sql.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if sql.is_none() && req.class.is_none() && req.min_confidence.is_none() {
        return Err(ApiError::new("invalid_request", "provide `sql`, or `class` / `min_confidence`"));
    }
    if req.min_confidence.is_some_and(|c| !(0.0..=1.0).contains(&c)) {
        return Err(ApiError::new("invalid_request", "min_confidence must be within 0..1"));
    }
    for (n, v) in [("gap_us", req.gap_us), ("pre_us", req.pre_us), ("post_us", req.post_us)] {
        if v.is_some_and(|v| v < 0) {
            return Err(ApiError::new("invalid_request", format!("{n} must not be negative")));
        }
    }
    let mut st = match sql {
        Some(s) => parse(s)?,
        None => Stmt { sel: Sel::All, table: Table::Detections, filter: None, order: None, limit: None },
    };
    let table = st.table;
    if let Some(c) = &req.class {
        let c = c.to_ascii_lowercase();
        and_in(&mut st, match table {
            Table::Frames => X::Cmp(E::CountOf(c), ">".into(), E::Num(0.0)),
            Table::Relations => X::Cmp(E::Field("object_class".into()), "=".into(), E::Text(c)),
            _ => X::Cmp(E::Field("class".into()), "=".into(), E::Text(c)),
        });
    }
    if let Some(c) = req.min_confidence {
        let f = if table == Table::Frames { "min_confidence" } else { "confidence" };
        and_in(&mut st, X::Cmp(E::Field(f.into()), ">=".into(), E::Num(c as f64)));
    }

    // Capability awareness.
    let models = store.models()?;
    if st.filter.as_ref().is_some_and(|x| x_holding(x, table))
        && !models.iter().any(|m| m.supported_relations.contains(&Predicate::Holding))
    {
        let have: Vec<String> = models.iter().map(|m| format!("{}@{}", m.id, m.version)).collect();
        return Err(ApiError::new(
            "unsupported",
            format!(
                "'holding' needs an installed relation module that declares Holding support (hand/pose-object model); installed: [{}]. Use 'near' for proximity only.",
                have.join(", ")
            ),
        ));
    }

    let snapshot = store.snapshot()?;
    let normalized = render(&st);
    let max_rows = req.limit.unwrap_or(MAX_ROWS).min(MAX_ROWS);
    let limit = st.limit.unwrap_or(MAX_ROWS).min(max_rows) as usize;
    let opts = matches::Opts {
        gap_us: req.gap_us.unwrap_or(2_000_000),
        pre_us: req.pre_us.unwrap_or(3_000_000),
        post_us: req.post_us.unwrap_or(3_000_000),
        ..Default::default()
    };
    let run_id = stable_id(
        "qr",
        &[
            &normalized,
            &serde_json::to_string(&req.scope).unwrap_or_default(),
            &snapshot.to_string(),
            &format!("{}/{}/{}/{}", opts.gap_us, opts.pre_us, opts.post_us, limit),
        ],
    );

    let aggregate = !matches!(st.sel, Sel::All);
    let (filter, rel_pred, dead) = pushdown(&st, &req.scope);
    let fl = Flags { unknown: Cell::new(false), unsupported: Cell::new(false) };
    let mut sc = Scan { start: t0, scanned: 0, partial: None };
    let mut warnings: Vec<String> = Vec::new();
    let (mut unknown_rows, mut unsupported_rows, mut limited) = (0u64, 0u64, false);
    let keep_all = aggregate || st.order.is_some() || table == Table::Tracks;
    let stop_at = limit + 1;

    let mut dets: Vec<(FrameObservation, Detection)> = Vec::new();
    let mut tracks: Vec<TrackRow> = Vec::new();
    let mut rels: Vec<Relation> = Vec::new();
    let mut frames: Vec<FrameSummary> = Vec::new();
    let mut note = |fl: &Flags, matched: bool| {
        if !matched {
            if fl.unknown.get() {
                unknown_rows += 1;
            }
            if fl.unsupported.get() {
                unsupported_rows += 1;
            }
        }
        fl.unknown.set(false);
        fl.unsupported.set(false);
    };
    let pass = |row: &Row, fl: &Flags| st.filter.as_ref().is_none_or(|x| evx(x, row, fl) == Some(true));

    if dead {
        warnings.push("the WHERE conditions contradict the scope; nothing can match".into());
    } else {
        match table {
            Table::Detections => paged(&mut sc, lim, |p| store.detections(&filter, p), |(o, d)| {
                let ok = pass(&Row::Det(&o, &d), &fl);
                note(&fl, ok);
                if ok {
                    dets.push((o, d));
                }
                keep_all || dets.len() < stop_at
            })?,
            Table::Tracks => {
                let mut by: BTreeMap<(String, String), TrackRow> = BTreeMap::new();
                paged(&mut sc, lim, |p| store.detections(&filter, p), |(o, d)| {
                    if let Some(tid) = d.track_id.clone() {
                        let e = by.entry((o.media_id.clone(), tid.clone())).or_insert_with(|| TrackRow {
                            source_id: o.source_id.clone(),
                            media_id: o.media_id.clone(),
                            track_id: tid,
                            class: d.class.clone(),
                            model_version: d.model_version.clone(),
                            confidence: 0.0,
                            first_us: o.media_time_us,
                            last_us: o.media_time_us,
                            pairs: vec![],
                        });
                        e.confidence = e.confidence.max(d.confidence);
                        e.first_us = e.first_us.min(o.media_time_us);
                        e.last_us = e.last_us.max(o.media_time_us);
                        e.pairs.push((o, d));
                    }
                    true
                })?;
                for t in by.into_values() {
                    let ok = pass(&Row::Track(&t), &fl);
                    note(&fl, ok);
                    if ok {
                        tracks.push(t);
                    }
                }
            }
            Table::Relations => paged(&mut sc, lim, |p| store.relations(&filter, rel_pred, p), |r| {
                let ok = pass(&Row::Rel(&r), &fl);
                note(&fl, ok);
                if ok {
                    rels.push(r);
                }
                keep_all || rels.len() < stop_at
            })?,
            Table::Frames => paged(&mut sc, lim, |p| store.summaries(&filter, p), |s| {
                let ok = pass(&Row::Frame(&s), &fl);
                note(&fl, ok);
                if ok {
                    frames.push(s);
                }
                keep_all || frames.len() < stop_at
            })?,
        }
    }

    // Ordering and limit (non-aggregates).
    if let Some(desc) = st.order {
        dets.sort_by(|a, b| (a.0.media_time_us, &a.0.media_id, &a.1.detection_id).cmp(&(b.0.media_time_us, &b.0.media_id, &b.1.detection_id)));
        tracks.sort_by(|a, b| (a.first_us, &a.media_id, &a.track_id).cmp(&(b.first_us, &b.media_id, &b.track_id)));
        frames.sort_by(|a, b| (a.media_time_us, &a.media_id, &a.observation_id).cmp(&(b.media_time_us, &b.media_id, &b.observation_id)));
        if desc {
            dets.reverse();
            tracks.reverse();
            frames.reverse();
        }
    }
    let total_rows = match table {
        Table::Detections => dets.len(),
        Table::Tracks => tracks.len(),
        Table::Relations => rels.len(),
        Table::Frames => frames.len(),
    };
    if !aggregate && total_rows > limit {
        limited = true;
        dets.truncate(limit);
        tracks.truncate(limit);
        rels.truncate(limit);
        frames.truncate(limit);
    }

    // Aggregates and distinctness.
    let rows: Vec<Row> = match table {
        Table::Detections => dets.iter().map(|(o, d)| Row::Det(o, d)).collect(),
        Table::Tracks => tracks.iter().map(Row::Track).collect(),
        Table::Relations => rels.iter().map(Row::Rel).collect(),
        Table::Frames => frames.iter().map(Row::Frame).collect(),
    };
    let distinct_tracks = match table {
        Table::Detections => Some(dets.iter().filter_map(|(o, d)| d.track_id.as_ref().map(|t| (&o.media_id, t))).collect::<HashSet<_>>().len() as u64),
        Table::Tracks => Some(tracks.len() as u64),
        _ => None,
    };
    let unit_rows = match table {
        Table::Detections => "observations",
        Table::Tracks => "tracks",
        Table::Relations => "relations",
        Table::Frames => "frames",
    };
    let mut values = Vec::new();
    let (kind, unit, result_count) = match &st.sel {
        Sel::All => (
            match table {
                Table::Detections => ResultKind::Detections,
                Table::Tracks => ResultKind::Tracks,
                Table::Relations => ResultKind::Relations,
                Table::Frames => ResultKind::Frames,
            },
            unit_rows.to_string(),
            rows.len() as u64,
        ),
        Sel::Count => (ResultKind::Aggregate, unit_rows.to_string(), rows.len() as u64),
        Sel::CountDistinct(c) | Sel::Distinct(c) => {
            let mut seen = HashSet::new();
            for r in &rows {
                if let Some((k, v)) = distinct_key(r, c, &fl) {
                    if seen.insert(k) && values.len() < limit {
                        values.push(v);
                    }
                }
            }
            if matches!(st.sel, Sel::CountDistinct(_)) {
                values.clear();
            }
            (ResultKind::Aggregate, if c.ends_with("track_id") { "distinct_tracks" } else { "distinct_values" }.to_string(), seen.len() as u64)
        }
    };

    // Windows.
    let mut out_matches = Vec::new();
    let mut missing_obs = 0usize;
    if !aggregate {
        let mut pts: Vec<Point> = Vec::new();
        let pt = |o: &FrameObservation, det: Option<&Detection>, rel: Option<&Relation>, c: f32| Point {
            source_id: o.source_id.clone(),
            media_id: o.media_id.clone(),
            observation_id: o.observation_id.clone(),
            media_time_us: o.media_time_us,
            detection_id: det.map(|d| d.detection_id.clone()).or_else(|| rel.map(|r| r.subject_detection_id.clone())),
            relation_id: rel.map(|r| r.relation_id.clone()),
            track_id: det.and_then(|d| d.track_id.clone()).or_else(|| rel.and_then(|r| r.subject_track_id.clone())),
            confidence: c,
        };
        match table {
            Table::Detections => pts.extend(dets.iter().map(|(o, d)| pt(o, Some(d), None, d.confidence))),
            Table::Tracks => pts.extend(tracks.iter().flat_map(|t| t.pairs.iter().map(|(o, d)| pt(o, Some(d), None, d.confidence)))),
            Table::Frames => pts.extend(frames.iter().map(|s| Point {
                source_id: s.source_id.clone(),
                media_id: s.media_id.clone(),
                observation_id: s.observation_id.clone(),
                media_time_us: s.media_time_us,
                detection_id: None,
                relation_id: None,
                track_id: None,
                confidence: s.min_confidence,
            })),
            Table::Relations => {
                // Relations carry only an observation id; resolve times from scoped observations.
                // ponytail: one extra bounded scan of detections; add a Store::observation_by_id when it matters.
                let want: HashSet<&str> = rels.iter().map(|r| r.observation_id.as_str()).collect();
                let mut obs: HashMap<String, FrameObservation> = HashMap::new();
                let f2 = DetectionFilter { source_ids: filter.source_ids.clone(), media_ids: filter.media_ids.clone(), utc_from_us: filter.utc_from_us, utc_to_us: filter.utc_to_us, ..Default::default() };
                paged(&mut sc, lim, |p| store.detections(&f2, p), |(o, _)| {
                    if want.contains(o.observation_id.as_str()) {
                        obs.entry(o.observation_id.clone()).or_insert(o);
                    }
                    obs.len() < want.len()
                })?;
                for r in &rels {
                    match obs.get(&r.observation_id) {
                        Some(o) => pts.push(pt(o, None, Some(r), r.confidence)),
                        None => missing_obs += 1,
                    }
                }
            }
        }
        let media = media_map(store, pts.iter().map(|p| p.media_id.clone()))?;
        out_matches = matches::windows_from_points(&pts, &media, &opts, &run_id);
    }

    // Warnings.
    if let Some(r) = &sc.partial {
        warnings.push(format!("partial results: scan stopped after {} rows ({r}); narrow the scope or add filters", sc.scanned));
    }
    if limited {
        warnings.push(format!("results limited to {limit} rows"));
    }
    if unknown_rows > 0 {
        warnings.push(format!("{unknown_rows} frame(s) have unknown counts for the referenced classes (not analyzed) and cannot satisfy numeric predicates; unknown is not zero"));
    }
    if unsupported_rows > 0 {
        warnings.push(format!("{unsupported_rows} frame(s) have no support for the referenced relation and cannot satisfy numeric predicates"));
    }
    if missing_obs > 0 {
        warnings.push(format!("{missing_obs} relation(s) could not be placed on the timeline (observation not found in scan)"));
    }
    let mut want_classes: Vec<String> = filter.classes.clone();
    if table == Table::Frames {
        // frame class counts are not pushed down; nothing to check
        want_classes.clear();
    }
    if !models.is_empty() {
        let labels: HashSet<&str> = models.iter().flat_map(|m| m.labels.iter().map(String::as_str)).collect();
        for c in want_classes.iter().filter(|c| !labels.contains(c.as_str())) {
            warnings.push(format!("class '{c}' is not a label of any installed model; zero results would mean the target cannot be detected, not that it is absent"));
        }
    }
    let (cov, cov_partial) = coverage(store, &req.scope)?;
    if result_count == 0 && (cov_partial || sc.partial.is_some() || unknown_rows > 0 || unsupported_rows > 0 || dead) {
        warnings.push("zero results are not evidence of absence: indexing coverage is unknown or partial for this scope".into());
    }
    if aggregate {
        warnings.push(format!("unit: {unit}"));
    }

    let run = QueryRun {
        id: run_id,
        schema_version: SCHEMA_VERSION,
        normalized,
        scope: req.scope.clone(),
        kind,
        snapshot,
        coverage: cov,
        warnings,
        result_count,
        elapsed_us: t0.elapsed().as_micros() as u64,
        actor: req.actor.clone(),
        cursor: None,
    };
    store.save_query_run(&run, &out_matches)?;
    Ok(Outcome { run, matches: out_matches, unit, distinct_tracks, values })
}

// ---------------------------------------------------------------- read side

pub fn get_run(store: &dyn Store, id: &str) -> Result<QueryRun, ApiError> {
    store.query_run(id)?.ok_or_else(|| ApiError::new("not_found", format!("query run {id} not found")))
}

/// Stable cursor pages straight from storage: `(matches, next_cursor)`.
pub fn matches_page(store: &dyn Store, run_id: &str, cursor: Option<String>, limit: u32) -> Result<(Vec<ReviewMatch>, Option<String>), ApiError> {
    get_run(store, run_id)?;
    store.matches(run_id, &Page { after: cursor, limit: limit.clamp(1, 1000) })
}

fn merge_intervals(mut v: Vec<[Us; 2]>) -> Vec<[Us; 2]> {
    v.sort();
    let mut out: Vec<[Us; 2]> = Vec::new();
    for i in v {
        match out.last_mut() {
            Some(l) if i[0] <= l[1] => l[1] = l[1].max(i[1]),
            _ => out.push(i),
        }
    }
    out
}

pub fn timeline(store: &dyn Store, run_id: &str, start_us: Us, end_us: Us, resolution_us: Us) -> Result<Value, ApiError> {
    if end_us < start_us || resolution_us < 0 {
        return Err(ApiError::new("invalid_request", "need start_us <= end_us and resolution_us >= 0"));
    }
    get_run(store, run_id)?;
    let mut all: Vec<ReviewMatch> = Vec::new();
    let mut after = None;
    loop {
        let (rows, next) = store.matches(run_id, &Page { after: after.clone(), limit: 1000 })?;
        all.extend(rows);
        match next {
            Some(c) if all.len() < 100_000 => after = Some(c),
            _ => break,
        }
    }
    let jobs = store.jobs()?;
    let mut groups: BTreeMap<(String, String), Vec<ReviewMatch>> = BTreeMap::new();
    for m in all {
        groups.entry((m.source_id.clone(), m.media_id.clone())).or_default().push(m);
    }
    let (mut lanes, mut gaps) = (Vec::new(), Vec::new());
    for ((source_id, media_id), mut ms) in groups {
        ms.sort_by_key(|m| (m.start_us, m.id.clone()));
        let asset = store.media(&media_id)?;
        let clock = ms[0].clock.clone();
        let visible: Vec<ReviewMatch> = ms.into_iter().filter(|m| m.context_end_us >= start_us && m.context_start_us <= end_us).collect();
        let exact_total = visible.len();
        // Coalesce small neighbouring windows for display only.
        let mut shown: Vec<Value> = Vec::new();
        let mut cluster: Vec<ReviewMatch> = Vec::new();
        let small = |m: &ReviewMatch| resolution_us > 0 && m.end_us - m.start_us < resolution_us;
        let flush = |c: &mut Vec<ReviewMatch>, shown: &mut Vec<Value>| {
            if c.is_empty() {
                return;
            }
            let mut m = c[0].clone();
            let n = c.len();
            if n > 1 {
                m.id = format!("cluster_{}", c[0].id);
                for x in &c[1..] {
                    m.start_us = m.start_us.min(x.start_us);
                    m.end_us = m.end_us.max(x.end_us);
                    m.context_start_us = m.context_start_us.min(x.context_start_us);
                    m.context_end_us = m.context_end_us.max(x.context_end_us);
                    m.observation_ids.extend(x.observation_ids.iter().cloned());
                    m.detection_ids.extend(x.detection_ids.iter().cloned());
                    m.relation_ids.extend(x.relation_ids.iter().cloned());
                    m.distinct_tracks += x.distinct_tracks;
                    m.max_confidence = m.max_confidence.max(x.max_confidence);
                }
            }
            let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
            v["exact"] = json!(n);
            v["coalesced"] = json!(n > 1);
            if n > 1 {
                v["exact_ids"] = json!(c.iter().map(|x| x.id.clone()).collect::<Vec<_>>());
            }
            shown.push(v);
            c.clear();
        };
        for m in visible {
            let join = cluster.last().is_some_and(|l| small(l) && small(&m) && m.start_us - l.end_us <= resolution_us)
                && cluster.iter().all(small);
            if !join {
                flush(&mut cluster, &mut shown);
            }
            cluster.push(m);
        }
        flush(&mut cluster, &mut shown);

        let analyzed = merge_intervals(
            jobs.iter().filter(|j| j.media_ids.len() == 1 && j.media_ids[0] == media_id).flat_map(|j| j.coverage.analyzed.clone()).collect(),
        );
        let coverage_known = !analyzed.is_empty();
        if coverage_known {
            let base_end = asset.as_ref().and_then(|a| a.duration_us).unwrap_or_else(|| analyzed.last().map_or(0, |a| a[1]));
            let mut cur = 0;
            let mut push_gap = |a: Us, b: Us| {
                let (a, b) = (a.max(start_us), b.min(end_us));
                if b > a {
                    gaps.push(json!({"source_id": source_id, "media_id": media_id, "start_us": a, "end_us": b}));
                }
            };
            for a in &analyzed {
                if a[0] > cur {
                    push_gap(cur, a[0].min(base_end));
                }
                cur = cur.max(a[1]);
            }
            if cur < base_end {
                push_gap(cur, base_end);
            }
        }
        let label = asset.as_ref().map_or(media_id.clone(), |a| a.uri.rsplit('/').next().unwrap_or(&a.uri).to_string());
        lanes.push(json!({
            "source_id": source_id,
            "media_id": media_id,
            "clock": clock,
            "label": label,
            "duration_us": asset.as_ref().and_then(|a| a.duration_us),
            "capture_start_utc_us": asset.as_ref().and_then(|a| a.capture_start_utc_us),
            "availability": asset.as_ref().map_or(Availability::Unknown, |a| a.availability),
            "still": asset.as_ref().is_some_and(|a| a.still),
            "coverage_known": coverage_known,
            "exact_count": exact_total,
            "matches": shown,
        }));
    }
    Ok(json!({"run_id": run_id, "start_us": start_us, "end_us": end_us, "resolution_us": resolution_us, "lanes": lanes, "gaps": gaps}))
}

mod lua;

#[cfg(test)]
mod tests;

