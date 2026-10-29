//! Find over the sighting log. Words, a template, or one sandboxed Lua script.
//! The script runs when someone searches. It does not run on each frame.

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value};

#[derive(Clone)]
pub struct HitIn {
    pub id: u64,
    pub ts: f64,
    pub at: f64,
    pub camera: String,
    pub camera_id: String,
    pub class: String,
    pub color: String,
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
        "lua" => lua_ids(&pool, &q.lua),
        "template" => Ok(pool.iter().filter(|h| template(h, q)).map(|h| h.id).collect()),
        _ => Ok(pool.iter().filter(|h| words(h, &q.words)).map(|h| h.id).collect()),
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
    raw.split_whitespace().all(|w| blob.contains(&w.to_ascii_lowercase()))
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
    let lua = Lua::new_with(StdLib::STRING | StdLib::MATH | StdLib::TABLE, LuaOptions::new()).map_err(|e| e.to_string())?;
    lua.set_hook(
        HookTriggers { every_nth_instruction: Some(20_000), ..HookTriggers::default() },
        |_lua, _dbg| Err(mlua::Error::external("stopped: the script ran too long")),
    );
    let rows = lua.create_table().map_err(|e| e.to_string())?;
    for h in hits {
        rows.push(hit_table(&lua, h)?).map_err(|e| e.to_string())?;
    }
    lua.globals().set("hits", rows).map_err(|e| e.to_string())?;
    let value: Value = lua.load(&wrapped).eval().map_err(|e| e.to_string())?;
    match value {
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
    }
}

fn hit_table(lua: &Lua, h: &HitIn) -> Result<mlua::Table, String> {
    let t = lua.create_table().map_err(|e| e.to_string())?;
    t.set("id", h.id as i64).map_err(|e| e.to_string())?;
    t.set("class", h.class.as_str()).map_err(|e| e.to_string())?;
    t.set("color", h.color.as_str()).map_err(|e| e.to_string())?;
    t.set("camera", h.camera.as_str()).map_err(|e| e.to_string())?;
    t.set("camera_id", h.camera_id.as_str()).map_err(|e| e.to_string())?;
    t.set("ts", h.ts).map_err(|e| e.to_string())?;
    t.set("at", h.at).map_err(|e| e.to_string())?;
    Ok(t)
}

fn truthy(v: Value) -> bool {
    match v {
        Value::Nil | Value::Boolean(false) => false,
        Value::Integer(0) => false,
        _ => true,
    }
}

fn ids_from_table(t: &mlua::Table) -> Result<Vec<u64>, String> {
    let mut ids = Vec::new();
    for v in t.sequence_values::<Value>() {
        let v = v.map_err(|e| e.to_string())?;
        match v {
            Value::Integer(n) if n > 0 => ids.push(n as u64),
            Value::Number(n) if n > 0.0 => ids.push(n as u64),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u64, class: &str, color: &str, at: f64, ts: f64) -> HitIn {
        HitIn { id, ts, at, camera: "Street".into(), camera_id: "street".into(), class: class.into(), color: color.into() }
    }

    #[test]
    fn words_need_every_term() {
        let hits = vec![hit(1, "person", "blue", 3.0, 100.0), hit(2, "bicycle", "gray", 8.0, 200.0)];
        let q = Ask { mode: "words".into(), words: "blue person".into(), class: String::new(), color: String::new(), camera: String::new(), from: 0.0, to: 0.0, lua: String::new() };
        assert_eq!(ask(&hits, &q).unwrap(), vec![1]);
    }

    #[test]
    fn time_window_drops_the_rest() {
        let hits = vec![hit(1, "person", "blue", 3.0, 100.0), hit(2, "person", "blue", 8.0, 500.0)];
        let q = Ask { mode: "words".into(), words: "person".into(), class: String::new(), color: String::new(), camera: String::new(), from: 400.0, to: 600.0, lua: String::new() };
        assert_eq!(ask(&hits, &q).unwrap(), vec![2]);
    }

    #[test]
    fn lua_reads_media_time() {
        let hits = vec![hit(1, "person", "blue", 3.0, 100.0), hit(2, "person", "blue", 12.0, 100.0)];
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
}
