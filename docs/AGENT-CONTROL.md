# Agent control (`pz-ctl`)

`pz-ctl` lets an agent run the game, press keys, click, take screenshots and
read the game state as JSON, without a person at the keyboard. It has three
parts:

- `pz-client --control`: the game boots without a window and runs frames
  only when told to. Commands arrive on stdin as JSON lines; each reply is one
  stdout line `@@pzctl <json>`. Every other output line is the game's log.
- `pz-ctl`: the shell front end. It starts the client, sends commands, sorts
  the log into error kinds and runs pass/fail scenarios.
- `pz_engine::game_window::control::Controller`: the engine side (boot, step
  frames, inject input, resize, screenshot, Lua eval, snapshot, quit).

Frames are deterministic in time: every frame advances 33 ms of game time,
however long it takes to compute. Wall-clock time still passes for Lua code
that reads real time (for example the main menu's intro `delay`).

## Build

```bash
nix shell nixpkgs#cargo nixpkgs#rustc -c env LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib cargo build --release -p pz-client -p pz-ctl
```

`pz-ctl` finds `pz-client` next to itself (`target/release`), or use
`--client PATH` / `PZ_CLIENT`.

Always pass `--cache-dir`: it holds options, saves and the logs `pz-ctl`
writes. `pz-ctl` refuses to start a game without it, so it never writes to
`~/Zomboid` or `~/Zomboid-rs`.

## A game in the background

One shell command per step. The game stays up behind a Unix socket
(`$TMPDIR/pz-ctl.sock`, or `--socket` / `PZ_CTL_SOCKET`).

```bash
pz-ctl --cache-dir ../pz-cache start                      # main menu
pz-ctl --cache-dir ../pz-cache start --new-game test1     # straight into a new game
pz-ctl --cache-dir ../pz-cache start --load-game Sandbox/test1
pz-ctl frames 300
pz-ctl snapshot
pz-ctl click 149 381                 # left click; --right, --hold N
pz-ctl key W --hold 60               # hold W for 60 frames; --down / --up
pz-ctl key ESCAPE                    # tap
pz-ctl text hello
pz-ctl mouse 640 360
pz-ctl resize 1600 900
pz-ctl screenshot shots/now.png
pz-ctl lua 'return getPlayer():getX()'
pz-ctl lua 'return pzctl_json({hp = getPlayer():getHealth()})'
pz-ctl send '{"cmd":"frames","n":10}'
pz-ctl stop                          # quit; saves a game in progress
```

Each command prints the reply as JSON and exits with 0 when `ok` is true,
1 otherwise. Every reply carries `frame`, `state` (the current
`GameStateMachine` state class) and `engine_error` (the first engine error;
once set, frames no longer run). The game's log goes to
`<cache-dir>/pz-ctl/game.log` (`--log` to change); `start` also writes
`game.serve.log` next to it.

Without a socket, `pz-ctl session [CLIENT ARGS]` reads JSON commands on stdin
and prints one reply per line on stdout for one game.

## Protocol

Commands are JSON objects with `cmd`. An optional `id` is echoed back.

| `cmd` | Fields | Does |
| --- | --- | --- |
| `ping` | | Nothing; returns the status. |
| `frames` | `n` | Runs `n` frames (`ran` = frames actually run). |
| `key` | `key` (LWJGL code or `Keyboard.KEY_<NAME>` name: `"W"`, `"ESCAPE"`, `"SPACE"`, `"F1"`), `action` (`tap`, `hold`, `down`, `up`), `frames` | `tap`/`hold`: press, run `frames` (default 1), release, run 1 frame. `down`/`up`: only the event. |
| `text` | `text` | Typed characters, then 1 frame. |
| `mouse` | `x`, `y`, `frames` | Moves the cursor (window pixels, y from the top), then runs `frames` (default 1). |
| `click` | `x`, `y`, `button` (`left`, `right`, `middle`), `frames` | Move, 1 frame, press, `frames` frames (default 1), release, 1 frame. |
| `resize` | `w`, `h`, `frames` | The window resize path (`Core.setScreenSize`, which queues `OnResolutionChange`), then `frames` (default 1). |
| `screenshot` | `path` | Writes the last rendered frame as PNG (not with `--no-gpu`). |
| `snapshot` | | See below. |
| `lua` | `code` | Runs a Lua chunk in the game's globals; `result` is the returned values through `tostring`, and `json` is the first one parsed when it is a JSON object or array. |
| `quit` | | Closes as the window close does (saves a game in progress); the reply's `error` is what close reported. |

`pz-ctl` also reads `timeout_s` in any command (default `--timeout`, 300 s).

Two Lua helpers exist after boot: `pzctl_json(value)` encodes a Lua value as
JSON (tables up to depth 6), and `pzctl_player()` returns the player fields
the snapshot reads through Lua.

### Snapshot

```json
{
  "frame": 1170, "state": "zombie.gameStates.IngameState", "paused": false,
  "engine_error": null, "screen": {"w": 1280, "h": 720},
  "player": {"x": 10776.5, "y": 9764.5, "z": 0.0, "action_state": "",
             "health": 1, "dead": false, "inventory": ["Base.IDcard", "Base.KeyRing"],
             "errors": {}},
  "zombies": {"total": 87, "radius": 30.0,
              "nearby": [{"x": 10773.5, "y": 9761.5, "z": 0.0, "dist": 4.24}]},
  "ui": [{"class": "zombie.ui.UIElement", "type": "SurvivalGuide", "text": "Survival Guide",
          "rect": [67, 30, 1145, 659], "children": [
            {"type": "ISButton", "text": "CLOSE", "rect": [1110, 662, 100, 25]}]}],
  "log": {"counts": {"lua_error": 3, "require_failed": 0, "no_such_function": 0,
                     "error": 0, "null_texture": 0, "warning": 0, "panic": 0},
          "samples": {"lua_error": ["attempted index: isAnyVisible of non-table: null"]}}
}
```

- `player` is null without a local player. `x`, `y`, `z` and `action_state`
  come from the engine; `health`, `dead` and `inventory` come from Lua, and
  failures to read them are listed in `errors` (an empty object when none).
- `zombies.nearby`: the 20 nearest within 30 tiles of the player.
- `ui`: visible top-level UI elements and their visible children, three
  levels deep. `type` is the Lua table's `Type`, `text` its `name` or
  `title` (a label's text, a window title), `rect` is absolute
  `[x, y, width, height]`; click the centre of a `rect` to press it.
- `log` (added by `pz-ctl`): what the game logged since the previous
  snapshot (the first snapshot counts from boot). Kinds: `lua_error` (the
  line after Kahlua's `-----` separator), `require_failed`,
  `no_such_function`, `error` (other `ERROR:` lines), `null_texture`,
  `warning`, `panic`. Up to 10 distinct samples per kind, none for
  `null_texture` and `warning`.

## Scenarios

```bash
pz-ctl --cache-dir ../pz-cache list
pz-ctl --cache-dir ../pz-cache scenarios                    # all
pz-ctl --cache-dir ../pz-cache scenarios menu walk --out ../pz-ctl-report
```

Each scenario boots its own game process. The output is a PASS/FAIL line per
scenario and a summary; `--out` (default `<cache-dir>/pz-ctl/report`) gets
`report.json`, screenshots and one game log per scenario. The exit status is
1 when any scenario fails. Failures that match a known bug name it
(`known bug:`).

| Scenario | Passes when |
| --- | --- |
| `menu` | After 60 frames the state is `MainScreenState`, the UI lists SOLO, LOAD, OPTIONS and QUIT, and the screenshot is not blank. |
| `boot-clean` | Boot and 60 menu frames log no failed `require`, Lua error, missing function, `ERROR:` line or panic. |
| `menu-solo` | After the menu's intro delay, a click on SOLO adds new UI (the new-game screen) without Lua errors. |
| `resize` | Resizing to 1600x900, 1024x768 and 1280x720 updates the screen size and logs no Lua errors. |
| `new-game` | `--new-game` reaches `IngameState` within 3000 frames with a living player. |
| `survival-guide` | The Survival Guide shown at game start closes with its CLOSE button, without Lua errors. |
| `walk` | Holding W for 60 frames moves the player at least 0.5 tiles. |
| `ingame-stable` | 600 frames in the world log no Lua errors. |
| `save-load` | Quit saves the game, and `--load-game` of that save reaches `IngameState` with a player. A failed save still tries the load and reports both. |

New games are created as `pzctl-<scenario>` under `<cache-dir>/Saves/Sandbox`;
each run deletes the previous save of that name first.

## Things to know

- New games spawn at a random point of the map, so two runs differ. Pass
  `--spawn X,Y,Z` (and `--seed`) after `start` for a fixed one.
- The `PlayerDB save host failure: ... bounded scratch component writer`
  on quit is intermittent: with the same spawn point and frame count it
  sometimes saves and sometimes fails. `save-load` therefore passes on some
  runs and fails on others.
- In the world the log fills with `attempted index: isAnyVisible of
  non-table: null` (126 in 600 frames with the cursor left at (0, 0)): the
  hotbar, equipment and inventory UI call `getPlayerContextMenu()`, which
  returns nil, likely because `ISUI/ISContextMenu` and
  `ISUI/PlayerData/ISPlayerData` fail to `require`.
- The main menu ignores clicks until `MainScreen.instance.delay` (wall-clock
  milliseconds) is used up; `menu-solo` polls it through `lua`.
- A new game is in `IngameState` after about 1050 frames (about 25 s on an
  M1 Pro with the GPU on). `--no-gpu` is faster but has no screenshots.
- Headless only: there is no windowed control mode yet.
