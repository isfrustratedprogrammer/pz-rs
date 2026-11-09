//! The game's log (everything on its stdout/stderr that is not a protocol
//! line), sorted into the kinds of trouble an agent looks for.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A Kahlua runtime error (the line after the `-----` separator).
    LuaError,
    /// `require("X") failed`.
    RequireFailed,
    /// `no such function "X.y"` (OnCreate and similar callbacks).
    NoSuchFunction,
    /// `ERROR:` lines other than the above.
    Error,
    /// `X is a null texture.`
    NullTexture,
    /// `WARN :` lines.
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
        if line.starts_with("LUAERR") {
            return Some(Kind::LuaError);
        }
        if line.contains("require(\"") && line.trim_end().ends_with("\") failed") {
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
}

const TAIL: usize = 40;
const SAMPLES: usize = 10;

impl LogBook {
    pub fn push(&mut self, line: &str) {
        self.lines += 1;
        if let Some(k) = self.classifier.classify(line) {
            self.entries.push((k, line.trim_end().to_string()));
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
        assert_eq!(c.classify("require(\"ISUI/ISContextMenu\") failed"), Some(Kind::RequireFailed));
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
