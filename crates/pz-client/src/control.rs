//! `--control`: the game reads JSON commands from stdin, one per line, and
//! answers each with one stdout line `@@pzctl <json>`. Everything else on
//! stdout/stderr is the game's own log. `pz-ctl` drives this mode; the
//! protocol is documented in `docs/AGENT-CONTROL.md`.

use pz_engine::game_window::control::{Controller, Snapshot, UiNode};
use pz_engine::game_window::Launch;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::path::Path;

/// The prefix of every protocol line on stdout.
pub const REPLY_PREFIX: &str = "@@pzctl ";

/// Lua helpers installed after boot: `pzctl_json(v)` encodes a value as
/// JSON; `pzctl_player()` reads what the Rust side cannot see directly.
const LUA_HELPERS: &str = r#"
local function esc(s)
    s = string.gsub(s, '\\', '\\\\')
    s = string.gsub(s, '"', '\\"')
    s = string.gsub(s, '\n', '\\n')
    s = string.gsub(s, '\r', '\\r')
    s = string.gsub(s, '\t', '\\t')
    return '"' .. s .. '"'
end
local function enc(v, depth)
    local t = type(v)
    if v == nil then return 'null' end
    if t == 'boolean' then return tostring(v) end
    if t == 'number' then
        if v ~= v or v == math.huge or v == -math.huge then return 'null' end
        return tostring(v)
    end
    if t == 'string' then return esc(v) end
    if t == 'table' and depth < 6 then
        local n = #v
        local parts = {}
        if n > 0 then
            for i = 1, n do parts[i] = enc(v[i], depth + 1) end
            return '[' .. table.concat(parts, ',') .. ']'
        end
        for k, x in pairs(v) do
            parts[#parts + 1] = esc(tostring(k)) .. ':' .. enc(x, depth + 1)
        end
        return '{' .. table.concat(parts, ',') .. '}'
    end
    return esc(tostring(v))
end
function pzctl_json(v) return enc(v, 0) end
function pzctl_player()
    local r = { errors = {} }
    local ok, p = pcall(getPlayer)
    if not ok then r.errors[#r.errors + 1] = 'getPlayer: ' .. tostring(p) return pzctl_json(r) end
    if not p then return pzctl_json(r) end
    local function try(name, f)
        local ok2, v = pcall(f)
        if ok2 then r[name] = v else r.errors[#r.errors + 1] = name .. ': ' .. tostring(v) end
    end
    try('health', function() return p:getHealth() end)
    try('dead', function() return p:isDead() end)
    try('inventory', function()
        local items = p:getInventory():getItems()
        local out = {}
        for i = 0, items:size() - 1 do out[#out + 1] = items:get(i):getFullType() end
        return out
    end)
    return pzctl_json(r)
end
"#;

pub fn run(launch: Launch, gpu: bool) -> i32 {
    let mut ctl = match Controller::start(launch, gpu) {
        Ok(c) => c,
        Err(e) => {
            reply(&json!({"event": "boot_failed", "ok": false, "error": e.to_string()}));
            return 1;
        }
    };
    if let Err(e) = ctl.eval(LUA_HELPERS) {
        eprintln!("pz-ctl: Lua helpers: {e}");
    }
    reply(&with_status(&ctl, json!({"event": "ready", "ok": true})));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let cmd: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                reply(&json!({"ok": false, "error": format!("bad JSON: {e}")}));
                continue;
            }
        };
        if cmd["cmd"] == "quit" {
            let frame = ctl.snapshot().frame;
            let r = match ctl.quit() {
                Ok(()) => json!({"cmd": "quit", "ok": true, "frame": frame}),
                Err(e) => json!({"cmd": "quit", "ok": false, "frame": frame, "error": e.to_string()}),
            };
            reply(&r);
            return 0;
        }
        let r = match handle(&mut ctl, &cmd) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e}),
        };
        let mut r = with_status(&ctl, r);
        if let (Some(m), Some(c)) = (r.as_object_mut(), cmd.get("cmd")) {
            m.insert("cmd".into(), c.clone());
            if let Some(id) = cmd.get("id") {
                m.insert("id".into(), id.clone());
            }
        }
        reply(&r);
    }
    // stdin closed without `quit`: close the game the same way.
    let _ = ctl.quit();
    0
}

fn reply(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{REPLY_PREFIX}{v}");
    let _ = out.flush();
}

/// Every reply carries where the game is.
fn with_status(ctl: &Controller, mut v: Value) -> Value {
    let s = ctl.snapshot();
    if let Some(m) = v.as_object_mut() {
        m.entry("ok").or_insert(Value::Bool(true));
        m.insert("frame".into(), json!(s.frame));
        m.insert("state".into(), json!(s.state));
        m.insert("engine_error".into(), json!(s.error));
    }
    v
}

fn handle(ctl: &mut Controller, cmd: &Value) -> Result<Value, String> {
    let n = |k: &str, d: u64| cmd.get(k).and_then(Value::as_u64).unwrap_or(d) as u32;
    let f = |k: &str| cmd.get(k).and_then(Value::as_f64).ok_or(format!("missing number \"{k}\""));
    match cmd["cmd"].as_str().unwrap_or("") {
        "ping" => Ok(json!({})),
        "frames" => Ok(json!({"ran": ctl.frames(n("n", 1))})),
        "key" => {
            let code = key_code(ctl, cmd.get("key").ok_or("missing \"key\"")?)?;
            let ran = match cmd["action"].as_str().unwrap_or("tap") {
                "down" => {
                    ctl.key(code, true);
                    0
                }
                "up" => {
                    ctl.key(code, false);
                    0
                }
                "tap" | "hold" => {
                    ctl.key(code, true);
                    let ran = ctl.frames(n("frames", 1).max(1));
                    ctl.key(code, false);
                    ran + ctl.frames(1)
                }
                other => return Err(format!("unknown key action \"{other}\"")),
            };
            Ok(json!({"code": code, "ran": ran}))
        }
        "text" => {
            for c in cmd["text"].as_str().ok_or("missing \"text\"")?.chars() {
                ctl.char(c);
            }
            Ok(json!({"ran": ctl.frames(1)}))
        }
        "mouse" => {
            ctl.mouse_move(f("x")?, f("y")?);
            Ok(json!({"ran": ctl.frames(n("frames", 1))}))
        }
        "click" => {
            let button = match cmd["button"].as_str().unwrap_or("left") {
                "left" => 0,
                "right" => 1,
                "middle" => 2,
                other => return Err(format!("unknown button \"{other}\"")),
            };
            ctl.mouse_move(f("x")?, f("y")?);
            let mut ran = ctl.frames(1);
            ctl.mouse_button(button, true);
            ran += ctl.frames(n("frames", 1).max(1));
            ctl.mouse_button(button, false);
            ran += ctl.frames(1);
            Ok(json!({"ran": ran}))
        }
        "resize" => {
            ctl.resize(f("w")? as u32, f("h")? as u32);
            Ok(json!({"ran": ctl.frames(n("frames", 1))}))
        }
        "screenshot" => {
            let path = cmd["path"].as_str().ok_or("missing \"path\"")?;
            let (w, h, px) = ctl.pixels().ok_or("no GPU (started with --no-gpu)")?;
            write_png(Path::new(path), w, h, &px)?;
            Ok(json!({"path": path, "width": w, "height": h}))
        }
        "snapshot" => Ok(snapshot_json(ctl)),
        "lua" => {
            let code = cmd["code"].as_str().ok_or("missing \"code\"")?;
            let result = ctl.eval(code)?;
            let parsed = result.first().and_then(|s| serde_json::from_str::<Value>(s).ok());
            let mut r = json!({"result": result});
            if let Some(p) = parsed.filter(|p| p.is_object() || p.is_array()) {
                r["json"] = p;
            }
            Ok(r)
        }
        "" => Err("missing \"cmd\"".into()),
        other => Err(format!("unknown command \"{other}\"")),
    }
}

/// A key as an LWJGL code, or a `Keyboard.KEY_<NAME>` name (`"W"`,
/// `"ESCAPE"`, `"SPACE"`, `"F1"`).
fn key_code(ctl: &mut Controller, key: &Value) -> Result<i32, String> {
    if let Some(n) = key.as_i64() {
        return Ok(n as i32);
    }
    let name = key.as_str().ok_or("\"key\" is a code or a name")?.to_ascii_uppercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("bad key name \"{name}\""));
    }
    let r = ctl.eval(&format!("return Keyboard.KEY_{name}"))?;
    r.first()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|n| n as i32)
        .ok_or(format!("unknown key \"{name}\" (no Keyboard.KEY_{name})"))
}

pub fn snapshot_json(ctl: &mut Controller) -> Value {
    let s: Snapshot = ctl.snapshot();
    let lua_player = if s.player.is_some() {
        ctl.eval("return pzctl_player()")
            .ok()
            .and_then(|r| r.first().and_then(|j| serde_json::from_str::<Value>(j).ok()))
    } else {
        None
    };
    let player = s.player.as_ref().map(|p| {
        let mut m = Map::new();
        m.insert("x".into(), json!(p.x));
        m.insert("y".into(), json!(p.y));
        m.insert("z".into(), json!(p.z));
        m.insert("action_state".into(), json!(p.action_state));
        if let Some(Value::Object(extra)) = &lua_player {
            for (k, v) in extra {
                m.insert(k.clone(), v.clone());
            }
        }
        Value::Object(m)
    });
    json!({
        "snapshot": {
            "frame": s.frame,
            "state": s.state,
            "paused": s.paused,
            "engine_error": s.error,
            "screen": {"w": s.screen.0, "h": s.screen.1},
            "player": player,
            "zombies": zombies_json(&s, 30.0, 20),
            "ui": s.ui.iter().map(ui_json).collect::<Vec<_>>(),
        }
    })
}

/// The zombie count and the `limit` nearest within `radius` tiles of the
/// player (all of them by distance from 0,0 without a player).
pub fn zombies_json(s: &Snapshot, radius: f32, limit: usize) -> Value {
    let (px, py) = s.player.as_ref().map_or((0.0, 0.0), |p| (p.x, p.y));
    let mut near: Vec<(f32, [f32; 3])> = s
        .zombies
        .iter()
        .map(|z| (((z[0] - px).powi(2) + (z[1] - py).powi(2)).sqrt(), *z))
        .filter(|(d, _)| s.player.is_none() || *d <= radius)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(limit);
    json!({
        "total": s.zombies.len(),
        "radius": radius,
        "nearby": near.iter().map(|(d, z)| json!({"x": z[0], "y": z[1], "z": z[2], "dist": d})).collect::<Vec<_>>(),
    })
}

pub fn ui_json(n: &UiNode) -> Value {
    let mut v = json!({
        "class": n.class,
        "type": n.lua_type,
        "rect": n.rect,
    });
    if let Some(t) = &n.text {
        v["text"] = json!(t);
    }
    if !n.children.is_empty() {
        v["children"] = Value::Array(n.children.iter().map(ui_json).collect());
    }
    v
}

pub fn write_png(path: &Path, w: u32, h: u32, px: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(px))
        .map_err(|e| format!("png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(player: Option<(f32, f32)>, zombies: Vec<[f32; 3]>) -> Snapshot {
        Snapshot {
            player: player.map(|(x, y)| pz_engine::game_window::control::PlayerInfo { x, y, ..Default::default() }),
            zombies,
            ..Default::default()
        }
    }

    #[test]
    fn zombies_near_player_sorted_and_limited() {
        let s = snap(Some((100.0, 100.0)), vec![[110.0, 100.0, 0.0], [101.0, 100.0, 0.0], [500.0, 500.0, 0.0], [103.0, 100.0, 0.0]]);
        let v = zombies_json(&s, 30.0, 2);
        assert_eq!(v["total"], 4);
        let near = v["nearby"].as_array().unwrap();
        assert_eq!(near.len(), 2);
        assert_eq!(near[0]["x"], 101.0);
        assert_eq!(near[1]["x"], 103.0);
    }

    #[test]
    fn zombies_without_player_are_listed() {
        let s = snap(None, vec![[5.0, 0.0, 0.0]]);
        assert_eq!(zombies_json(&s, 30.0, 20)["nearby"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn ui_json_keeps_text_and_children() {
        let n = UiNode {
            class: "zombie.ui.UIElement".into(),
            lua_type: Some("MainScreen".into()),
            text: None,
            rect: [0.0, 0.0, 10.0, 10.0],
            children: vec![UiNode { text: Some("SOLO".into()), ..Default::default() }],
        };
        let v = ui_json(&n);
        assert_eq!(v["type"], "MainScreen");
        assert!(v.get("text").is_none());
        assert_eq!(v["children"][0]["text"], "SOLO");
    }

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("pzctl-png-{}", std::process::id()));
        let p = dir.join("a.png");
        write_png(&p, 2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        let d = png::Decoder::new(std::fs::File::open(&p).unwrap());
        let r = d.read_info().unwrap();
        assert_eq!((r.info().width, r.info().height), (2, 1));
        let _ = std::fs::remove_dir_all(dir);
    }
}
