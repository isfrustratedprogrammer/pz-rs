//! A `pz-client --control` process: commands go to its stdin; its stdout
//! and stderr share one pipe, so protocol replies and log lines arrive in
//! the order the game wrote them.

use crate::logs::LogBook;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const REPLY_PREFIX: &str = "@@pzctl ";

#[derive(Clone, Debug)]
pub struct Options {
    /// The `pz-client` binary.
    pub client: PathBuf,
    /// `--cache-dir` (saves, options); never the user's `~/Zomboid`.
    pub cache_dir: PathBuf,
    pub game_dir: Option<PathBuf>,
    /// Where the game's log goes.
    pub log: PathBuf,
    /// Longest wait for one reply.
    pub timeout: Duration,
    pub gpu: bool,
}

/// One protocol reply and how many classified log entries preceded it.
struct Reply {
    value: Value,
    mark: usize,
}

pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Reply>,
    pub log: Arc<Mutex<LogBook>>,
    timeout: Duration,
    /// Log position of the last reply / the last snapshot.
    mark: usize,
    snapshot_mark: usize,
    /// Set once the process is gone.
    pub exit: Option<String>,
}

impl Client {
    /// Spawn the game and wait until it has booted. `args` are extra
    /// `pz-client` arguments (`--new-game NAME`, `--load-game MODE/NAME`).
    pub fn start(opts: &Options, args: &[String]) -> Result<(Client, Value), String> {
        if let Some(dir) = opts.log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut file = std::fs::File::create(&opts.log).map_err(|e| format!("{}: {e}", opts.log.display()))?;
        let (reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
        let writer2 = writer.try_clone().map_err(|e| format!("pipe: {e}"))?;
        let mut cmd = Command::new(&opts.client);
        cmd.arg("--control").arg("--cache-dir").arg(&opts.cache_dir);
        if let Some(g) = &opts.game_dir {
            cmd.arg("--game-dir").arg(g);
        }
        if !opts.gpu {
            cmd.arg("--no-gpu");
        }
        cmd.args(args).stdin(Stdio::piped()).stdout(writer).stderr(writer2);
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", opts.client.display()))?;
        // The pipe reaches EOF only once no writer is left in this process.
        drop(cmd);
        let stdin = child.stdin.take();
        let log = Arc::new(Mutex::new(LogBook::default()));
        let (tx, rx) = channel();
        let book = log.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\n', '\r']);
                if let Some(j) = line.strip_prefix(REPLY_PREFIX) {
                    let value = serde_json::from_str(j).unwrap_or_else(|e| json!({"ok": false, "error": format!("bad reply: {e}")}));
                    let mark = book.lock().unwrap().entries.len();
                    if tx.send(Reply { value, mark }).is_err() {
                        break;
                    }
                } else {
                    let _ = writeln!(file, "{line}");
                    book.lock().unwrap().push(line);
                }
            }
        });
        let mut c = Client {
            child,
            stdin,
            replies: rx,
            log,
            timeout: opts.timeout,
            mark: 0,
            snapshot_mark: 0,
            exit: None,
        };
        let ready = c.wait_reply(opts.timeout.max(Duration::from_secs(180)))?;
        if ready["ok"] != true {
            return Err(format!("boot failed: {}", ready["error"]));
        }
        Ok((c, ready))
    }

    /// Send one command and wait for its reply. A `snapshot` reply gets
    /// `log`: what the game logged since the previous snapshot.
    pub fn send(&mut self, cmd: &Value) -> Result<Value, String> {
        let timeout = cmd
            .get("timeout_s")
            .and_then(Value::as_f64)
            .map_or(self.timeout, Duration::from_secs_f64);
        let stdin = self.stdin.as_mut().ok_or("the game's stdin is closed")?;
        if let Err(e) = writeln!(stdin, "{cmd}").and_then(|_| stdin.flush()) {
            return Err(self.exited(&format!("write: {e}")));
        }
        let mut v = self.wait_reply(timeout)?;
        if v.get("snapshot").is_some() {
            let s = self.log_since(self.snapshot_mark);
            self.snapshot_mark = self.mark;
            v["snapshot"]["log"] = s;
        }
        Ok(v)
    }

    /// The log summary from `mark` up to the last reply.
    pub fn log_since(&self, mark: usize) -> Value {
        self.log.lock().unwrap().summary(mark, self.mark)
    }

    /// Log position of the last reply.
    pub fn mark(&self) -> usize {
        self.mark
    }

    fn wait_reply(&mut self, timeout: Duration) -> Result<Value, String> {
        match self.replies.recv_timeout(timeout) {
            Ok(r) => {
                self.mark = r.mark;
                Ok(r.value)
            }
            Err(RecvTimeoutError::Timeout) => Err(format!("no reply within {:.0} s", timeout.as_secs_f64())),
            Err(RecvTimeoutError::Disconnected) => Err(self.exited("the game closed its output")),
        }
    }

    /// Describe a dead process: exit status and the last log lines.
    fn exited(&mut self, why: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(s)) => break s.to_string(),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => break "still running".into(),
            }
        };
        let tail = self.log.lock().unwrap().tail();
        let msg = format!("game process gone ({why}; {status}); last log lines:\n{}", tail.join("\n"));
        self.exit = Some(msg.clone());
        msg
    }

    /// `quit` (the window-close path, which saves a game in progress),
    /// then wait for the process to end.
    pub fn quit(mut self) -> Result<Value, String> {
        let r = self.send(&json!({"cmd": "quit", "timeout_s": 120}));
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        r
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for `pz-client --control` written as a shell script.
    fn fake_client(name: &str, body: &str) -> (Options, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pzctl-client-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-client");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opts = Options {
            client: script,
            cache_dir: dir.join("cache"),
            game_dir: None,
            log: dir.join("game.log"),
            timeout: Duration::from_secs(10),
            gpu: false,
        };
        (opts, dir)
    }

    #[test]
    fn snapshot_gets_the_log_written_before_it() {
        let (opts, dir) = fake_client(
            "log",
            r#"echo 'require("A") failed' >&2
echo '@@pzctl {"event":"ready","ok":true}'
while read line; do
  echo '-------------------------------------------------------------' >&2
  echo 'attempted index: x of non-table: null' >&2
  echo '@@pzctl {"ok":true,"snapshot":{}}'
done
"#,
        );
        let (mut c, ready) = Client::start(&opts, &[]).unwrap();
        assert_eq!(ready["event"], "ready");
        let first = c.send(&json!({"cmd": "snapshot"})).unwrap();
        assert_eq!(first["snapshot"]["log"]["counts"]["require_failed"], 1);
        assert_eq!(first["snapshot"]["log"]["counts"]["lua_error"], 1);
        let second = c.send(&json!({"cmd": "snapshot"})).unwrap();
        assert_eq!(second["snapshot"]["log"]["counts"]["require_failed"], 0);
        assert_eq!(second["snapshot"]["log"]["counts"]["lua_error"], 1);
        c.quit().unwrap();
        let log = std::fs::read_to_string(&opts.log).unwrap();
        assert!(log.starts_with("require(\"A\") failed\n"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dead_game_is_reported_with_its_last_lines() {
        let (opts, dir) = fake_client(
            "dead",
            "echo '@@pzctl {\"event\":\"ready\",\"ok\":true}'\nread line\necho 'thread main panicked at boom' >&2\nexit 3\n",
        );
        let (mut c, _) = Client::start(&opts, &[]).unwrap();
        let e = c.send(&json!({"cmd": "frames", "n": 1})).unwrap_err();
        assert!(e.contains("game process gone"), "{e}");
        assert!(e.contains("panicked at boom"), "{e}");
        assert!(c.exit.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn boot_failure_is_an_error() {
        let (opts, dir) = fake_client("boot", "echo '@@pzctl {\"event\":\"boot_failed\",\"ok\":false,\"error\":\"no Lua\"}'\n");
        let e = Client::start(&opts, &[]).err().unwrap();
        assert!(e.contains("no Lua"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
