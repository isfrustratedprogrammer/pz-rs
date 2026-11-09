//! Agent control: a headless game driven step by step from outside.
//!
//! [`Controller`] owns the same `App` the window and `--headless-frames`
//! use. The caller decides when frames run and injects input between them,
//! so a run is deterministic (every frame advances 33 ms of game time).
//! The JSON protocol lives in `pz-client --control`.

use super::{close_headless, headless_app, App, HostRetirementBoundary, Launch};
use pz_kahlua::Value;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A booted game that runs frames only on request.
pub struct Controller {
    app: App,
    frame: u64,
    // Dropped after `app`: host pins are released after its owners stop.
    _boundary: HostRetirementBoundary,
}

/// What the agent sees after a step.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Frames run since boot.
    pub frame: u64,
    /// `GameWindow.states.current` class name.
    pub state: Option<&'static str>,
    pub paused: bool,
    /// The first engine error; once set, frames no longer run.
    pub error: Option<String>,
    pub screen: (i32, i32),
    pub player: Option<PlayerInfo>,
    /// Positions of all live zombies.
    pub zombies: Vec<[f32; 3]>,
    /// Visible top-level UI elements with their visible children.
    pub ui: Vec<UiNode>,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerInfo {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// The action state machine's current state (`idle`, `walk`, ...).
    pub action_state: String,
}

#[derive(Clone, Debug, Default)]
pub struct UiNode {
    /// The engine class (`zombie.ui.UIElement`, ...).
    pub class: String,
    /// The Lua table's `Type` (`ISCollapsableWindow`, `MainScreen`, ...).
    pub lua_type: Option<String>,
    /// The table's `name` or `title` (a label's text, a window title).
    pub text: Option<String>,
    /// Absolute position and size.
    pub rect: [f64; 4],
    pub children: Vec<UiNode>,
}

/// How deep [`Snapshot::ui`] walks below a top-level element.
const UI_DEPTH: usize = 3;

impl Controller {
    /// Boot the game without a window. `gpu` renders frames into an
    /// offscreen target (needed for screenshots).
    pub fn start(launch: Launch, gpu: bool) -> crate::failure::EngineResult<Self> {
        let boundary = HostRetirementBoundary;
        let mut app = headless_app(launch)?;
        if gpu {
            let (w, h) = app.screen_size();
            app.gpu = Some(pz_gpu::Gpu::new_headless(w as u32, h as u32).map_err(|e| e.to_string())?);
        }
        app.boot()?;
        super::run_headless_lua(&mut app)?;
        Ok(Controller { app, frame: 0, _boundary: boundary })
    }

    /// Run up to `n` frames; stops early at the first engine error.
    /// Returns the number of frames run.
    pub fn frames(&mut self, n: u32) -> u32 {
        let mut ran = 0;
        for _ in 0..n {
            if self.app.error.is_some() {
                break;
            }
            self.app.last_frame = Instant::now() - Duration::from_millis(33);
            self.app.frame();
            self.frame += 1;
            ran += 1;
        }
        ran
    }

    /// A key press (`down`) or release by LWJGL code.
    pub fn key(&mut self, code: i32, down: bool) {
        pz_input::with_input(|i| i.key(code, i32::from(down)));
    }

    /// A typed character (text fields).
    pub fn char(&mut self, c: char) {
        pz_input::with_input(|i| i.char(c));
    }

    /// The cursor position in window pixels, y from the top.
    pub fn mouse_move(&mut self, x: f64, y: f64) {
        pz_input::with_input(|i| i.cursor_moved(x, y));
    }

    /// Mouse button 0 = left, 1 = right, 2 = middle.
    pub fn mouse_button(&mut self, button: usize, down: bool) {
        pz_input::with_input(|i| i.mouse_button(button, down));
    }

    /// The window resize path: the GPU target and `Core.setScreenSize`
    /// (which queues `OnResolutionChange`).
    pub fn resize(&mut self, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        if let Some(g) = self.app.gpu.as_mut() {
            g.resize(w, h);
        }
        self.app.core.borrow_mut().set_screen_size(w as i32, h as i32);
    }

    /// The last rendered frame as RGBA (width, height, pixels); `None`
    /// without a GPU.
    pub fn pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
        let gpu = self.app.gpu.as_ref()?;
        let (w, h) = self.app.screen_size();
        Some((w as u32, h as u32, gpu.read_pixels()))
    }

    /// Run a Lua chunk in the game's global environment and return its
    /// results through `tostring`.
    pub fn eval(&mut self, source: &str) -> Result<Vec<String>, String> {
        let _deferral = pz_kahlua::gc::defer_collection();
        let lua = self.app.lua.as_mut().ok_or("Lua is not booted")?;
        let f = pz_kahlua::stdlib::load_string(source, Some("pz-ctl"), lua.thread.globals().clone())
            .map_err(|e| format!("compile: {e:?}"))?;
        let values = lua.thread.pcall(&f, &[]).map_err(|(e, trace)| {
            let msg = e.as_str().map(str::to_string).unwrap_or_else(|| format!("{e:?}"));
            format!("{msg}\n{trace}")
        })?;
        Ok(values
            .iter()
            .map(|v| lua.thread.tostring(v).unwrap_or_else(|e| format!("<tostring: {e:?}>")))
            .collect())
    }

    pub fn error(&self) -> Option<String> {
        self.app.error.as_ref().map(ToString::to_string)
    }

    pub fn snapshot(&self) -> Snapshot {
        let player = pz_lua_api::bind_players::player(0).and_then(|p| {
            p.try_borrow().ok().map(|c| PlayerInfo {
                x: c.vars.x,
                y: c.vars.y,
                z: c.vars.z,
                action_state: c.state.action_context.current_state_name(),
            })
        });
        let zombies = self
            .app
            .world
            .as_ref()
            .map(|w| w.zombies.iter().map(|z| [z.vars.x, z.vars.y, z.vars.z]).collect())
            .unwrap_or_default();
        let ui = pz_ui::with_ui(|m| m.ui.clone())
            .iter()
            .filter(|e| e.is_visible())
            .map(|e| ui_node(e, UI_DEPTH))
            .collect();
        Snapshot {
            frame: self.frame,
            state: self.app.current_state(),
            paused: pz_lua_api::bind_climate::is_game_paused(),
            error: self.error(),
            screen: self.app.screen_size(),
            player,
            zombies,
            ui,
        }
    }

    /// Close the game as the window close does (saves a game in
    /// progress); returns the error that close recorded.
    pub fn quit(mut self) -> crate::failure::EngineResult<()> {
        if let Some(error) = self.app.error.take() {
            // Still tear the session down before reporting.
            self.app.close();
            return Err(error);
        }
        close_headless(&mut self.app)
    }
}

fn table_str(t: &pz_kahlua::TableRef, key: &str) -> Option<String> {
    t.rawget(&Value::str(key)).as_str().map(str::to_string)
}

fn ui_node(e: &Rc<pz_ui::UIElement>, depth: usize) -> UiNode {
    let table = e.get_table();
    let lua_type = table.as_ref().and_then(|t| table_str(t, "Type"));
    let text = table
        .as_ref()
        .and_then(|t| table_str(t, "name").or_else(|| table_str(t, "title")));
    let children = if depth == 0 {
        Vec::new()
    } else {
        e.get_controls()
            .iter()
            .filter(|c| c.is_visible())
            .map(|c| ui_node(c, depth - 1))
            .collect()
    };
    UiNode {
        class: pz_kahlua::LuaObject::class_name(&**e).to_string(),
        lua_type,
        text,
        rect: [e.get_absolute_x(), e.get_absolute_y(), e.get_width(), e.get_height()],
        children,
    }
}
