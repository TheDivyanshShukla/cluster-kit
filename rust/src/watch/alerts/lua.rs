//! Typed, sandboxed Lua rule runtime. One `Lua` state per rule version, compiled once; each
//! frame only builds a small `ctx` table and calls the cached function.
//!
//! Unknown handling: `ctx:count*()` returns `nil` when the value is Unknown/Unsupported. A
//! script that then compares `nil` raises a Lua error; because a nil was handed out during that
//! call the error is reported as Unknown (not as an error and never as `false`).

use crate::watch::domain::{relation_key, Count, FrameSummary, Predicate};
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Value, VmState, Variadic};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const MEM_LIMIT: usize = 4 * 1024 * 1024;
const TICK: u32 = 1000; // instructions per hook call
const MAX_TICKS: u32 = 500; // 500k VM instructions per evaluation
const MAX_SRC: usize = 16 * 1024;

#[derive(Default)]
struct Shared {
    cur: Option<Arc<FrameSummary>>,
    unknown: bool,
    diags: Vec<(String, String)>,
}

pub struct LuaRule {
    lua: Lua,
    func: Function,
    fns: Vec<(&'static str, Function)>,
    shared: Arc<Mutex<Shared>>,
    ticks: Arc<AtomicU32>,
    src: String,
}

/// (result, diagnostics as (code, message)). `Err` = rule error (fail closed).
pub type Evaluated = (Result<Option<bool>, String>, Vec<(String, String)>);

fn pred(s: Option<&String>) -> Result<Predicate, String> {
    match s.map(String::as_str) {
        Some("holding") => Ok(Predicate::Holding),
        Some("near") => Ok(Predicate::Near),
        other => Err(format!("unknown predicate {other:?}; use 'holding' or 'near'")),
    }
}

fn count_of(g: &mut Shared, what: &str, c: Count) -> Option<f64> {
    match c {
        Count::Known(n) => Some(n as f64),
        Count::Unknown => {
            g.diags.push(("unknown".into(), format!("{what} is unknown for this observation")));
            None
        }
        Count::Unsupported => {
            g.diags.push(("unsupported".into(), format!("{what} is not provided by the installed models")));
            None
        }
    }
}

fn method<F>(lua: &Lua, shared: &Arc<Mutex<Shared>>, f: F) -> mlua::Result<Function>
where
    F: Fn(&FrameSummary, &[String], &mut Shared) -> Result<Option<f64>, String> + Send + 'static,
{
    let shared = shared.clone();
    lua.create_function(move |_, args: Variadic<Value>| {
        // Accept both ctx.count("x") and ctx:count("x"): only strings are arguments.
        let strs: Vec<String> = args
            .iter()
            .filter_map(|v| v.as_str().map(|s| { let x: &str = &s; x.to_string() }))
            .collect();
        let mut g = shared.lock().unwrap();
        let Some(cur) = g.cur.clone() else { return Ok(Value::Nil) };
        match f(&cur, &strs, &mut g).map_err(mlua::Error::runtime)? {
            Some(n) if n.fract() == 0.0 => Ok(Value::Integer(n as i64)),
            Some(n) => Ok(Value::Number(n)),
            None => {
                g.unknown = true;
                Ok(Value::Nil)
            }
        }
    })
}

impl LuaRule {
    pub fn new(src: &str) -> Result<Self, String> {
        let t = src.trim();
        if t.is_empty() || t.len() > MAX_SRC {
            return Err(format!("Lua rule must contain 1..{MAX_SRC} bytes"));
        }
        let e = |e: mlua::Error| e.to_string();
        let lua = Lua::new_with(StdLib::MATH | StdLib::STRING | StdLib::TABLE, LuaOptions::new()).map_err(e)?;
        lua.set_memory_limit(MEM_LIMIT).map_err(e)?;
        // The base library is always present; remove everything that loads code or touches the host.
        let g = lua.globals();
        for name in ["load", "loadfile", "loadstring", "dofile", "require", "collectgarbage", "print", "debug", "io", "os", "package"] {
            g.set(name, Value::Nil).map_err(e)?;
        }
        if let Ok(string) = g.get::<mlua::Table>("string") {
            string.set("dump", Value::Nil).map_err(e)?;
        }
        let ticks = Arc::new(AtomicU32::new(0));
        let t2 = ticks.clone();
        let _ = lua.set_hook(
            HookTriggers { every_nth_instruction: Some(TICK), ..HookTriggers::default() },
            move |_, _| {
                if t2.fetch_add(1, Ordering::Relaxed) >= MAX_TICKS {
                    Err(mlua::Error::runtime("budget exceeded"))
                } else {
                    Ok(VmState::Continue)
                }
            },
        );
        let shared = Arc::new(Mutex::new(Shared::default()));
        let fns = vec![
            ("count", method(&lua, &shared, |s, a, g| {
                let c = a.first().ok_or("count(class) needs a class name")?;
                Ok(count_of(g, &format!("count('{c}')"), s.class(c)))
            }).map_err(e)?),
            ("count_people", method(&lua, &shared, |s, _, g| Ok(count_of(g, "count_people()", s.class("person")))).map_err(e)?),
            ("count_relation", method(&lua, &shared, |s, a, g| {
                let p = pred(a.first())?;
                let c = a.get(1).ok_or("count_relation(pred, class) needs a class name")?;
                Ok(count_of(g, &relation_key(p, c), s.relation(p, c)))
            }).map_err(e)?),
            ("ratio_people_with", method(&lua, &shared, |s, a, g| {
                let p = pred(a.first())?;
                let c = a.get(1).ok_or("ratio_people_with(pred, class) needs a class name")?;
                let num = count_of(g, &relation_key(p, c), s.relation(p, c));
                let den = count_of(g, "count_people()", s.class("person"));
                Ok(match (num, den) {
                    (Some(n), Some(d)) if d > 0.0 => Some(n / d),
                    (Some(_), Some(_)) => {
                        g.diags.push(("zero_denominator".into(), "no people in this observation; ratio is unknown".into()));
                        None
                    }
                    _ => None,
                })
            }).map_err(e)?),
        ];
        // Chunk load runs under the same budget (a top-level `while true` must not hang).
        let code = if t.starts_with("return") { t.to_string() } else { format!("return function(ctx) return ({t}) end") };
        let func = match lua.load(&code).into_function().and_then(|f| f.call::<Value>(())).map_err(e)? {
            Value::Function(f) => f,
            _ => return Err("Lua rule must be `return function(ctx) ... end` or a boolean expression over ctx".into()),
        };
        Ok(LuaRule { lua, func, fns, shared, ticks, src: src.to_string() })
    }

    pub fn eval(&mut self, s: &Arc<FrameSummary>) -> Evaluated {
        {
            let mut g = self.shared.lock().unwrap();
            g.cur = Some(s.clone());
            g.unknown = false;
            g.diags.clear();
        }
        self.ticks.store(0, Ordering::Relaxed);
        let r = (|| -> mlua::Result<Value> {
            let ctx = self.lua.create_table()?;
            for (k, f) in &self.fns {
                ctx.set(*k, f.clone())?;
            }
            ctx.set("source_id", s.source_id.as_str())?;
            ctx.set("zone", s.zone.as_deref())?;
            ctx.set("media_time_us", s.media_time_us)?;
            self.func.call::<Value>(ctx)
        })();
        let (unknown, diags) = {
            let mut g = self.shared.lock().unwrap();
            g.cur = None;
            (g.unknown, std::mem::take(&mut g.diags))
        };
        match r {
            // A script that was handed a nil and still answers false cannot be trusted
            // (`nil == 0` is false in Lua): Unknown, never a false-zero.
            Ok(Value::Boolean(false)) if unknown => (Ok(None), diags),
            Ok(Value::Boolean(b)) => (Ok(Some(b)), diags),
            Ok(Value::Nil) => (Ok(None), diags),
            Ok(_) => (Err("rule must return a boolean or nil".into()), diags),
            Err(err) => {
                let m = err.to_string();
                let fatal = m.contains("budget exceeded") || m.contains("not enough memory") || matches!(err, mlua::Error::MemoryError(_));
                if fatal {
                    // The state may be half-built after a memory/instruction abort: start clean.
                    if let Ok(fresh) = LuaRule::new(&self.src) {
                        *self = fresh;
                    }
                    let why = if m.contains("budget exceeded") { "budget exceeded" } else { "memory limit exceeded" };
                    (Err(why.into()), diags)
                } else if unknown {
                    (Ok(None), diags)
                } else {
                    (Err(m), diags)
                }
            }
        }
    }
}
