//! The game's log (everything on its stdout/stderr that is not a protocol
//! line), sorted into the kinds of trouble an agent looks for.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A Kahlua runtime error: the line after the `-----` separator, or an
    /// event handler's `<message> java.lang.RuntimeException` line.
    LuaError,
    /// `require("X") failed`, except [`VANILLA_REQUIRE_FAILURES`].
    RequireFailed,
    /// `no such function "X.y"` (OnCreate and similar callbacks).
    NoSuchFunction,
    /// `ERROR:` lines other than the above.
    Error,
    /// `X is a null texture.`
    NullTexture,
    /// `WARN :` lines, and the require failures the original also logs.
    Warning,
    /// A Rust panic.
    Panic,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::LuaError,
        Kind::RequireFailed,
        Kind::NoSuchFunction,
        Kind::Error,
        Kind::NullTexture,
        Kind::Warning,
        Kind::Panic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::LuaError => "lua_error",
            Kind::RequireFailed => "require_failed",
            Kind::NoSuchFunction => "no_such_function",
            Kind::Error => "error",
            Kind::NullTexture => "null_texture",
            Kind::Warning => "warning",
            Kind::Panic => "panic",
        }
    }
}

const SEPARATOR: &str = "-------------------------------------------------------------";

/// Modules that the original game also fails to require at boot. It logs
/// them with `DebugType.Lua.warn`. `LuaManager.require` searches only the
/// `paths` that `LoadDirBase` has added so far, and `LoadDirBase()` loads
/// `shared` before it adds `client`. So shared files that require client
/// modules fail, and so does a client file that requires a server module.
/// The last two paths do not exist in the game.
pub const VANILLA_REQUIRE_FAILURES: &[&str] = &[
    "ISUI/ISInventoryPaneContextMenu",
    "ISUI/ISVehicleMenu",
    "ISUI/ISContextMenu",
    "Camping/CCampfireSystem",
    "TimedActions/ISInventoryTransferAction",
    "ISUI/PlayerData/ISPlayerData",
    "BuildingObjects/ISAnimalPickMateCursor",
    "ISUI/InventoryWindow/ISLootWindowControlHandler",
    "pzapi/ui/ui",
];

/// The module of a `require("X") failed` line.
fn required_module(line: &str) -> Option<&str> {
    let rest = &line[line.find("require(\"")? + "require(\"".len()..];
    let module = rest.strip_suffix("\") failed")?;
    Some(module)
}

const RUNTIME_EXCEPTION: &str = " java.lang.RuntimeException";

/// An error thrown in an event handler (`Events.OnKeyPressed` and the
/// like): `<message> java.lang.RuntimeException`, then `at <function>:<line>`
/// lines, without the `-----` separator.
fn is_event_error(line: &str) -> bool {
    line.trim_end().ends_with(RUNTIME_EXCEPTION) && !line.starts_with("ERROR:") && !line.starts_with("WARN")
}

/// Sorts lines one at a time; Lua errors need the previous line.
#[derive(Default)]
pub struct Classifier {
    after_separator: bool,
}

impl Classifier {
    pub fn classify(&mut self, line: &str) -> Option<Kind> {
        let after = std::mem::replace(&mut self.after_separator, line.trim_end() == SEPARATOR);
        if self.after_separator {
            return None;
        }
        if after {
            return Some(Kind::LuaError);
        }
        if line.starts_with("LUAERR") || is_event_error(line) {
            return Some(Kind::LuaError);
        }
        if let Some(module) = required_module(line.trim_end()) {
            if VANILLA_REQUIRE_FAILURES.contains(&module) {
                return Some(Kind::Warning);
            }
            return Some(Kind::RequireFailed);
        }
        if line.contains("no such function \"") {
            return Some(Kind::NoSuchFunction);
        }
        if line.contains("panicked at") {
            return Some(Kind::Panic);
        }
        if line.trim_end().ends_with("is a null texture.") {
            return Some(Kind::NullTexture);
        }
        if line.starts_with("ERROR:") {
            return Some(Kind::Error);
        }
        if line.starts_with("WARN :") {
            return Some(Kind::Warning);
        }
        None
    }
}

/// Every classified line in order, plus the last lines for crash reports.
#[derive(Default)]
pub struct LogBook {
    classifier: Classifier,
    pub entries: Vec<(Kind, String)>,
    pub lines: usize,
    tail: VecDeque<String>,
    /// The last entry is an event error still waiting for its first
    /// `at <function>:<line>`.
    wants_at: bool,
}

const TAIL: usize = 40;
const SAMPLES: usize = 10;

impl LogBook {
    pub fn push(&mut self, line: &str) {
        self.lines += 1;
        let wants_at = std::mem::take(&mut self.wants_at);
        if let Some(k) = self.classifier.classify(line) {
            self.entries.push((k, line.trim_end().to_string()));
            self.wants_at = k == Kind::LuaError && is_event_error(line);
        } else if wants_at && line.starts_with("at ") {
            // The function and line make the sample useful.
            if let Some((_, l)) = self.entries.last_mut() {
                l.truncate(l.len() - RUNTIME_EXCEPTION.len());
                l.push_str(", ");
                l.push_str(line.trim_end());
            }
        }
        if self.tail.len() == TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line.trim_end().to_string());
    }

    pub fn tail(&self) -> Vec<String> {
        self.tail.iter().cloned().collect()
    }

    /// Counts and a few distinct samples per kind for `entries[from..to]`.
    pub fn summary(&self, from: usize, to: usize) -> Value {
        let to = to.min(self.entries.len());
        let from = from.min(to);
        let mut counts: BTreeMap<Kind, usize> = BTreeMap::new();
        let mut samples: BTreeMap<Kind, Vec<&str>> = BTreeMap::new();
        for (k, l) in &self.entries[from..to] {
            *counts.entry(*k).or_default() += 1;
            let s = samples.entry(*k).or_default();
            if s.len() < SAMPLES && !s.contains(&l.as_str()) {
                s.push(l);
            }
        }
        let mut c = Map::new();
        let mut s = Map::new();
        for k in Kind::ALL {
            c.insert(k.name().into(), json!(counts.get(&k).copied().unwrap_or(0)));
            if let Some(v) = samples.get(&k) {
                if k != Kind::NullTexture && k != Kind::Warning {
                    s.insert(k.name().into(), json!(v));
                }
            }
        }
        json!({"counts": c, "samples": s})
    }
}

/// `summary["counts"][kind]`.
pub fn count(summary: &Value, k: Kind) -> u64 {
    summary["counts"][k.name()].as_u64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_lines() {
        let mut c = Classifier::default();
        assert_eq!(c.classify("require(\"ISUI/ISContextMenu\") failed"), Some(Kind::Warning));
        assert_eq!(c.classify("require(\"ISUI/ISInventoryPage\") failed"), Some(Kind::RequireFailed));
        assert_eq!(
            c.classify("WARN : Lua f:0 at Lua(Vanilla).ISInventoryTransferUtil.lua> require(\"TimedActions/ISInventoryTransferAction\") failed"),
            Some(Kind::Warning)
        );
        assert_eq!(
            c.classify("ERROR: General      f:0 at (UnknownStack)    > no such function \"ItemCodeOnCreate.onCreateIDCard\""),
            Some(Kind::NoSuchFunction)
        );
        assert_eq!(c.classify("ERROR: General f:0> something else"), Some(Kind::Error));
        assert_eq!(c.classify("media/inventory/world/WItem_PipeBomb.png is a null texture."), Some(Kind::NullTexture));
        assert_eq!(c.classify("WARN : Lua f:0 > color not found"), Some(Kind::Warning));
        assert_eq!(c.classify("thread 'main' panicked at src/x.rs:1:1:"), Some(Kind::Panic));
        assert_eq!(c.classify("LOG  : General f:0> binkId: -1"), None);
    }

    #[test]
    fn lua_error_is_the_line_after_the_separator() {
        let mut c = Classifier::default();
        assert_eq!(c.classify(SEPARATOR), None);
        assert_eq!(c.classify("attempted index: isAnyVisible of non-table: null"), Some(Kind::LuaError));
        assert_eq!(c.classify("Could not find video"), None);
    }

    #[test]
    fn event_handler_errors_are_lua_errors_with_their_function() {
        let mut b = LogBook::default();
        b.push("Object tried to call nil in displayCharacterInfo java.lang.RuntimeException");
        b.push("at displayCharacterInfo:128");
        b.push("at onKeyPressed:1650");
        b.push("ERROR: General f:0> x java.lang.RuntimeException");
        let s = b.summary(0, b.entries.len());
        assert_eq!(count(&s, Kind::LuaError), 1);
        assert_eq!(count(&s, Kind::Error), 1);
        assert_eq!(
            s["samples"]["lua_error"][0],
            "Object tried to call nil in displayCharacterInfo, at displayCharacterInfo:128"
        );
    }

    #[test]
    fn summary_counts_a_range_and_dedups_samples() {
        let mut b = LogBook::default();
        b.push("require(\"A\") failed");
        let mark = b.entries.len();
        for _ in 0..3 {
            b.push(SEPARATOR);
            b.push("attempted index: x of non-table: null");
        }
        b.push("t.png is a null texture.");
        let s = b.summary(mark, b.entries.len());
        assert_eq!(count(&s, Kind::RequireFailed), 0);
        assert_eq!(count(&s, Kind::LuaError), 3);
        assert_eq!(count(&s, Kind::NullTexture), 1);
        assert_eq!(s["samples"]["lua_error"].as_array().unwrap().len(), 1);
        assert!(s["samples"].get("null_texture").is_none());
        assert_eq!(b.lines, 8);
        assert_eq!(b.tail().last().unwrap(), "t.png is a null texture.");
    }
}
