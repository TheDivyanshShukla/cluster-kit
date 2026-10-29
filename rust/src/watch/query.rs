//! Find over the sighting log. Words, a template, or one sandboxed Lua script.
//! The script runs when someone searches. It does not run on each frame.

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value};

pub mod v1;

#[derive(Clone)]
pub struct HitIn {
    pub id: u64,
    pub ts: f64,
    pub at: f64,
    pub camera: String,
    pub camera_id: String,
    pub class: String,
    pub color: String,
    pub confidence: f32,
    pub track_id: u64,
    pub model: String,
    pub near_person: bool,
}

pub struct Ask {
    pub mode: String,
    pub words: String,
    pub class: String,
    pub color: String,
    pub camera: String,
    pub from: f64,
    pub to: f64,
    pub lua: String,
}

pub fn ask(hits: &[HitIn], q: &Ask) -> Result<Vec<u64>, String> {
    let pool: Vec<&HitIn> = hits.iter().filter(|h| in_window(h, q)).collect();
    match q.mode.as_str() {
        "sql" => sql(
            &pool.iter().map(|h| (*h).clone()).collect::<Vec<_>>(),
            &q.words,
        )
        .map(|r| r.ids),
        "lua" => lua_ids(&pool, &q.lua),
        "template" => Ok(pool
            .iter()
            .filter(|h| template(h, q))
            .map(|h| h.id)
            .collect()),
        _ => Ok(pool
            .iter()
            .filter(|h| words(h, &q.words))
            .map(|h| h.id)
            .collect()),
    }
}

fn in_window(h: &HitIn, q: &Ask) -> bool {
    if q.from > 0.0 && h.ts < q.from {
        return false;
    }
    if q.to > 0.0 && h.ts > q.to {
        return false;
    }
    let cam = q.camera.trim().to_ascii_lowercase();
    if cam.is_empty() {
        return true;
    }
    h.camera.to_ascii_lowercase().contains(&cam) || h.camera_id.to_ascii_lowercase().contains(&cam)
}

fn words(h: &HitIn, raw: &str) -> bool {
    let blob = format!("{} {} {}", h.class, h.color, h.camera).to_ascii_lowercase();
    raw.split_whitespace()
        .all(|w| blob.contains(&w.to_ascii_lowercase()))
}

fn template(h: &HitIn, q: &Ask) -> bool {
    let class = q.class.trim().to_ascii_lowercase();
    let color = q.color.trim().to_ascii_lowercase();
    (class.is_empty() || h.class.to_ascii_lowercase().contains(&class))
        && (color.is_empty() || h.color.to_ascii_lowercase().contains(&color))
}

fn lua_ids(hits: &[&HitIn], code: &str) -> Result<Vec<u64>, String> {
    let code = code.trim();
    if code.is_empty() {
        return Ok(hits.iter().map(|h| h.id).collect());
    }
    let wrapped = if code.contains("return") {
        code.to_string()
    } else {
        format!("return function(hit)\nreturn {code}\nend")
    };
    let lua = Lua::new_with(
        StdLib::STRING | StdLib::MATH | StdLib::TABLE,
        LuaOptions::new(),
    )
    .map_err(|e| e.to_string())?;
    lua.set_memory_limit(8 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    lua.set_hook(
        HookTriggers {
            every_nth_instruction: Some(20_000),
            ..HookTriggers::default()
        },
        |_lua, _dbg| Err(mlua::Error::external("stopped: the script ran too long")),
    );
    let rows = lua.create_table().map_err(|e| e.to_string())?;
    for h in hits {
        rows.push(hit_table(&lua, h)?).map_err(|e| e.to_string())?;
    }
    lua.globals().set("hits", rows).map_err(|e| e.to_string())?;
    let value: Value = lua.load(&wrapped).eval().map_err(|e| e.to_string())?;
    let ids = match value {
        Value::Function(f) => {
            let mut ids = Vec::new();
            for h in hits {
                let row = hit_table(&lua, h)?;
                let keep: Value = f.call(row).map_err(|e| e.to_string())?;
                if truthy(keep) {
                    ids.push(h.id);
                }
            }
            Ok(ids)
        }
        Value::Table(t) => ids_from_table(&t),
        _ => Err("the script should return a function(hit) or a list of ids".into()),
    }?;
    let allowed: std::collections::HashSet<u64> = hits.iter().map(|h| h.id).collect();
    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter(|id| allowed.contains(id) && seen.insert(*id))
        .collect())
}

fn hit_table(lua: &Lua, h: &HitIn) -> Result<mlua::Table, String> {
    let t = lua.create_table().map_err(|e| e.to_string())?;
    t.set("id", h.id as i64).map_err(|e| e.to_string())?;
    t.set("class", h.class.as_str())
        .map_err(|e| e.to_string())?;
    t.set("color", h.color.as_str())
        .map_err(|e| e.to_string())?;
    t.set("camera", h.camera.as_str())
        .map_err(|e| e.to_string())?;
    t.set("camera_id", h.camera_id.as_str())
        .map_err(|e| e.to_string())?;
    t.set("ts", h.ts).map_err(|e| e.to_string())?;
    t.set("at", h.at).map_err(|e| e.to_string())?;
    t.set("confidence", h.confidence)
        .map_err(|e| e.to_string())?;
    t.set("track_id", h.track_id).map_err(|e| e.to_string())?;
    t.set("model", h.model.as_str())
        .map_err(|e| e.to_string())?;
    t.set("near_person", h.near_person)
        .map_err(|e| e.to_string())?;
    Ok(t)
}

fn truthy(v: Value) -> bool {
    match v {
        Value::Nil | Value::Boolean(false) => false,
        _ => true,
    }
}

fn ids_from_table(t: &mlua::Table) -> Result<Vec<u64>, String> {
    let mut ids = Vec::new();
    for v in t.sequence_values::<Value>() {
        let v = v.map_err(|e| e.to_string())?;
        match v {
            Value::Integer(n) if n > 0 => ids.push(n as u64),
            Value::Number(n)
                if n.is_finite() && n > 0.0 && n.fract() == 0.0 && n < u64::MAX as f64 =>
            {
                ids.push(n as u64)
            }
            Value::Table(row) => {
                let id: i64 = row.get("id").unwrap_or(0);
                if id > 0 {
                    ids.push(id as u64);
                }
            }
            _ => {}
        }
    }
    Ok(ids)
}

/// SQL over indexed sighting rows. Counts are observations, not distinct people.
#[derive(Debug, serde::Serialize)]
pub struct SqlResult {
    pub ids: Vec<u64>,
    pub count: usize,
    pub groups: Vec<SqlGroup>,
    pub aggregate: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct SqlGroup {
    pub camera_id: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Text(String),
    Num(f64),
    Symbol(char),
    Op(String),
}

#[derive(Clone, Debug)]
enum Predicate {
    Compare(String, String, Token),
    In(String, Vec<Token>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

struct SqlParser {
    tokens: Vec<Token>,
    at: usize,
}
impl SqlParser {
    fn word(&mut self, s: &str) -> bool {
        if matches!(self.tokens.get(self.at), Some(Token::Word(w)) if w.eq_ignore_ascii_case(s)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn need(&mut self, s: &str) -> Result<(), String> {
        if self.word(s) {
            Ok(())
        } else {
            Err(format!("expected {s}"))
        }
    }
    fn symbol(&mut self, c: char) -> bool {
        if self.tokens.get(self.at) == Some(&Token::Symbol(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Result<String, String> {
        match self.tokens.get(self.at).cloned() {
            Some(Token::Word(s)) => {
                self.at += 1;
                Ok(s.to_ascii_lowercase())
            }
            _ => Err("expected a field name".into()),
        }
    }
    fn value(&mut self) -> Result<Token, String> {
        let v = self
            .tokens
            .get(self.at)
            .cloned()
            .ok_or("expected a value")?;
        if matches!(&v, Token::Text(_) | Token::Num(_))
            || matches!(&v, Token::Word(w) if w.eq_ignore_ascii_case("true") || w.eq_ignore_ascii_case("false"))
        {
            self.at += 1;
            Ok(v)
        } else {
            Err("values must be quoted strings, numbers, true or false".into())
        }
    }
    fn expr(&mut self, depth: usize) -> Result<Predicate, String> {
        let mut p = self.and(depth)?;
        while self.word("OR") {
            p = Predicate::Or(Box::new(p), Box::new(self.and(depth)?));
        }
        Ok(p)
    }
    fn and(&mut self, depth: usize) -> Result<Predicate, String> {
        let mut p = self.primary(depth)?;
        while self.word("AND") {
            p = Predicate::And(Box::new(p), Box::new(self.primary(depth)?));
        }
        Ok(p)
    }
    fn primary(&mut self, depth: usize) -> Result<Predicate, String> {
        if depth > 24 {
            return Err("query nesting exceeds 24 levels".into());
        }
        if self.symbol('(') {
            let p = self.expr(depth + 1)?;
            if !self.symbol(')') {
                return Err("missing closing parenthesis".into());
            }
            return Ok(p);
        }
        let field = self.name()?;
        if !matches!(
            field.as_str(),
            "id" | "ts"
                | "at"
                | "camera"
                | "camera_id"
                | "class"
                | "color"
                | "confidence"
                | "track_id"
                | "model"
                | "near_person"
        ) {
            return Err(format!("unknown sighting field: {field}"));
        }
        if self.word("IN") {
            if !self.symbol('(') {
                return Err("expected ( after IN".into());
            }
            let mut values = vec![self.value()?];
            while self.symbol(',') {
                values.push(self.value()?);
            }
            if !self.symbol(')') {
                return Err("expected ) after IN values".into());
            }
            for value in &values {
                validate_value(&field, value)?;
            }
            return Ok(Predicate::In(field, values));
        }
        let op = match self.tokens.get(self.at).cloned() {
            Some(Token::Op(op)) => {
                self.at += 1;
                op
            }
            _ => return Err("expected =, !=, <, <=, >, >= or IN".into()),
        };
        let v = self.value()?;
        validate_value(&field, &v)?;
        Ok(Predicate::Compare(field, op, v))
    }
}

fn validate_value(field: &str, value: &Token) -> Result<(), String> {
    let valid = match field {
        "id" | "ts" | "at" | "confidence" | "track_id" => matches!(value, Token::Num(_)),
        "near_person" => {
            matches!(value, Token::Word(w) if w.eq_ignore_ascii_case("true") || w.eq_ignore_ascii_case("false"))
        }
        _ => matches!(value, Token::Text(_)),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("invalid value type for {field}"))
    }
}

fn lex_sql(s: &str) -> Result<Vec<Token>, String> {
    if s.len() > 16_384 {
        return Err("SQL exceeds 16 KiB".into());
    }
    let mut chars = s.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '\'' {
            let mut text = String::new();
            loop {
                match chars.next() {
                    Some('\'') if chars.peek() == Some(&'\'') => {
                        chars.next();
                        text.push('\'');
                    }
                    Some('\'') => break,
                    Some(c) => text.push(c),
                    None => return Err("unterminated quoted string".into()),
                }
            }
            tokens.push(Token::Text(text));
        } else if c.is_ascii_digit() || c == '-' || c == '.' {
            let mut number = String::from(c);
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
            {
                number.push(chars.next().unwrap());
            }
            let n: f64 = number
                .parse()
                .map_err(|_| format!("invalid number: {number}"))?;
            if !n.is_finite() {
                return Err("numbers must be finite".into());
            }
            tokens.push(Token::Num(n));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut word = String::from(c);
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                word.push(chars.next().unwrap());
            }
            tokens.push(Token::Word(word));
        } else if matches!(c, '=' | '!' | '<' | '>') {
            let mut op = String::from(c);
            if chars.peek() == Some(&'=') {
                op.push(chars.next().unwrap());
            }
            if !matches!(op.as_str(), "=" | "!=" | "<" | "<=" | ">" | ">=") {
                return Err(format!("unsupported operator {op}"));
            }
            tokens.push(Token::Op(op));
        } else if matches!(c, '*' | '(' | ')' | ',' | ';') {
            tokens.push(Token::Symbol(c));
        } else {
            return Err(format!("unsupported SQL character: {c}"));
        }
        if tokens.len() > 1024 {
            return Err("query has too many tokens".into());
        }
    }
    Ok(tokens)
}

fn field_value(h: &HitIn, field: &str) -> Token {
    match field {
        "id" => Token::Num(h.id as f64),
        "ts" => Token::Num(h.ts),
        "at" => Token::Num(h.at),
        "confidence" => Token::Num(h.confidence as f64),
        "track_id" => Token::Num(h.track_id as f64),
        "class" => Token::Text(h.class.clone()),
        "camera" => Token::Text(h.camera.clone()),
        "camera_id" => Token::Text(h.camera_id.clone()),
        "color" => Token::Text(h.color.clone()),
        "model" => Token::Text(h.model.clone()),
        "near_person" => Token::Word(h.near_person.to_string()),
        _ => unreachable!(),
    }
}

fn compare(a: &Token, op: &str, b: &Token) -> bool {
    let ord = match (a, b) {
        (Token::Num(a), Token::Num(b)) => a.partial_cmp(b),
        (Token::Text(a), Token::Text(b)) => Some(a.cmp(b)),
        (Token::Word(a), Token::Word(b)) => {
            Some(a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
        }
        _ => None,
    };
    use std::cmp::Ordering::*;
    match op {
        "=" => ord == Some(Equal),
        "!=" => ord.is_some() && ord != Some(Equal),
        "<" => ord == Some(Less),
        "<=" => matches!(ord, Some(Less | Equal)),
        ">" => ord == Some(Greater),
        ">=" => matches!(ord, Some(Greater | Equal)),
        _ => false,
    }
}

fn matches_sql(h: &HitIn, p: &Predicate) -> bool {
    match p {
        Predicate::Compare(field, op, v) => compare(&field_value(h, field), op, v),
        Predicate::In(field, values) => values
            .iter()
            .any(|v| compare(&field_value(h, field), "=", v)),
        Predicate::And(a, b) => matches_sql(h, a) && matches_sql(h, b),
        Predicate::Or(a, b) => matches_sql(h, a) || matches_sql(h, b),
    }
}

/// Supported: SELECT *|id|COUNT(*) FROM sightings [WHERE ...] [GROUP BY camera_id] [LIMIT n].
/// Predicates support AND/OR, parentheses, IN and typed comparisons. Arbitrary SQL is rejected.
pub fn sql(hits: &[HitIn], statement: &str) -> Result<SqlResult, String> {
    let mut p = SqlParser {
        tokens: lex_sql(statement)?,
        at: 0,
    };
    p.need("SELECT")?;
    let aggregate = if p.word("COUNT") {
        if !p.symbol('(') || !p.symbol('*') || !p.symbol(')') {
            return Err("only COUNT(*) is supported".into());
        }
        true
    } else {
        if !p.symbol('*') && !p.word("id") {
            return Err("select *, id or COUNT(*)".into());
        }
        false
    };
    p.need("FROM")?;
    p.need("sightings")?;
    let predicate = if p.word("WHERE") {
        Some(p.expr(0)?)
    } else {
        None
    };
    let group = if p.word("GROUP") {
        p.need("BY")?;
        p.need("camera_id")?;
        if !aggregate {
            return Err("GROUP BY requires COUNT(*)".into());
        }
        true
    } else {
        false
    };
    let limit = if p.word("LIMIT") {
        match p.value()? {
            Token::Num(n) if n >= 0.0 && n.fract() == 0.0 && n <= 100_000.0 => n as usize,
            _ => return Err("LIMIT must be an integer from 0 to 100000".into()),
        }
    } else {
        100_000
    };
    p.symbol(';');
    if p.at != p.tokens.len() {
        return Err("unsupported SQL clause or extra statement".into());
    }
    let matched: Vec<&HitIn> = hits
        .iter()
        .filter(|h| predicate.as_ref().is_none_or(|p| matches_sql(h, p)))
        .collect();
    let count = matched.len();
    let mut groups = std::collections::BTreeMap::new();
    if group {
        for h in &matched {
            *groups.entry(h.camera_id.clone()).or_insert(0usize) += 1;
        }
    }
    let ids = matched
        .into_iter()
        .take(if aggregate { 0 } else { limit })
        .map(|h| h.id)
        .collect();
    Ok(SqlResult {
        ids,
        count,
        aggregate,
        groups: groups
            .into_iter()
            .take(limit)
            .map(|(camera_id, count)| SqlGroup { camera_id, count })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u64, class: &str, color: &str, at: f64, ts: f64) -> HitIn {
        HitIn {
            id,
            ts,
            at,
            camera: "Street".into(),
            camera_id: "street".into(),
            class: class.into(),
            color: color.into(),
            confidence: 0.9,
            track_id: id,
            model: "yolo".into(),
            near_person: false,
        }
    }

    #[test]
    fn words_need_every_term() {
        let hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "bicycle", "gray", 8.0, 200.0),
        ];
        let q = Ask {
            mode: "words".into(),
            words: "blue person".into(),
            class: String::new(),
            color: String::new(),
            camera: String::new(),
            from: 0.0,
            to: 0.0,
            lua: String::new(),
        };
        assert_eq!(ask(&hits, &q).unwrap(), vec![1]);
    }

    #[test]
    fn time_window_drops_the_rest() {
        let hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "person", "blue", 8.0, 500.0),
        ];
        let q = Ask {
            mode: "words".into(),
            words: "person".into(),
            class: String::new(),
            color: String::new(),
            camera: String::new(),
            from: 400.0,
            to: 600.0,
            lua: String::new(),
        };
        assert_eq!(ask(&hits, &q).unwrap(), vec![2]);
    }

    #[test]
    fn lua_reads_media_time() {
        let hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "person", "blue", 12.0, 100.0),
        ];
        let q = Ask {
            mode: "lua".into(),
            words: String::new(),
            class: String::new(),
            color: String::new(),
            camera: String::new(),
            from: 0.0,
            to: 0.0,
            lua: "return function(hit) return hit.class == \"person\" and hit.at >= 5 and hit.at <= 20 end".into(),
        };
        assert_eq!(ask(&hits, &q).unwrap(), vec![2]);
    }
    #[test]
    fn sql_filters_confidence_and_groups_records() {
        let mut hits = vec![
            hit(1, "cell phone", "black", 3.0, 100.0),
            hit(2, "cell phone", "black", 8.0, 200.0),
            hit(3, "person", "blue", 8.0, 200.0),
        ];
        hits[1].confidence = 0.4;
        let result = sql(&hits, "SELECT * FROM sightings WHERE class IN ('cell phone', 'knife') AND confidence >= 0.8 LIMIT 5;").unwrap();
        assert_eq!(result.ids, vec![1]);
        let result = sql(
            &hits,
            "SELECT COUNT(*) FROM sightings WHERE class = 'cell phone' GROUP BY camera_id",
        )
        .unwrap();
        assert!(result.aggregate);
        assert_eq!(result.count, 2);
        assert_eq!(result.groups[0].count, 2);
        assert_eq!(result.groups[0].camera_id, "street");
    }

    #[test]
    fn sql_operator_precedence_and_limit() {
        let hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "bicycle", "gray", 8.0, 200.0),
        ];
        assert_eq!(
            sql(
                &hits,
                "SELECT id FROM sightings WHERE class = 'person' OR class = 'bicycle' AND at > 10"
            )
            .unwrap()
            .ids,
            vec![1]
        );
        assert_eq!(sql(&hits, "SELECT id FROM sightings WHERE (class = 'person' OR class = 'bicycle') AND at > 5 LIMIT 1").unwrap().ids, vec![2]);
        assert!(sql(&hits, "SELECT * FROM sightings LIMIT 0")
            .unwrap()
            .ids
            .is_empty());
    }

    #[test]
    fn sql_rejects_unknown_or_unsafe_semantics() {
        assert!(sql(&[], "SELECT * FROM sightings WHERE holding = true").is_err());
        assert!(sql(&[], "SELECT * FROM sightings WHERE confidence = 'high'").is_err());
        assert!(sql(&[], "SELECT * FROM sightings; DELETE FROM sightings").is_err());
        assert!(sql(&[], "SELECT * FROM sightings LIMIT 1.5").is_err());
        assert!(sql(&[], "SELECT COUNT(person) FROM sightings").is_err());
        assert!(sql(&[], "SELECT * FROM sightings WHERE class = 'unterminated").is_err());
    }

    #[test]
    fn lua_cannot_escape_filtered_ids_or_loop_forever() {
        let hits = vec![hit(1, "person", "blue", 3.0, 100.0)];
        let pool = hits.iter().collect::<Vec<_>>();
        assert_eq!(lua_ids(&pool, "return {1, 999, 1}").unwrap(), vec![1]);
        assert!(lua_ids(&pool, "while true do end; return {}").is_err());
        assert!(lua_ids(&pool, "return os.execute('true')").is_err());
    }
    #[test]
    fn sql_empty_count_is_zero_and_grouping_has_no_phantom_rows() {
        let result = sql(&[], "SELECT COUNT(*) FROM sightings").unwrap();
        assert_eq!(result.count, 0);
        assert!(result.aggregate && result.ids.is_empty() && result.groups.is_empty());
        assert!(
            sql(&[], "SELECT COUNT(*) FROM sightings GROUP BY camera_id")
                .unwrap()
                .groups
                .is_empty()
        );
    }

    #[test]
    fn sql_group_count_precedes_group_limit_and_names_are_exact() {
        let mut hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "person", "blue", 8.0, 200.0),
            hit(3, "person", "blue", 8.0, 200.0),
        ];
        hits[0].camera_id = "room-a".into();
        hits[1].camera_id = "room-a".into();
        hits[2].camera_id = "room-b".into();
        let result = sql(
            &hits,
            "SELECT COUNT(*) FROM sightings GROUP BY camera_id LIMIT 1",
        )
        .unwrap();
        assert_eq!(result.count, 3);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].camera_id, "room-a");
        assert_eq!(result.groups[0].count, 2);
        assert!(
            sql(&hits, "SELECT * FROM sightings WHERE camera_id = 'room'")
                .unwrap()
                .ids
                .is_empty()
        );
    }

    #[test]
    fn sql_mode_preserves_outer_camera_and_time_scope() {
        let hits = vec![
            hit(1, "person", "blue", 3.0, 100.0),
            hit(2, "person", "blue", 8.0, 200.0),
        ];
        let q = Ask {
            mode: "sql".into(),
            words: "SELECT * FROM sightings".into(),
            class: String::new(),
            color: String::new(),
            camera: "street".into(),
            from: 150.0,
            to: 250.0,
            lua: String::new(),
        };
        assert_eq!(ask(&hits, &q).unwrap(), vec![2]);
    }

    #[test]
    fn sql_text_unicode_quotes_boolean_and_typed_numbers() {
        let mut h = hit(1, "cell phone", "black", 3.5, 100.0);
        h.camera = "École d'Art".into();
        h.near_person = true;
        assert_eq!(sql(&[h.clone()], "select id from sightings where camera = 'École d''Art' AND near_person = TRUE AND at >= 3.5").unwrap().ids, vec![1]);
        assert!(sql(&[h], "SELECT * FROM sightings WHERE confidence > 1e999").is_err());
        assert!(sql(&[], "SELECT * FROM sightings ORDER BY ts").is_err());
        assert!(sql(&[], "SELECT * FROM sightings WHERE near_person = 1").is_err());
    }

    #[test]
    fn sql_parser_rejects_excessive_input_and_nesting() {
        assert!(sql(&[], &" ".repeat(16385)).is_err());
        let nested = format!(
            "SELECT * FROM sightings WHERE {}class = 'person'{}",
            "(".repeat(26),
            ")".repeat(26)
        );
        assert!(sql(&[], &nested).is_err());
        let wide = format!(
            "SELECT * FROM sightings WHERE {}class = 'person'",
            "class = 'person' OR ".repeat(300)
        );
        assert!(sql(&[], &wide).is_err());
    }
}
