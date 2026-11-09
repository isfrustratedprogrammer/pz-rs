//! `pz-ctl start` keeps one game running behind a Unix socket, so an agent
//! can drive it with one shell command per step.

use crate::client::{Client, Options};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The socket path: `--socket`, `PZ_CTL_SOCKET`, else the temp dir
/// (macOS limits socket paths to about 100 bytes).
pub fn default_socket() -> PathBuf {
    std::env::var_os("PZ_CTL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pz-ctl.sock"))
}

/// One request, one reply, over the socket.
pub fn request(socket: &Path, cmd: &Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket)
        .map_err(|e| format!("no game at {} ({e}); run `pz-ctl start` first", socket.display()))?;
    writeln!(s, "{cmd}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    if line.is_empty() {
        return Err("the pz-ctl server closed the connection without a reply".into());
    }
    serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))
}

/// `pz-ctl start`: run `pz-ctl serve` in the background and wait until it
/// answers.
pub fn start(socket: &Path, global_args: &[String], client_args: &[String], log: &Path) -> Result<Value, String> {
    if request(socket, &json!({"cmd": "ping"})).is_ok() {
        return Err(format!("a game is already running at {}; `pz-ctl stop` it first", socket.display()));
    }
    let serve_log = log.with_extension("serve.log");
    if let Some(d) = serve_log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let out = std::fs::File::create(&serve_log).map_err(|e| format!("{}: {e}", serve_log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(global_args)
        .arg("serve")
        .args(client_args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(240);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let msg = std::fs::read_to_string(&serve_log).unwrap_or_default();
            return Err(format!("pz-ctl serve exited ({status}):\n{msg}"));
        }
        if let Ok(v) = request(socket, &json!({"cmd": "ping"})) {
            return Ok(v);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    Err("the game did not come up within 240 s".into())
}

/// `pz-ctl serve`: own the game and answer socket requests until `quit`
/// or until the game dies.
pub fn serve(opts: &Options, socket: &Path, args: &[String]) -> Result<(), String> {
    let (mut client, ready) = Client::start(opts, args)?;
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    eprintln!("pz-ctl: game ready at {} ({ready})", socket.display());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() || line.trim().is_empty() {
            continue;
        }
        let cmd: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(stream, "{}", json!({"ok": false, "error": format!("bad JSON: {e}")}));
                continue;
            }
        };
        if cmd["cmd"] == "quit" {
            let reply = client.quit().unwrap_or_else(|e| json!({"ok": false, "error": e}));
            let _ = writeln!(stream, "{reply}");
            break;
        }
        let reply = client.send(&cmd).unwrap_or_else(|e| json!({"ok": false, "error": e}));
        let _ = writeln!(stream, "{reply}");
        if client.exit.is_some() {
            break;
        }
    }
    let _ = std::fs::remove_file(socket);
    Ok(())
}
