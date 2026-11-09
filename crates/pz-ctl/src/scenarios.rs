//! Pass/fail checks of what a player does first. Each scenario starts its
//! own game process, so one failure cannot spoil the next.

use crate::client::{Client, Options};
use crate::logs::{count, Kind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct Env {
    pub opts: Options,
    /// Screenshots, logs and `report.json`.
    pub out: PathBuf,
}

type Check = fn(&Env, &mut Outcome) -> Result<String, String>;

pub struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    check: Check,
}

pub const ALL: &[Scenario] = &[
    Scenario { name: "menu", about: "the main menu draws and lists its items", check: menu },
    Scenario { name: "boot-clean", about: "boot and menu log no Lua errors or failed requires", check: boot_clean },
    Scenario { name: "menu-solo", about: "a click on SOLO opens the next screen", check: menu_solo },
    Scenario { name: "resize", about: "resizing the window logs no Lua errors", check: resize },
    Scenario { name: "new-game", about: "a new game reaches the world with a living player", check: new_game },
    Scenario { name: "survival-guide", about: "the Survival Guide closes with its CLOSE button", check: survival_guide },
    Scenario { name: "walk", about: "holding W moves the player", check: walk },
    Scenario { name: "ingame-stable", about: "600 frames in the world log no Lua errors", check: ingame_stable },
    Scenario { name: "save-load", about: "quit saves the game and --load-game enters it again", check: save_load },
];

/// What a scenario leaves behind besides pass/fail.
#[derive(Default)]
pub struct Outcome {
    pub screenshots: Vec<String>,
    pub log: Option<Value>,
    pub frames: u64,
}

/// Known engine bugs (`docs/AGENT-CONTROL.md`), matched in failure reasons.
const KNOWN: &[(&str, &str)] = &[
    ("bounded scratch component writer", "PlayerDB save: bounded scratch component writer is unsupported"),
    ("IsoPlayer.getInstance() is null", "--load-game: IsoPlayer.getInstance() is null"),
    ("isAnyVisible of non-table", "getPlayerContextMenu() returns nil: ISPlayerDataObject:createInventoryInterface aborts when an unexplored container is near the spawn, because ItemPicker.fillContainer (ItemPickerJava, the loot distributions) is not ported"),
    ("require_failed", "a Lua module that loads in the original fails to require (the 9 vanilla failures count as warnings)"),
    ("no_such_function", "ItemCodeOnCreate handlers not ported (GasMask, SubjectBook, ...; RandomColor, IDCard and RecipeMagazine are)"),
];

pub fn known_bug(reason: &str) -> Option<&'static str> {
    KNOWN.iter().find(|(k, _)| reason.contains(k)).map(|(_, v)| *v)
}

pub fn run(env: &Env, only: &[String]) -> Value {
    let _ = std::fs::create_dir_all(&env.out);
    let mut results = Vec::new();
    for s in ALL.iter().filter(|s| only.is_empty() || only.iter().any(|o| o == s.name)) {
        eprintln!("--- {} ({})", s.name, s.about);
        let t = Instant::now();
        let mut o = Outcome::default();
        let r = (s.check)(env, &mut o);
        let secs = t.elapsed().as_secs_f64();
        let (ok, text) = match &r {
            Ok(d) => (true, d.clone()),
            Err(e) => (false, e.clone()),
        };
        eprintln!("    {} in {secs:.1} s: {}", if ok { "PASS" } else { "FAIL" }, first_line(&text));
        results.push(json!({
            "name": s.name,
            "about": s.about,
            "ok": ok,
            "details": if ok { json!(text) } else { Value::Null },
            "reason": if ok { Value::Null } else { json!(text) },
            "known_bug": if ok { None } else { known_bug(&text) },
            "seconds": (secs * 10.0).round() / 10.0,
            "frames": o.frames,
            "screenshots": o.screenshots,
            "log": o.log,
        }));
    }
    let passed = results.iter().filter(|r| r["ok"] == true).count();
    let report = json!({"passed": passed, "failed": results.len() - passed, "scenarios": results});
    let path = env.out.join("report.json");
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default());
    report
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// The summary table `pz-ctl scenarios` prints.
pub fn summary(report: &Value) -> String {
    let mut s = String::new();
    for r in report["scenarios"].as_array().into_iter().flatten() {
        let status = if r["ok"] == true { "PASS" } else { "FAIL" };
        let text = if r["ok"] == true { &r["details"] } else { &r["reason"] };
        s.push_str(&format!(
            "{status}  {:<15} {:>6.1}s  {}\n",
            r["name"].as_str().unwrap_or(""),
            r["seconds"].as_f64().unwrap_or(0.0),
            first_line(text.as_str().unwrap_or(""))
        ));
        if let Some(k) = r["known_bug"].as_str() {
            s.push_str(&format!("      known bug: {k}\n"));
        }
    }
    s.push_str(&format!("{} passed, {} failed\n", report["passed"], report["failed"]));
    s
}

/// One game process for one scenario.
struct Session<'a> {
    c: Client,
    env: &'a Env,
    name: &'a str,
    frame: u64,
}

const MAIN_MENU: &str = "zombie.gameStates.MainScreenState";
const INGAME: &str = "zombie.gameStates.IngameState";

impl<'a> Session<'a> {
    fn start(env: &'a Env, name: &'a str, args: &[String]) -> Result<Self, String> {
        let mut opts = env.opts.clone();
        opts.log = env.out.join(format!("{name}.log"));
        let (c, _) = Client::start(&opts, args)?;
        Ok(Session { c, env, name, frame: 0 })
    }

    /// A new game in a fresh save `pzctl-<scenario>`.
    fn new_game(env: &'a Env, name: &'a str) -> Result<Self, String> {
        let world = format!("pzctl-{name}");
        let _ = std::fs::remove_dir_all(env.opts.cache_dir.join("Saves").join("Sandbox").join(&world));
        Self::start(env, name, &["--new-game".into(), world])
    }

    /// Send a command; a failed command or an engine error is an `Err`.
    fn cmd(&mut self, v: Value) -> Result<Value, String> {
        let r = self.c.send(&v)?;
        if let Some(f) = r["frame"].as_u64() {
            self.frame = f;
        }
        if r["ok"] != true {
            return Err(format!("{} failed: {}", v["cmd"], r["error"]));
        }
        if let Some(e) = r["engine_error"].as_str() {
            return Err(format!("engine error at frame {}: {e}", self.frame));
        }
        Ok(r)
    }

    fn frames(&mut self, n: u32) -> Result<(), String> {
        self.cmd(json!({"cmd": "frames", "n": n})).map(|_| ())
    }

    /// Run frames in steps of 50 until the state machine is in `state`.
    fn until_state(&mut self, state: &str, max: u64) -> Result<u64, String> {
        let start = self.frame;
        loop {
            let r = self.cmd(json!({"cmd": "frames", "n": 50}))?;
            if r["state"] == state {
                return Ok(self.frame);
            }
            if self.frame - start >= max {
                return Err(format!("not in {state} after {max} frames (state {})", r["state"]));
            }
        }
    }

    fn snapshot(&mut self) -> Result<Value, String> {
        Ok(self.cmd(json!({"cmd": "snapshot"}))?["snapshot"].take())
    }

    fn shot(&mut self, o: &mut Outcome, label: &str) -> Result<PathBuf, String> {
        let p = absolute(&self.env.out.join(format!("{}-{label}.png", self.name)));
        self.cmd(json!({"cmd": "screenshot", "path": p}))?;
        o.screenshots.push(p.display().to_string());
        Ok(p)
    }

    fn click(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.cmd(json!({"cmd": "click", "x": x, "y": y})).map(|_| ())
    }

    /// Record frames and the log since `mark`, then quit.
    fn finish(self, o: &mut Outcome, mark: usize) -> Result<Value, String> {
        o.frames = self.frame;
        o.log = Some(self.c.log_since(mark));
        self.c.quit()
    }

    fn note(&self, o: &mut Outcome, mark: usize) {
        o.frames = self.frame;
        o.log = Some(self.c.log_since(mark));
    }
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Depth-first search of the snapshot's UI tree.
pub fn find_ui<'v>(nodes: &'v Value, pred: &dyn Fn(&Value) -> bool) -> Option<&'v Value> {
    for n in nodes.as_array()? {
        if pred(n) {
            return Some(n);
        }
        if let Some(found) = n.get("children").and_then(|c| find_ui(c, pred)) {
            return Some(found);
        }
    }
    None
}

/// Lua types of every node in the UI tree, without repeats.
pub fn ui_types(nodes: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in nodes.as_array().into_iter().flatten() {
        if let Some(t) = n["type"].as_str() {
            if !out.iter().any(|o| o == t) {
                out.push(t.to_string());
            }
        }
        if let Some(c) = n.get("children") {
            for t in ui_types(c) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    }
    out
}

/// Texts of every node in the UI tree.
pub fn ui_texts(nodes: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for n in nodes.as_array().into_iter().flatten() {
        if let Some(t) = n["text"].as_str() {
            out.push(t.to_string());
        }
        if let Some(c) = n.get("children") {
            out.extend(ui_texts(c));
        }
    }
    out
}

pub fn center(node: &Value) -> Option<(f64, f64)> {
    let r = node["rect"].as_array()?;
    let f = |i: usize| r.get(i).and_then(Value::as_f64);
    Some((f(0)? + f(2)? / 2.0, f(1)? + f(3)? / 2.0))
}

/// How many distinct colours a screenshot has (sampled); a black or
/// one-colour frame has very few.
pub fn png_colours(path: &Path) -> Result<usize, String> {
    let d = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?));
    let mut r = d.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let px = &buf[..info.buffer_size()];
    let step = (info.color_type.samples()).max(1);
    let mut seen = std::collections::HashSet::new();
    for c in px.chunks(step).step_by(97) {
        seen.insert((c[0] >> 3, c.get(1).copied().unwrap_or(0) >> 3, c.get(2).copied().unwrap_or(0) >> 3));
    }
    Ok(seen.len())
}

fn no_new_errors(log: &Value, what: &str) -> Result<(), String> {
    let lua = count(log, Kind::LuaError);
    let err = count(log, Kind::Error);
    if lua + err > 0 {
        let sample = log["samples"]["lua_error"][0].as_str().or(log["samples"]["error"][0].as_str()).unwrap_or("");
        return Err(format!("{what}: {lua} Lua errors, {err} ERROR lines; first: {sample}"));
    }
    Ok(())
}

const MENU_ITEMS: &[&str] = &["SOLO", "LOAD", "OPTIONS", "QUIT"];

fn menu(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = Session::start(env, "menu", &[])?;
    s.frames(60)?;
    let snap = s.snapshot()?;
    if snap["state"] != MAIN_MENU {
        return Err(format!("state is {} after 60 frames", snap["state"]));
    }
    let shot = s.shot(o, "frame60")?;
    let texts = ui_texts(&snap["ui"]);
    let missing: Vec<_> = MENU_ITEMS.iter().filter(|m| !texts.iter().any(|t| t == *m)).collect();
    s.finish(o, 0)?;
    if !missing.is_empty() {
        return Err(format!("menu items missing: {missing:?} (UI texts: {texts:?})"));
    }
    let colours = png_colours(&shot)?;
    if colours < 50 {
        return Err(format!("screenshot looks blank ({colours} colours)"));
    }
    Ok(format!("{} UI texts incl. {MENU_ITEMS:?}; screenshot has {colours} colours", texts.len()))
}

fn boot_clean(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = Session::start(env, "boot-clean", &[])?;
    s.frames(60)?;
    s.snapshot()?;
    s.note(o, 0);
    let log = o.log.clone().unwrap_or_default();
    s.c.quit()?;
    let bad: Vec<String> = [Kind::RequireFailed, Kind::LuaError, Kind::NoSuchFunction, Kind::Error, Kind::Panic]
        .iter()
        .filter(|k| count(&log, **k) > 0)
        .map(|k| format!("{} {}", count(&log, *k), k.name()))
        .collect();
    if bad.is_empty() {
        Ok("no errors at boot".into())
    } else {
        let first = log["samples"]["require_failed"][0].as_str().unwrap_or("");
        Err(format!("boot logs {}; e.g. {first}", bad.join(", ")))
    }
}

fn menu_solo(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = Session::start(env, "menu-solo", &[])?;
    s.frames(60)?;
    // MainScreen ignores clicks while its intro `delay` runs.
    let mut waited = 0;
    loop {
        let r = s.cmd(json!({"cmd": "lua", "code": "return MainScreen.instance.delay"}))?;
        let delay = r["result"][0].as_str().and_then(|d| d.parse::<f64>().ok()).ok_or("MainScreen.instance.delay is not a number")?;
        if delay <= 0.0 {
            break;
        }
        if waited >= 1200 {
            return Err(format!("MainScreen.delay still {delay} after {waited} frames"));
        }
        s.frames(50)?;
        waited += 50;
    }
    let before = s.snapshot()?;
    let solo = find_ui(&before["ui"], &|n| n["text"] == "SOLO").ok_or("no SOLO item in the menu")?;
    let (x, y) = center(solo).ok_or("SOLO has no rect")?;
    let mark = s.c.mark();
    s.click(x, y)?;
    s.frames(20)?;
    let after = s.snapshot()?;
    s.shot(o, "after-click")?;
    s.finish(o, mark)?;
    let (tb, ta) = (ui_types(&before["ui"]), ui_types(&after["ui"]));
    let new: Vec<_> = ta.iter().filter(|t| !tb.contains(t)).collect();
    no_new_errors(&after["log"], "after the click")?;
    if new.is_empty() {
        return Err(format!("no new UI after clicking SOLO at ({x:.0}, {y:.0}) at frame {}", o.frames));
    }
    Ok(format!("clicked SOLO after the menu delay ({waited} frames); new UI: {new:?}"))
}

fn resize(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = Session::start(env, "resize", &[])?;
    s.frames(60)?;
    s.snapshot()?;
    let mark = s.c.mark();
    for (w, h) in [(1600, 900), (1024, 768), (1280, 720)] {
        s.cmd(json!({"cmd": "resize", "w": w, "h": h, "frames": 10}))?;
        let snap = s.snapshot()?;
        if snap["screen"] != json!({"w": w, "h": h}) {
            return Err(format!("screen is {} after resizing to {w}x{h}", snap["screen"]));
        }
        if w == 1024 {
            s.shot(o, "1024x768")?;
        }
    }
    s.finish(o, mark)?;
    no_new_errors(o.log.as_ref().unwrap_or(&Value::Null), "after resizing")?;
    Ok("1600x900, 1024x768, 1280x720 without Lua errors".into())
}

/// A new game up to the first frames of `IngameState`.
fn into_world<'a>(env: &'a Env, name: &'a str) -> Result<Session<'a>, String> {
    let mut s = Session::new_game(env, name)?;
    s.until_state(INGAME, 3000)?;
    s.frames(20)?;
    Ok(s)
}

fn new_game(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_world(env, "new-game")?;
    let reached = s.frame;
    let snap = s.snapshot()?;
    s.shot(o, "world")?;
    s.note(o, 0);
    let p = &snap["player"];
    if p.is_null() {
        return Err(format!("in {INGAME} at frame {reached} but there is no player"));
    }
    let health = p["health"].as_f64().unwrap_or(0.0);
    let items = p["inventory"].as_array().map_or(0, Vec::len);
    let errors = p["errors"].as_array().map_or(0, Vec::len);
    if health <= 0.0 || p["dead"] == true {
        return Err(format!("player is dead or has no health: {p}"));
    }
    s.c.quit().ok();
    Ok(format!(
        "{INGAME} at frame {reached}; player at ({}, {}), health {health}, {items} items, {} zombies near{}",
        p["x"],
        p["y"],
        snap["zombies"]["nearby"].as_array().map_or(0, Vec::len),
        if errors > 0 { format!(", {errors} Lua read errors") } else { String::new() }
    ))
}

/// Click the Survival Guide's CLOSE button if the guide is open.
/// Returns whether it was open.
fn close_guide(s: &mut Session, o: &mut Outcome) -> Result<bool, String> {
    let snap = s.snapshot()?;
    let Some(guide) = find_ui(&snap["ui"], &|n| n["type"] == "SurvivalGuide") else {
        return Ok(false);
    };
    let close = find_ui(guide.get("children").unwrap_or(&Value::Null), &|n| n["text"] == "CLOSE")
        .ok_or("the Survival Guide has no CLOSE button")?;
    let (x, y) = center(close).ok_or("CLOSE has no rect")?;
    s.shot(o, "guide-open")?;
    s.click(x, y)?;
    s.frames(10)?;
    let after = s.snapshot()?;
    if find_ui(&after["ui"], &|n| n["type"] == "SurvivalGuide").is_some() {
        s.shot(o, "guide-still-open")?;
        return Err(format!("the Survival Guide is still open after clicking CLOSE at ({x:.0}, {y:.0})"));
    }
    Ok(true)
}

fn survival_guide(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_world(env, "survival-guide")?;
    let mark = s.c.mark();
    if !close_guide(&mut s, o)? {
        return Err("the Survival Guide did not open in a new game".into());
    }
    s.shot(o, "guide-closed")?;
    s.finish(o, mark).ok();
    no_new_errors(o.log.as_ref().unwrap_or(&Value::Null), "the guide closed, but clicking CLOSE logged")?;
    Ok("closed with CLOSE".into())
}

fn position(snap: &Value) -> Option<(f64, f64)> {
    Some((snap["player"]["x"].as_f64()?, snap["player"]["y"].as_f64()?))
}

fn walk(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_world(env, "walk")?;
    // The guide may hold the keyboard; a failure to close it is reported
    // by its own scenario.
    let _ = close_guide(&mut s, o);
    let mark = s.c.mark();
    let a = position(&s.snapshot()?).ok_or("no player")?;
    s.cmd(json!({"cmd": "key", "key": "W", "action": "hold", "frames": 60}))?;
    let snap = s.snapshot()?;
    let b = position(&snap).ok_or("no player")?;
    s.shot(o, "after-w")?;
    s.finish(o, mark).ok();
    let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    if d < 0.5 {
        return Err(format!(
            "player did not move: ({:.2}, {:.2}) -> ({:.2}, {:.2}) after holding W for 60 frames; action state {}",
            a.0, a.1, b.0, b.1, snap["player"]["action_state"]
        ));
    }
    Ok(format!("moved {d:.2} tiles: ({:.2}, {:.2}) -> ({:.2}, {:.2})", a.0, a.1, b.0, b.1))
}

fn ingame_stable(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_world(env, "ingame-stable")?;
    let mark = s.c.mark();
    for _ in 0..6 {
        s.frames(100)?;
    }
    s.shot(o, "frame-plus-600")?;
    s.note(o, mark);
    s.c.quit().ok();
    no_new_errors(o.log.as_ref().unwrap_or(&Value::Null), "600 frames in the world")?;
    Ok("600 frames without Lua errors".into())
}

fn save_load(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let name = "save-load";
    let mut s = into_world(env, name)?;
    s.frames(100)?;
    let before = position(&s.snapshot()?).ok_or("no player before saving")?;
    let quit = s.finish(o, 0)?;
    // Try the load even after a failed save: it shows the next bug too.
    let save_error = (quit["ok"] != true).then(|| quit["error"].as_str().unwrap_or("?").to_string());
    let load = load_into_world(env, name, o);
    match (save_error, load) {
        (None, Ok(after)) => Ok(format!(
            "saved at ({:.1}, {:.1}), loaded at ({:.1}, {:.1})",
            before.0, before.1, after.0, after.1
        )),
        (Some(e), Ok(_)) => Err(format!("saving on quit failed: {e} (the partial save still loaded)")),
        (None, Err(e)) => Err(format!("saved, but loading failed: {e}")),
        (Some(e), Err(l)) => Err(format!("saving on quit failed: {e}; loading then failed: {l}")),
    }
}

fn load_into_world(env: &Env, name: &str, o: &mut Outcome) -> Result<(f64, f64), String> {
    let mut l = Session::start(env, "save-load-2", &["--load-game".into(), format!("Sandbox/pzctl-{name}")])?;
    let r = l.until_state(INGAME, 3000);
    if let Some(log) = &mut o.log {
        log["after_load"] = l.c.log_since(0);
    }
    r?;
    l.frames(20)?;
    let after = position(&l.snapshot()?).ok_or("no player after loading")?;
    l.shot(o, "loaded")?;
    l.c.quit().ok();
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> Value {
        json!([
            {"type": "MainScreen", "rect": [0, 0, 100, 100], "children": [
                {"type": "ISPanel", "rect": [0, 0, 50, 50], "children": [
                    {"type": "ISLabel", "text": "SOLO", "rect": [10, 20, 30, 40]}
                ]}
            ]},
            {"type": "SurvivalGuide", "text": "Survival Guide", "rect": [0, 0, 1, 1]}
        ])
    }

    #[test]
    fn find_ui_searches_depth_first() {
        let u = ui();
        let solo = find_ui(&u, &|n| n["text"] == "SOLO").unwrap();
        assert_eq!(center(solo), Some((25.0, 40.0)));
        assert!(find_ui(&u, &|n| n["type"] == "SurvivalGuide").is_some());
        assert!(find_ui(&u, &|n| n["text"] == "QUIT").is_none());
    }

    #[test]
    fn ui_texts_lists_all_levels() {
        assert_eq!(ui_texts(&ui()), vec!["SOLO".to_string(), "Survival Guide".to_string()]);
    }

    #[test]
    fn ui_types_lists_each_type_once() {
        assert_eq!(ui_types(&ui()), vec!["MainScreen", "ISPanel", "ISLabel", "SurvivalGuide"]);
    }

    #[test]
    fn known_bugs_are_recognised() {
        assert!(known_bug("quit failed: PlayerDB save host failure: Unsupported { operation: \"bounded scratch component writer\" }").is_some());
        assert!(known_bug("something new").is_none());
    }

    #[test]
    fn summary_lists_each_scenario() {
        let r = json!({"passed": 1, "failed": 1, "scenarios": [
            {"name": "menu", "ok": true, "details": "fine", "seconds": 1.5},
            {"name": "walk", "ok": false, "reason": "stuck\nmore", "seconds": 2.0, "known_bug": null}
        ]});
        let s = summary(&r);
        assert!(s.contains("PASS  menu"));
        assert!(s.contains("FAIL  walk") && s.contains("stuck") && !s.contains("more"));
        assert!(s.ends_with("1 passed, 1 failed\n"));
    }

    #[test]
    fn png_colours_tells_blank_from_busy() {
        let dir = std::env::temp_dir().join(format!("pzctl-scen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, px: &[u8]| {
            let p = dir.join(name);
            let mut e = png::Encoder::new(std::fs::File::create(&p).unwrap(), 64, 64);
            e.set_color(png::ColorType::Rgba);
            e.write_header().unwrap().write_image_data(px).unwrap();
            p
        };
        let black = write("black.png", &[0; 64 * 64 * 4]);
        let busy: Vec<u8> = (0..64 * 64 * 4).map(|i| (i * 37 % 251) as u8).collect();
        let busy = write("busy.png", &busy);
        assert_eq!(png_colours(&black).unwrap(), 1);
        assert!(png_colours(&busy).unwrap() > 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
