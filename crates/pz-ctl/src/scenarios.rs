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
    Scenario { name: "eat", about: "Eat from an apple's inventory menu lowers hunger", check: eat },
    Scenario { name: "equip", about: "Equip in Both Hands from a bat's inventory menu puts it in hand", check: equip },
    Scenario { name: "melee", about: "aim and click kill a zombie in front with a bat", check: melee },
    Scenario { name: "door", about: "E (or the context menu) opens a door", check: door },
    Scenario { name: "stances", about: "C sneaks, Shift+W runs, Meta+W sprints", check: stances },
    Scenario { name: "sleep", about: "Sleep from the context menu puts a tired player to sleep", check: sleep },
    Scenario { name: "panels", about: "H, L and B open the health, skills and crafting windows", check: panels },
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
    ("in createMenu", "ISInventoryPaneContextMenu.createMenu calls about 20 unbound InventoryItem/IsoPlayer methods (getClothingItemExtraOption, ...), so item menus stop half-built"),
    ("no world context menu", "the world context menu does not open: UIManager does not fire OnObjectRightMouseButtonUp (no IsoObjectPicker)"),
    ("no attack started", "aim + left click starts no attack, and zombies do not move or react"),
    ("context_actions_nswe", "E does nothing at doors and windows: the world's doContext hook (context_actions_nswe) is not ported"),
    ("at FindCraftSurface:", "IsoObject.getSquare() is nil for map objects (only world items get the square back-reference), so FindCraftSurface fails"),
    ("at getVehicleToInteractWith:", "IsoPlayer.getUseableVehicle/getNearVehicle are unbound: ISVehicleMenu.onKeyPressed fails on every key press"),
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

    fn right_click(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.cmd(json!({"cmd": "click", "x": x, "y": y, "button": "right"})).map(|_| ())
    }

    /// `action`: `tap`, `hold`, `down` or `up`.
    fn key(&mut self, key: &str, action: &str, frames: u32) -> Result<(), String> {
        self.cmd(json!({"cmd": "key", "key": key, "action": action, "frames": frames})).map(|_| ())
    }

    /// Run a Lua chunk. Returns its first value parsed as JSON when it is
    /// JSON, else that value as a string.
    fn lua(&mut self, code: &str) -> Result<Value, String> {
        let mut r = self.cmd(json!({"cmd": "lua", "code": code}))?;
        if !r["json"].is_null() {
            return Ok(r["json"].take());
        }
        Ok(r["result"][0].take())
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

/// Lua that finds where to click; the action itself is mouse or keyboard
/// input. Positions are window pixels, like `click`.
const ACTION_LUA: &str = r#"
-- Options of the player's visible context menus (the root and its
-- submenus): name, centre and whether it can be chosen.
function pzctl_menu()
    local root = getPlayerContextMenu(0)
    if not root then return pzctl_json({ error = 'getPlayerContextMenu(0) is nil' }) end
    local menus, out = { root }, {}
    for _, m in ipairs(root.instanceMap or {}) do menus[#menus + 1] = m end
    for _, m in ipairs(menus) do
        if m:isVisible() and m.options then
            local dy = (m:getScrollHeight() > m:getScrollAreaHeight()) and m.scrollIndicatorHgt or 0
            for i = 1, m.numOptions - 1 do
                local o = m.options[i]
                if o then
                    out[#out + 1] = {
                        name = o.name,
                        x = m:getAbsoluteX() + math.min(40, m:getWidth() / 2),
                        y = m:getAbsoluteY() + m.padTopBottom + dy + (i - 0.5) * m.itemHgt + m:getYScroll(),
                        off = (o.notAvailable or o.isDisabled) and true or false,
                        sub = o.subOption ~= nil,
                    }
                end
            end
        end
    end
    return pzctl_json(out)
end

-- The row of the first item of `fullType` in the player's inventory pane.
function pzctl_inventory_row(fullType)
    local page = getPlayerInventory(0)
    if not page then return pzctl_json({ error = 'getPlayerInventory(0) is nil' }) end
    local pane = page.inventoryPane
    pane:refreshContainer()
    -- `pane.items` is rebuilt while the pane renders; `itemslist` holds
    -- the same rows (one per collapsed group) at once.
    local row = 0
    for _, v in ipairs(pane.itemslist) do
        row = row + 1
        local i = row
        local it = v.items and v.items[1]
        if not pane.collapsed[v.name] and v.items and #v.items > 1 then row = row + #v.items - 1 end
        if it and it:getFullType() == fullType then
            return pzctl_json({
                x = pane:getAbsoluteX() + math.min(pane.column2 or 60, pane.column3 or 60) / 2 + 20,
                y = pane:getAbsoluteY() + pane.headerHgt + (i - 0.5) * pane.itemHgt + pane:getYScroll(),
                visible = page:isVisible(), collapsed = page.isCollapsed and true or false,
                title = page:getAbsoluteY() + 8,
            })
        end
    end
    return pzctl_json({ error = fullType .. ' is not in the inventory pane (' .. row .. ' rows)' })
end

-- Why no item menu opened: `ISInventoryPaneContextMenu.createMenu` on the
-- item, under pcall (errors in UI mouse handlers are not logged).
function pzctl_item_menu_error(fullType)
    local it = getPlayer():getInventory():getFirstTypeRecurse(fullType)
    local ok, e = pcall(function() return ISInventoryPaneContextMenu.createMenu(0, true, { it }, 0, 0) end)
    if ok then return 'createMenu succeeded' end
    return tostring(e)
end

-- What the action scenarios check, each field read on its own.
function pzctl_state()
    local p = getPlayer()
    local r = { errors = {} }
    local function try(name, f)
        local ok, v = pcall(f)
        if ok then r[name] = v else r.errors[#r.errors + 1] = name .. ': ' .. tostring(v) end
    end
    try('hunger', function() return p:getStats():get(CharacterStat.HUNGER) end)
    try('fatigue', function() return p:getStats():get(CharacterStat.FATIGUE) end)
    try('primary', function() local i = p:getPrimaryHandItem() return i and i:getFullType() or false end)
    try('sneaking', function() return p:isSneaking() end)
    try('running', function() return p:isRunning() end)
    try('sprinting', function() return p:isSprinting() end)
    try('asleep', function() return p:isAsleep() end)
    try('speed', function() return getGameSpeed() end)
    try('hours', function() return getGameTime():getWorldAgeHours() end)
    try('actions', function() return #ISTimedActionQueue.getTimedActionQueue(p).queue end)
    return pzctl_json(r)
end
"#;

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

/// Where most action scenarios start: outdoors on a street, with no
/// container in reach (an unexplored container near the spawn breaks the
/// inventory UI; see `docs/AGENT-CONTROL.md`).
const ACTION_SPAWN: &str = "10620,9940,0";
/// An open parking lot; zombies are placed at random, but some are near.
const MELEE_SPAWN: &str = "10624,9930,0";
/// The square south of the north-facing door at 10621,9911.
const DOOR_SPAWN: &str = "10621,9911,0";

/// A new game at `spawn`, the guide closed and the action Lua loaded.
fn into_action_world<'a>(env: &'a Env, name: &'a str, spawn: &str, o: &mut Outcome) -> Result<Session<'a>, String> {
    let world = format!("pzctl-{name}");
    let _ = std::fs::remove_dir_all(env.opts.cache_dir.join("Saves").join("Sandbox").join(&world));
    let mut s = Session::start(env, name, &["--new-game".into(), world, "--spawn".into(), spawn.into()])?;
    s.until_state(INGAME, 3000)?;
    s.frames(20)?;
    let _ = close_guide(&mut s, o);
    s.cmd(json!({"cmd": "lua", "code": ACTION_LUA}))?;
    // The character window's visibility and tab come from `Lua/layout.ini`
    // (earlier games). Its Info tab fails to render (no
    // `IsoPlayer.getHumanVisual`), and the frame then stays black.
    s.prepare("closing the character window", "getPlayerInfoPanel(0):setVisible(false) return 'ok'")?;
    Ok(s)
}

impl Session<'_> {
    fn state(&mut self) -> Result<Value, String> {
        self.lua("return pzctl_state()")
    }

    /// The visible context-menu options; empty when no menu is open.
    fn menu(&mut self) -> Result<Vec<Value>, String> {
        let opts = self.lua("return pzctl_menu()")?;
        if let Some(e) = opts["error"].as_str() {
            return Err(e.to_string());
        }
        Ok(opts.as_array().cloned().unwrap_or_default())
    }

    /// Click the visible context-menu option `name` (a submenu opens).
    fn choose(&mut self, name: &str) -> Result<(), String> {
        let opts = self.menu().map_err(|e| format!("choosing \"{name}\": {e}"))?;
        let names: Vec<&str> = opts.iter().filter_map(|o| o["name"].as_str()).collect();
        let o = opts
            .iter()
            .find(|o| o["name"] == name)
            .ok_or_else(|| format!("no \"{name}\" in the context menu; it has {names:?}"))?;
        if o["off"] == true {
            return Err(format!("\"{name}\" is disabled in the context menu"));
        }
        let (x, y) = (f(&o["x"]), f(&o["y"]));
        self.click(x, y)?;
        self.frames(2)
    }

    /// Right-click the inventory row of the first `full_type` item.
    fn item_menu(&mut self, full_type: &str) -> Result<(), String> {
        let code = format!("return pzctl_inventory_row({full_type:?})");
        let row = self.lua(&code)?;
        if let Some(e) = row["error"].as_str() {
            return Err(e.to_string());
        }
        let (x, y) = (f(&row["x"]), f(&row["y"]));
        // The inventory page unfolds while the mouse is over its title.
        self.cmd(json!({"cmd": "mouse", "x": x, "y": f(&row["title"]), "frames": 10}))?;
        self.cmd(json!({"cmd": "mouse", "x": x, "y": y, "frames": 5}))?;
        let row = self.lua(&code)?;
        let (x, y) = (row["x"].as_f64().unwrap_or(x), row["y"].as_f64().unwrap_or(y));
        self.right_click(x, y)?;
        self.frames(2)?;
        if self.menu()?.is_empty() {
            let e = self.lua(&format!("return pzctl_item_menu_error({full_type:?})"))?;
            return Err(format!(
                "no context menu after a right click on the {full_type} row at ({x:.0}, {y:.0}); \
                 ISInventoryPaneContextMenu.createMenu: {} (the UI swallows errors in mouse handlers, \
                 so the log has none)",
                e.as_str().unwrap_or("?")
            ));
        }
        Ok(())
    }

    /// Right-click a window point; an error when no context menu opens.
    fn world_menu(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.right_click(x, y)?;
        self.frames(5)?;
        if self.menu()?.is_empty() {
            return Err(format!(
                "no world context menu after a right click at ({x:.0}, {y:.0}): UIManager does not fire \
                 OnObjectRightMouseButtonUp (no IsoObjectPicker)"
            ));
        }
        Ok(())
    }

    /// Lua that must succeed; its value as JSON or a string.
    fn prepare(&mut self, what: &str, code: &str) -> Result<Value, String> {
        let r = self.lua(code)?;
        if let Some(e) = r["error"].as_str() {
            return Err(format!("{what}: {e}"));
        }
        Ok(r)
    }

    /// Walk with the movement keys straight at a world point until within
    /// `near` tiles. W moves -x -y, D +x -y, S +x +y, A -x +y.
    fn walk_to(&mut self, tx: f64, ty: f64, near: f64) -> Result<(f64, f64), String> {
        let mut last: Option<(f64, f64)> = None;
        let mut stuck = 0;
        for _ in 0..150 {
            let (x, y) = position(&self.snapshot()?).ok_or("no player")?;
            let (dx, dy) = (tx - x, ty - y);
            if (dx * dx + dy * dy).sqrt() <= near {
                return Ok((x, y));
            }
            let sign = |d: f64| if d.abs() < 0.25 { 0 } else if d > 0.0 { 1 } else { -1 };
            let keys: &[&str] = match (sign(dx), sign(dy)) {
                (1, -1) => &["D"],
                (1, 1) => &["S"],
                (-1, 1) => &["A"],
                (-1, -1) => &["W"],
                (1, 0) => &["S", "D"],
                (-1, 0) => &["W", "A"],
                (0, 1) => &["S", "A"],
                (0, -1) => &["W", "D"],
                _ => return Ok((x, y)),
            };
            for k in keys {
                self.key(k, "down", 1)?;
            }
            self.frames(10)?;
            for k in keys {
                self.key(k, "up", 1)?;
            }
            match last {
                Some((lx, ly)) if ((x - lx).powi(2) + (y - ly).powi(2)).sqrt() < 0.05 => {
                    stuck += 1;
                    if stuck >= 4 {
                        return Err(format!("stuck at ({x:.2}, {y:.2})"));
                    }
                }
                _ => stuck = 0,
            }
            last = Some((x, y));
        }
        Err("still walking after 150 steps".into())
    }
}

/// The check's result, failed by the Lua errors logged since `mark` (all
/// distinct samples, so one error does not hide the next).
fn verdict(s: &Session, o: &mut Outcome, mark: usize, r: Result<String, String>) -> Result<String, String> {
    s.note(o, mark);
    let log = o.log.as_ref().unwrap_or(&Value::Null);
    let n = count(log, Kind::LuaError) + count(log, Kind::Error);
    let errors = (n > 0).then(|| {
        let samples: Vec<&str> = ["lua_error", "error"]
            .iter()
            .flat_map(|k| log["samples"][k].as_array().into_iter().flatten().filter_map(Value::as_str))
            .collect();
        format!("{n} Lua errors: {}", samples.join(" | "))
    });
    match (r, errors) {
        (Ok(t), None) => Ok(t),
        (Ok(t), Some(e)) => Err(format!("{t}, but {e}")),
        (Err(f), None) => Err(f),
        (Err(f), Some(e)) => Err(format!("{f}; {e}")),
    }
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn eat(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "eat", ACTION_SPAWN, o)?;
    s.prepare(
        "giving an apple",
        "local p = getPlayer() p:getInventory():AddItem('Base.Apple') \
         p:getStats():set(CharacterStat.HUNGER, 0.5) return 'ok'",
    )?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let before = s.state()?;
        s.item_menu("Base.Apple")?;
        s.shot(o, "menu")?;
        s.choose("Eat")?;
        // Food with more than a quarter of hunger opens All / 1/2 / 1/4.
        if s.menu()?.iter().any(|o| o["name"] == "All") {
            s.choose("All")?;
        }
        s.frames(400)?;
        let after = s.state()?;
        let left = s.lua("return getPlayer():getInventory():getCountTypeRecurse('Base.Apple')")?;
        s.shot(o, "after")?;
        let (h0, h1) = (f(&before["hunger"]), f(&after["hunger"]));
        if !(h1 < h0) {
            return Err(format!("hunger {h0} -> {h1} after choosing Eat; apples left {left}; state {after}"));
        }
        Ok(format!("hunger {h0:.3} -> {h1:.3}; apples left {left}"))
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

fn equip(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "equip", ACTION_SPAWN, o)?;
    s.prepare("giving a bat", "getPlayer():getInventory():AddItem('Base.BaseballBat') return 'ok'")?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        s.item_menu("Base.BaseballBat")?;
        s.shot(o, "menu")?;
        s.choose("Equip in Both Hands")?;
        s.frames(150)?;
        let st = s.state()?;
        s.shot(o, "after")?;
        if st["primary"] != "Base.BaseballBat" {
            return Err(format!("primary hand holds {} after Equip in Both Hands; state {st}", st["primary"]));
        }
        Ok("Equip in Both Hands put the bat in the primary hand".into())
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

fn melee(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "melee", MELEE_SPAWN, o)?;
    s.prepare(
        "equipping a bat",
        "local p = getPlayer() local bat = p:getInventory():AddItem('Base.BaseballBat') \
         p:setPrimaryHandItem(bat) p:setSecondaryHandItem(bat) return 'ok'",
    )?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let snap = s.snapshot()?;
        let z = snap["zombies"]["nearby"][0].clone();
        if z.is_null() {
            return Err("no zombie within 30 tiles".into());
        }
        let (zx, zy) = (f(&z["x"]), f(&z["y"]));
        let (px, py) = s
            .walk_to(zx, zy, 1.1)
            .map_err(|e| format!("walking to the nearest zombie at ({zx}, {zy}): {e}"))?;
        let at = s.prepare(
            "facing the zombie",
            &format!(
                "getPlayer():faceLocation({zx}, {zy}) \
                 return pzctl_json({{ x = isoToScreenX(0, {zx}, {zy}, 0), y = isoToScreenY(0, {zx}, {zy}, 0) }})"
            ),
        )?;
        // Click the zombie's body, above its feet.
        let (cx, cy) = (f(&at["x"]), f(&at["y"]) - 40.0);
        s.shot(o, "zombie")?;
        let mut states = Vec::new();
        for swing in 1..=6 {
            s.key("LCONTROL", "down", 1)?;
            s.frames(5)?;
            s.cmd(json!({"cmd": "click", "x": cx, "y": cy, "frames": 2}))?;
            s.frames(10)?;
            let snap = s.snapshot()?;
            states.push(snap["player"]["action_state"].as_str().unwrap_or("?").to_string());
            s.frames(30)?;
            s.key("LCONTROL", "up", 1)?;
            let snap = s.snapshot()?;
            let standing = snap["zombies"]["nearby"]
                .as_array()
                .is_some_and(|a| a.iter().any(|z| (f(&z["x"]) - zx).abs() < 0.3 && (f(&z["y"]) - zy).abs() < 0.3));
            // Not the zombie count: the population manager removes zombies
            // elsewhere while the player fights.
            if !standing {
                s.shot(o, "after")?;
                return Ok(format!(
                    "the zombie at ({zx}, {zy}) left its spot after {swing} aimed clicks; action states {states:?}"
                ));
            }
        }
        s.shot(o, "after")?;
        let d = ((zx - px).powi(2) + (zy - py).powi(2)).sqrt();
        Err(format!(
            "no attack started: the zombie at ({zx}, {zy}) still stands after 6 aimed clicks from {d:.2} tiles; \
             action states {states:?}"
        ))
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

/// Lua: the nearest door within 30 tiles (`IsoObject.getSquare()` is nil
/// for map objects, so the scan keeps the square); the player faces it.
const FIND_DOOR: &str = r#"
local p = getPlayer()
local px, py, pz = math.floor(p:getX()), math.floor(p:getY()), math.floor(p:getZ())
local best, bd, bsq
for dy = -30, 30 do for dx = -30, 30 do
    local sq = getCell():getGridSquare(px + dx, py + dy, pz)
    if sq then
        local objs = sq:getObjects()
        for i = 0, objs:size() - 1 do
            local o = objs:get(i)
            if instanceof(o, 'IsoDoor') then
                local d = dx * dx + dy * dy
                if not bd or d < bd then best, bd, bsq = o, d, sq end
            end
        end
    end
end end
if not best then return pzctl_json({ error = 'no IsoDoor within 30 tiles of ' .. px .. ',' .. py }) end
p:setDir(best:getNorth() and IsoDirections.N or IsoDirections.W)
pzctl_door = best
return pzctl_json({ x = bsq:getX(), y = bsq:getY(), north = best:getNorth(), open = best:IsOpen() })
"#;

fn door(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "door", DOOR_SPAWN, o)?;
    let d = s.prepare("finding a door", FIND_DOOR)?;
    s.frames(10)?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let is_open = |s: &mut Session| s.lua("return tostring(pzctl_door:IsOpen())").map(|v| v == "true");
        let word = |open: bool| if open { "open" } else { "closed" };
        let open0 = is_open(&mut s)?;
        s.key("E", "tap", 1)?;
        s.frames(30)?;
        s.shot(o, "after-e")?;
        if is_open(&mut s)? != open0 {
            return Ok(format!("E made the door at ({}, {}) {}", d["x"], d["y"], word(!open0)));
        }
        let e = format!(
            "E left the door at ({}, {}) {}: doContext finds no contextual action (the world's \
             context_actions_nswe is not ported)",
            d["x"],
            d["y"],
            word(open0)
        );
        // Fall back to the context menu on the door's edge.
        let (ex, ey) = if d["north"] == true { (f(&d["x"]) + 0.5, f(&d["y"])) } else { (f(&d["x"]), f(&d["y"]) + 0.5) };
        let at = s.lua(&format!(
            "return pzctl_json({{ x = isoToScreenX(0, {ex}, {ey}, 0), y = isoToScreenY(0, {ex}, {ey}, 0) }})"
        ))?;
        s.world_menu(f(&at["x"]), f(&at["y"]) - 40.0).map_err(|m| format!("{e}; {m}"))?;
        s.shot(o, "menu")?;
        s.choose(if open0 { "Close Door" } else { "Open Door" }).map_err(|m| format!("{e}; {m}"))?;
        s.frames(60)?;
        s.shot(o, "after")?;
        if is_open(&mut s)? == open0 {
            return Err(format!("{e}; the context menu left it {} too", word(open0)));
        }
        Ok(format!("the context menu made the door {} ({e})", word(!open0)))
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

fn stances(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "stances", ACTION_SPAWN, o)?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let mut bad = Vec::new();
        s.key("C", "tap", 1)?;
        s.frames(10)?;
        let st = s.state()?;
        if st["sneaking"] != true {
            bad.push(format!("C: sneaking {}", st["sneaking"]));
        }
        s.key("C", "tap", 1)?;
        s.frames(10)?;
        if s.state()?["sneaking"] != false {
            bad.push("second C: still sneaking".into());
        }
        for (mod_key, field) in [("LSHIFT", "running"), ("LMETA", "sprinting")] {
            s.key(mod_key, "down", 1)?;
            s.key("W", "down", 1)?;
            s.frames(20)?;
            let st = s.state()?;
            s.key("W", "up", 1)?;
            s.key(mod_key, "up", 1)?;
            s.frames(10)?;
            if st[field] != true {
                bad.push(format!("{mod_key}+W: {field} {}", st[field]));
            }
        }
        s.shot(o, "after")?;
        if !bad.is_empty() {
            return Err(bad.join("; "));
        }
        Ok("C sneaks and stands up, Shift+W runs, Meta+W sprints".into())
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

fn sleep(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "sleep", ACTION_SPAWN, o)?;
    s.prepare("tiring the player", "getPlayer():getStats():set(CharacterStat.FATIGUE, 0.9) return 'ok'")?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let before = s.state()?;
        let scr = s.snapshot()?["screen"].clone();
        s.world_menu(f(&scr["w"]) / 2.0, f(&scr["h"]) / 2.0)?;
        s.shot(o, "menu")?;
        s.choose("Sleep")?;
        s.frames(5)?;
        // The confirm dialog (ISModalDialog) has a YES button.
        let snap = s.snapshot()?;
        let yes = find_ui(&snap["ui"], &|n| n["text"] == "Yes" || n["text"] == "YES").ok_or("no Yes button after Sleep")?;
        let (x, y) = center(yes).ok_or("Yes has no rect")?;
        s.click(x, y)?;
        s.frames(60)?;
        let asleep = s.state()?;
        s.shot(o, "asleep")?;
        s.frames(600)?;
        let later = s.state()?;
        if asleep["asleep"] != true {
            return Err(format!("not asleep after confirming Sleep; state {asleep}"));
        }
        let (h0, h1) = (f(&before["hours"]), f(&later["hours"]));
        Ok(format!("asleep at speed {}; {:.2} game hours in 600 frames", asleep["speed"], h1 - h0))
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
}

/// Keys that open the character windows, Lua that reads what is open,
/// and the answer each key should give.
const PANELS: &[(&str, &str, &str)] = &[
    ("H", INFO_VIEW, "Health"),
    ("L", INFO_VIEW, "Skills"),
    ("B", HANDCRAFT_OPEN, "true"),
];
/// `ISCharacterInfoWindow`'s active tab when the window is visible.
const INFO_VIEW: &str = "local w = getPlayerInfoPanel(0) \
     return tostring(w and w:isVisible() and w.panel.activeView and w.panel.activeView.name)";
const HANDCRAFT_OPEN: &str = "local h = ISEntityUI.GetWindowInstance(0, 'HandcraftWindow') \
     return tostring(h ~= nil and h:isVisible())";

fn panels(env: &Env, o: &mut Outcome) -> Result<String, String> {
    let mut s = into_action_world(env, "panels", ACTION_SPAWN, o)?;
    let mark = s.c.mark();
    let r = (|| -> Result<String, String> {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        for (key, read, want) in PANELS {
            s.key(key, "tap", 1)?;
            s.frames(10)?;
            s.shot(o, &format!("key-{key}"))?;
            let got = s.lua(read)?;
            if got == *want {
                ok.push(format!("{key} -> {want}"));
            } else {
                bad.push(format!("{key}: {got} instead of {want}"));
            }
        }
        if !bad.is_empty() {
            return Err(format!("{} ({} worked)", bad.join("; "), ok.join(", ")));
        }
        Ok(ok.join(", "))
    })();
    let v = verdict(&s, o, mark, r);
    s.c.quit().ok();
    v
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
