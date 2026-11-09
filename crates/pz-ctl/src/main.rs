//! `pz-ctl`: drive `pz-client` from the shell, so an agent can play,
//! look and check without a person at the keyboard. See
//! `docs/AGENT-CONTROL.md`.

mod client;
mod daemon;
mod logs;
mod scenarios;

use client::{Client, Options};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "\
pz-ctl [OPTIONS] COMMAND [ARGS]

Options (before the command):
  --cache-dir DIR   the game's cache and saves (required to start a game;
                    never ~/Zomboid)
  --game-dir DIR    the Project Zomboid install (default: found by pz-client)
  --client PATH     the pz-client binary (default: next to pz-ctl, or $PZ_CLIENT)
  --socket PATH     the background game's socket (default: $PZ_CTL_SOCKET or
                    $TMPDIR/pz-ctl.sock)
  --log FILE        the game's log (default: <cache-dir>/pz-ctl/game.log)
  --timeout SECS    longest wait for one reply (default 300)
  --no-gpu          no offscreen rendering (faster; no screenshots)

A game in the background (one command per step):
  start [CLIENT ARGS]      boot pz-client --control, e.g. `start --new-game X`
                           or `start --load-game Sandbox/X`
  frames N                 run N frames
  key NAME|CODE [--hold N | --down | --up]
                           tap (default), hold for N frames, press or release
  text STRING              type characters
  mouse X Y                move the cursor
  click X Y [--right] [--hold N]
  resize W H               resize the screen
  screenshot FILE.png
  snapshot                 state, player, zombies, UI and the log since the
                           last snapshot, as JSON
  lua CODE                 run Lua; `return pzctl_json(t)` returns JSON
  send JSON                any protocol command
  stop                     quit the game (saves a game in progress)

Without a background game:
  session [CLIENT ARGS]    JSON commands on stdin, replies on stdout
  scenarios [--out DIR] [--spawn X,Y,Z] [NAME...]
                           run the checks and print PASS/FAIL (exit 1 on FAIL)
  list                     list the scenarios
";

struct Global {
    raw: Vec<String>,
    cache_dir: Option<PathBuf>,
    game_dir: Option<PathBuf>,
    client: Option<PathBuf>,
    socket: PathBuf,
    log: Option<PathBuf>,
    timeout: Duration,
    gpu: bool,
}

impl Global {
    fn options(&self, default_log: &str) -> Result<Options, String> {
        let cache_dir = self.cache_dir.clone().ok_or("--cache-dir is required to start a game")?;
        let client = match &self.client {
            Some(c) => c.clone(),
            None => match std::env::var_os("PZ_CLIENT") {
                Some(c) => PathBuf::from(c),
                None => std::env::current_exe()
                    .map_err(|e| e.to_string())?
                    .with_file_name("pz-client"),
            },
        };
        let log = self.log.clone().unwrap_or_else(|| cache_dir.join("pz-ctl").join(default_log));
        Ok(Options { client, cache_dir, game_dir: self.game_dir.clone(), log, timeout: self.timeout, gpu: self.gpu })
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pz-ctl: {e}");
            2
        }
    });
}

fn run(args: Vec<String>) -> Result<i32, String> {
    let mut g = Global {
        raw: Vec::new(),
        cache_dir: None,
        game_dir: None,
        client: None,
        socket: daemon::default_socket(),
        log: None,
        timeout: Duration::from_secs(300),
        gpu: true,
    };
    let mut it = args.into_iter().peekable();
    while let Some(a) = it.peek().cloned() {
        if !a.starts_with("--") {
            break;
        }
        it.next();
        let mut val = || it.next().ok_or(format!("{a} takes a value"));
        match a.as_str() {
            "--cache-dir" => g.cache_dir = Some(absolute(val()?)),
            "--game-dir" => g.game_dir = Some(absolute(val()?)),
            "--client" => g.client = Some(absolute(val()?)),
            "--socket" => g.socket = PathBuf::from(val()?),
            "--log" => g.log = Some(absolute(val()?)),
            "--timeout" => g.timeout = Duration::from_secs_f64(val()?.parse().map_err(|_| "--timeout takes seconds")?),
            "--no-gpu" => {
                g.gpu = false;
                g.raw.push(a);
                continue;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(0);
            }
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
        // Re-pass the resolved value to `serve`.
        let v = match a.as_str() {
            "--cache-dir" => g.cache_dir.as_ref().map(|p| p.display().to_string()),
            "--game-dir" => g.game_dir.as_ref().map(|p| p.display().to_string()),
            "--client" => g.client.as_ref().map(|p| p.display().to_string()),
            "--socket" => Some(g.socket.display().to_string()),
            "--log" => g.log.as_ref().map(|p| p.display().to_string()),
            _ => Some(g.timeout.as_secs_f64().to_string()),
        };
        g.raw.push(a);
        g.raw.extend(v);
    }
    let Some(cmd) = it.next() else {
        print!("{USAGE}");
        return Ok(2);
    };
    let rest: Vec<String> = it.collect();
    let num = |i: usize| -> Result<f64, String> {
        rest.get(i)
            .ok_or(format!("{cmd}: missing argument {}", i + 1))?
            .parse::<f64>()
            .map_err(|_| format!("{cmd}: argument {} is not a number", i + 1))
    };
    let flag_val = |name: &str| rest.iter().position(|a| a == name).and_then(|i| rest.get(i + 1)).cloned();
    let request = match cmd.as_str() {
        "start" => {
            let opts = g.options("game.log")?;
            let v = daemon::start(&g.socket, &g.raw, &rest, &opts.log)?;
            return Ok(print_reply(&v));
        }
        "serve" => {
            let opts = g.options("game.log")?;
            daemon::serve(&opts, &g.socket, &rest)?;
            return Ok(0);
        }
        "session" => return session(&g, &rest),
        "scenarios" => return run_scenarios(&g, &rest),
        "list" => {
            for s in scenarios::ALL {
                println!("{:<15} {}", s.name, s.about);
            }
            return Ok(0);
        }
        "stop" => json!({"cmd": "quit"}),
        "frames" => json!({"cmd": "frames", "n": num(0)? as u64}),
        "key" => {
            let key = rest.first().ok_or("key: missing key name or code")?;
            let key = key.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(key.as_str()));
            let mut v = json!({"cmd": "key", "key": key, "action": "tap"});
            if let Some(n) = flag_val("--hold") {
                v["action"] = "hold".into();
                v["frames"] = n.parse::<u64>().map_err(|_| "--hold takes frames")?.into();
            } else if rest.iter().any(|a| a == "--down") {
                v["action"] = "down".into();
            } else if rest.iter().any(|a| a == "--up") {
                v["action"] = "up".into();
            }
            v
        }
        "text" => json!({"cmd": "text", "text": rest.join(" ")}),
        "mouse" => json!({"cmd": "mouse", "x": num(0)?, "y": num(1)?}),
        "click" => {
            let mut v = json!({"cmd": "click", "x": num(0)?, "y": num(1)?});
            if rest.iter().any(|a| a == "--right") {
                v["button"] = "right".into();
            }
            if let Some(n) = flag_val("--hold") {
                v["frames"] = n.parse::<u64>().map_err(|_| "--hold takes frames")?.into();
            }
            v
        }
        "resize" => json!({"cmd": "resize", "w": num(0)?, "h": num(1)?}),
        "screenshot" => {
            let p = rest.first().ok_or("screenshot: missing file")?;
            json!({"cmd": "screenshot", "path": absolute(p.clone())})
        }
        "snapshot" => json!({"cmd": "snapshot"}),
        "lua" => json!({"cmd": "lua", "code": rest.join(" ")}),
        "send" => serde_json::from_str(&rest.join(" ")).map_err(|e| format!("send: bad JSON: {e}"))?,
        other => return Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    let v = daemon::request(&g.socket, &request)?;
    Ok(print_reply(&v))
}

fn absolute(p: impl Into<PathBuf>) -> PathBuf {
    let p = p.into();
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

/// Pretty JSON on stdout; exit status 1 unless `ok`.
fn print_reply(v: &Value) -> i32 {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    if v["ok"] == true {
        0
    } else {
        1
    }
}

/// `pz-ctl session`: one game for the lifetime of stdin.
fn session(g: &Global, client_args: &[String]) -> Result<i32, String> {
    let opts = g.options("session.log")?;
    let (mut c, ready) = Client::start(&opts, client_args)?;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{ready}");
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let cmd: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("bad JSON: {e}")}));
                continue;
            }
        };
        if cmd["cmd"] == "quit" {
            let r = c.quit().unwrap_or_else(|e| json!({"ok": false, "error": e}));
            let _ = writeln!(out, "{r}");
            return Ok(0);
        }
        let r = c.send(&cmd).unwrap_or_else(|e| json!({"ok": false, "error": e}));
        let _ = writeln!(out, "{r}");
        let _ = out.flush();
        if c.exit.is_some() {
            return Ok(1);
        }
    }
    let _ = c.quit();
    Ok(0)
}

fn run_scenarios(g: &Global, rest: &[String]) -> Result<i32, String> {
    let mut out = None;
    let mut spawn = None;
    let mut only = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--out" {
            out = it.next().map(|o| absolute(o.clone()));
        } else if a == "--spawn" {
            spawn = Some(it.next().ok_or("--spawn needs X,Y,Z")?.clone());
        } else {
            if !scenarios::ALL.iter().any(|s| s.name == a) {
                return Err(format!("unknown scenario {a} (see `pz-ctl list`)"));
            }
            only.push(a.clone());
        }
    }
    let opts = g.options("unused.log")?;
    let out = out.unwrap_or_else(|| opts.cache_dir.join("pz-ctl").join("report"));
    let env = scenarios::Env { opts, out: out.clone(), spawn };
    let report = scenarios::run(&env, &only);
    print!("{}", scenarios::summary(&report));
    println!("report: {}", out.join("report.json").display());
    Ok(if report["failed"] == 0 { 0 } else { 1 })
}
