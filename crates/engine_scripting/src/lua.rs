use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path};
use std::rc::Rc;

use mlua::chunk::ChunkMode;
use mlua::{
    FromLua, Function, HookTriggers, Lua, LuaOptions, MultiValue, RegistryKey, StdLib, Table,
    Value, VmState,
};
use serde::{Deserialize, Serialize};

use crate::GameCommand;
use crate::input::ScriptKey;
use crate::input::{ButtonState, InputSnapshot};

mod camera_api;
mod input_api;
mod object_api;
mod physics_api;
mod state_api;
mod transform_api;
mod vec3;

const MAX_MEMORY: usize = 64 * 1024 * 1024;
const MAX_COMMANDS: usize = 1_024;
const MAX_LOG_BYTES: usize = 64 * 1024;
const HOOK_INTERVAL: u32 = 1_000;
const MAX_INSTRUCTIONS: u32 = 1_000_000;
const MAX_SAVE_BYTES: usize = 64 * 1024;
const MAX_SAVE_ENTRIES: usize = 128;
/// Fixed step used for `engine.add_force` until the host sets one.
const DEFAULT_FIXED_STEP: f32 = 1.0 / 60.0;

/// Framing for one JSON diagnostic line sent by the player to Studio.
pub const LUA_DIAGNOSTIC_PREFIX: &str = "VGE_LUA_DIAGNOSTIC:";

/// Source location and summary of a Lua failure; Output retains the traceback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuaScriptDiagnostic {
    /// Project-relative script path.
    pub path: String,
    /// 1-based source line.
    pub line: u32,
    /// One-line explanation.
    pub message: String,
}

/// A Lua parse error suitable for a source marker before Play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaSyntaxDiagnostic {
    /// 1-based source line.
    pub line: u32,
    /// Lua's explanation of the parse failure.
    pub message: String,
}

/// Compiles Lua source without executing top-level code or callbacks.
pub fn check_lua_syntax(path: &str, source: &str) -> Result<(), LuaSyntaxDiagnostic> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).map_err(|error| {
        LuaSyntaxDiagnostic {
            line: 1,
            message: error.to_string(),
        }
    })?;
    lua.set_memory_limit(MAX_MEMORY)
        .map_err(|error| LuaSyntaxDiagnostic {
            line: 1,
            message: error.to_string(),
        })?;
    lua.load(source)
        .set_name(path)
        .set_mode(ChunkMode::Text)
        .into_function()
        .map(|_| ())
        .map_err(|error| {
            let message = match error {
                mlua::Error::SyntaxError { message, .. } => message,
                other => other.to_string(),
            };
            let line = message
                .split(':')
                .find_map(|part| part.parse::<u32>().ok())
                .unwrap_or(1);
            LuaSyntaxDiagnostic { line, message }
        })
}

/// A Lua project script failed validation, loading, or execution.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    /// The script must be a project asset with a `.lua` extension.
    #[error("script path must be under assets/scripts and end in .lua: {0}")]
    InvalidPath(String),
    /// Lua returned an error, including syntax and callback stack traces.
    #[error("{path}: {source}")]
    Lua {
        /// Project-relative script path.
        path: String,
        /// Lua error and traceback.
        #[source]
        source: mlua::Error,
    },
    /// The Lua VM could not be initialized.
    #[error("could not initialize Lua: {0}")]
    Initialize(#[source] mlua::Error),
    /// Saved Lua gameplay values were malformed or exceeded the limit.
    #[error("invalid Lua save data: {0}")]
    SaveData(String),
    /// The scoped `engine.raycast` / `engine.is_grounded` functions could not be installed or removed.
    #[error("could not set up physics queries for a Lua callback: {0}")]
    Queries(#[source] mlua::Error),
}

impl ScriptError {
    /// Extracts a source location from a Lua syntax or callback error.
    pub fn diagnostic(&self) -> Option<LuaScriptDiagnostic> {
        let Self::Lua { path, source } = self else {
            return None;
        };
        let message = source.to_string();
        let named_chunk = format!("[string \"{path}\"]:");
        let line = message
            .lines()
            .filter_map(|line| line.split_once(&named_chunk).map(|(_, tail)| tail))
            .filter_map(|tail| {
                tail.split(':')
                    .next()
                    .and_then(|part| part.parse::<u32>().ok())
            })
            .next()
            .unwrap_or(1);
        Some(LuaScriptDiagnostic {
            path: path.clone(),
            line,
            message: message
                .lines()
                .next()
                .unwrap_or("Lua script error")
                .to_owned(),
        })
    }
}

struct LoadedScript {
    path: String,
    table: RegistryKey,
    object_id: Option<String>,
    /// A `start` was requested while the object was disabled; it runs the
    /// first time the object is seen enabled, before any other callback.
    awaiting_start: bool,
}

/// Which scripts one callback goes to.
#[derive(Clone, Copy)]
enum Only<'a> {
    All,
    Object(&'a str),
    Except(&'a HashSet<String>),
}

impl Only<'_> {
    fn includes(self, object_id: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Object(only) => object_id == Some(only),
            Self::Except(skipped) => object_id.is_none_or(|id| !skipped.contains(id)),
        }
    }
}

#[derive(Default)]
struct SceneSnapshot {
    positions: HashMap<String, [f32; 3]>,
    rotations: HashMap<String, [f32; 4]>,
    scales: HashMap<String, [f32; 3]>,
    velocities: HashMap<String, [f32; 3]>,
    names: HashMap<String, String>,
    /// Object IDs by tag, in snapshot order.
    tags: HashMap<String, Vec<String>>,
    variables: BTreeMap<String, f64>,
    /// Current text of UI elements by object ID.
    ui_texts: HashMap<String, String>,
    /// Every object by ID with its `Hidden` state inverted.
    visible: HashMap<String, bool>,
    /// Every object by ID with its `Disabled` state inverted.
    enabled: HashMap<String, bool>,
    pending_visible: HashMap<String, bool>,
    pending_enabled: HashMap<String, bool>,
    /// Objects this callback destroyed.
    pending_destroyed: HashSet<String>,
    pending_positions: HashMap<String, [f32; 3]>,
    pending_rotations: HashMap<String, [f32; 4]>,
    pending_scales: HashMap<String, [f32; 3]>,
    pending_velocities: HashMap<String, [f32; 3]>,
    pending_names: HashMap<String, String>,
    pending_variables: BTreeMap<String, f64>,
    /// The camera object the game views through.
    active_camera: Option<String>,
    /// This callback's own `set_camera`.
    pending_camera: Option<String>,
    /// The game state name when the callback began (`playing` if never set).
    game_state: Option<String>,
    /// This callback's own `set_state`.
    pending_state: Option<String>,
}

impl SceneSnapshot {
    fn clear_pending(&mut self) {
        self.pending_positions.clear();
        self.pending_rotations.clear();
        self.pending_scales.clear();
        self.pending_velocities.clear();
        self.pending_names.clear();
        self.pending_variables.clear();
        self.pending_visible.clear();
        self.pending_enabled.clear();
        self.pending_destroyed.clear();
        self.pending_camera = None;
        self.pending_state = None;
    }

    /// The game state, this callback's own `set_state` first.
    fn state(&self) -> String {
        self.pending_state
            .clone()
            .or_else(|| self.game_state.clone())
            .unwrap_or_else(|| "playing".to_owned())
    }

    /// Whether the id is in the dispatch-start snapshot or spawned this callback.
    fn knows(&self, object_id: &str) -> bool {
        self.visible.contains_key(object_id)
            || self.enabled.contains_key(object_id)
            || self.pending_positions.contains_key(object_id)
    }

    fn destroyed(&self, object_id: &str) -> bool {
        self.pending_destroyed.contains(object_id)
    }

    /// Visibility, this callback's own writes first; unknown or destroyed is `false`.
    fn is_visible(&self, object_id: &str) -> bool {
        !self.destroyed(object_id)
            && self.knows(object_id)
            && self
                .pending_visible
                .get(object_id)
                .or_else(|| self.visible.get(object_id))
                .copied()
                .unwrap_or(false)
    }

    /// Enabled state, this callback's own writes first; unknown or destroyed is `false`.
    fn is_enabled(&self, object_id: &str) -> bool {
        !self.destroyed(object_id)
            && self.knows(object_id)
            && self
                .pending_enabled
                .get(object_id)
                .or_else(|| self.enabled.get(object_id))
                .copied()
                .unwrap_or(false)
    }

    /// True when the dispatch-start snapshot marks the object disabled.
    fn disabled_at_dispatch_start(&self, object_id: &str) -> bool {
        self.enabled.get(object_id) == Some(&false)
    }

    /// The active camera, this callback's own `set_camera` first.
    fn camera(&self) -> Option<String> {
        self.pending_camera
            .clone()
            .or_else(|| self.active_camera.clone())
    }

    fn variable(&self, name: &str) -> f64 {
        self.pending_variables
            .get(name)
            .or_else(|| self.variables.get(name))
            .copied()
            .unwrap_or(0.0)
    }

    fn position(&self, object_id: &str) -> Option<[f32; 3]> {
        if self.destroyed(object_id) {
            return None;
        }
        self.pending_positions
            .get(object_id)
            .or_else(|| self.positions.get(object_id))
            .copied()
    }

    fn rotation(&self, object_id: &str) -> Option<glam::Quat> {
        if self.destroyed(object_id) {
            return None;
        }
        self.pending_rotations
            .get(object_id)
            .or_else(|| self.rotations.get(object_id))
            .map(|r| glam::Quat::from_array(*r))
    }

    /// Velocity of a dynamic body, pending writes first.
    fn velocity(&self, object_id: &str) -> Option<[f32; 3]> {
        if self.destroyed(object_id) {
            return None;
        }
        self.pending_velocities
            .get(object_id)
            .or_else(|| self.velocities.get(object_id))
            .copied()
    }

    fn scale(&self, object_id: &str) -> Option<glam::Vec3> {
        if self.destroyed(object_id) {
            return None;
        }
        self.pending_scales
            .get(object_id)
            .or_else(|| self.scales.get(object_id))
            .map(|s| glam::Vec3::from_array(*s))
    }

    /// Records a freshly spawned object's transform (identity rotation, unit scale).
    fn insert_pending_spawn(&mut self, object_id: &str, position: [f32; 3]) {
        self.pending_positions
            .insert(object_id.to_owned(), position);
        self.pending_rotations
            .insert(object_id.to_owned(), glam::Quat::IDENTITY.to_array());
        self.pending_scales
            .insert(object_id.to_owned(), glam::Vec3::ONE.to_array());
    }

    /// The objects carrying `tag`, without any this callback destroyed.
    fn tagged_ids(&self, tag: &str) -> Vec<String> {
        self.tags.get(tag).map_or_else(Vec::new, |ids| {
            ids.iter()
                .filter(|id| !self.destroyed(id))
                .cloned()
                .collect()
        })
    }

    fn named_id(&self, name: &str) -> Option<String> {
        self.names
            .get(name)
            .or_else(|| self.pending_names.get(name))
            .filter(|id| !self.destroyed(id))
            .cloned()
    }
}

/// One scene object as Lua sees it. Rotation is a quaternion in xyzw order.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotObject {
    /// Stable scene object ID.
    pub id: String,
    /// Display name, if the object has one.
    pub name: Option<String>,
    /// Local translation; `None` for objects without a transform.
    pub position: Option<[f32; 3]>,
    /// Local rotation quaternion (xyzw).
    pub rotation: [f32; 4],
    /// Local scale.
    pub scale: [f32; 3],
    /// Linear velocity; `Some` only for dynamic physics bodies.
    pub velocity: Option<[f32; 3]>,
    /// False when the object has the `Hidden` marker.
    pub visible: bool,
    /// False when the object has the `Disabled` marker.
    pub enabled: bool,
    /// The object's tags.
    pub tags: Vec<String>,
}

/// `engine.load_scene`, `engine.unload_scene` and `engine.next_scene`.
fn scene_commands_api(
    lua: &Lua,
    engine: &Table,
    commands: &Rc<RefCell<Vec<GameCommand>>>,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    type Make = fn(String) -> GameCommand;
    let paths: [(&str, Make); 2] = [
        ("load_scene", |path| GameCommand::LoadScene { path }),
        ("unload_scene", |path| GameCommand::UnloadScene { path }),
    ];
    let ready = |active: &Cell<bool>, pending: &[GameCommand]| {
        if !active.get() {
            return Err(mlua::Error::RuntimeError(
                "engine commands may only run in callbacks".to_owned(),
            ));
        }
        if pending.len() >= MAX_COMMANDS {
            return Err(mlua::Error::RuntimeError(
                "Lua command limit exceeded".to_owned(),
            ));
        }
        Ok(())
    };
    for (name, make) in paths {
        let (queue, on) = (Rc::clone(commands), Rc::clone(active));
        engine.set(
            name,
            lua.create_function(move |_, path: String| {
                let mut pending = queue.borrow_mut();
                ready(&on, &pending)?;
                if !valid_scene_path(Path::new(&path)) {
                    return Err(mlua::Error::RuntimeError(
                        "scene path must be under scenes/ and end in .ron".to_owned(),
                    ));
                }
                pending.push(make(path));
                Ok(())
            })?,
        )?;
    }
    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "next_scene",
        lua.create_function(move |_, ()| {
            let mut pending = queue.borrow_mut();
            ready(&on, &pending)?;
            pending.push(GameCommand::NextScene);
            Ok(())
        })?,
    )
}

/// Consumes the leading string argument of a variadic call.
fn string_arg(lua: &Lua, args: &mut MultiValue, what: &str) -> mlua::Result<String> {
    match args.pop_front() {
        Some(value @ (Value::String(_) | Value::Integer(_) | Value::Number(_))) => {
            String::from_lua(value, lua)
        }
        _ => Err(mlua::Error::RuntimeError(format!(
            "{what} must be a string"
        ))),
    }
}

#[derive(Clone, Copy)]
enum CallbackArgs<'a> {
    None,
    Delta(f32),
    Event(&'a str, &'a str),
    StateChange(&'a str, &'a str),
    Collision(&'a str, &'a str, &'a str),
}

/// One bounded Lua VM for one game; scripts are tables of lifecycle callbacks.
pub struct LuaRuntime {
    lua: Lua,
    scripts: Vec<LoadedScript>,
    commands: Rc<RefCell<Vec<GameCommand>>>,
    active_object: Rc<RefCell<Option<String>>>,
    input: Rc<RefCell<InputSnapshot>>,
    scene: Rc<RefCell<SceneSnapshot>>,
    active_callback: Rc<Cell<bool>>,
    instructions: Rc<Cell<u32>>,
    save_values: Rc<RefCell<BTreeMap<String, serde_json::Value>>>,
    save_writes: Rc<Cell<usize>>,
    query_stubs: physics_api::QueryStubs,
    /// Seconds an `engine.add_force` call pushes for: the running callback's
    /// `dt` in `update`/`fixed_update`, the fixed step elsewhere.
    force_seconds: Rc<Cell<f32>>,
    fixed_step: f32,
}

fn valid_save_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => value.len() <= 16 * 1024,
        _ => false,
    }
}

impl LuaRuntime {
    /// Creates a Lua VM with only computation libraries and engine-owned APIs.
    pub fn new() -> Result<Self, ScriptError> {
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
        let lua = Lua::new_with(libs, LuaOptions::default()).map_err(ScriptError::Initialize)?;
        lua.set_memory_limit(MAX_MEMORY)
            .map_err(ScriptError::Initialize)?;
        vec3::install(&lua).map_err(ScriptError::Initialize)?;
        let globals = lua.globals();
        for name in ["dofile", "loadfile", "load", "require", "collectgarbage"] {
            globals
                .set(name, mlua::Value::Nil)
                .map_err(ScriptError::Initialize)?;
        }
        let commands = Rc::new(RefCell::new(Vec::new()));
        let log_commands = Rc::clone(&commands);
        let active_callback = Rc::new(Cell::new(false));
        let log_active = Rc::clone(&active_callback);
        let engine = lua.create_table().map_err(ScriptError::Initialize)?;
        engine
            .set(
                "log",
                lua.create_function(move |_, message: String| {
                    if !log_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    let mut pending = log_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS || message.len() > MAX_LOG_BYTES {
                        return Err(mlua::Error::RuntimeError(
                            "Lua log limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::Log(message));
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let save_values = Rc::new(RefCell::new(BTreeMap::<String, serde_json::Value>::new()));
        let values_for_get = Rc::clone(&save_values);
        let get_active = Rc::clone(&active_callback);
        engine
            .set(
                "save_get",
                lua.create_function(move |lua, key: String| {
                    if !get_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine save data is only available in callbacks".to_owned(),
                        ));
                    }
                    Ok(match values_for_get.borrow().get(&key) {
                        Some(serde_json::Value::Bool(value)) => mlua::Value::Boolean(*value),
                        Some(serde_json::Value::Number(value)) => {
                            if let Some(integer) = value.as_i64() {
                                mlua::Value::Integer(integer)
                            } else {
                                mlua::Value::Number(value.as_f64().ok_or_else(|| {
                                    mlua::Error::RuntimeError(
                                        "saved number cannot be represented in Lua".to_owned(),
                                    )
                                })?)
                            }
                        }
                        Some(serde_json::Value::String(value)) => {
                            mlua::Value::String(lua.create_string(value)?)
                        }
                        _ => mlua::Value::Nil,
                    })
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let values_for_set = Rc::clone(&save_values);
        let save_writes = Rc::new(Cell::new(0));
        let writes_for_set = Rc::clone(&save_writes);
        let set_active = Rc::clone(&active_callback);
        engine
            .set(
                "save_set",
                lua.create_function(move |_, (key, value): (String, mlua::Value)| {
                    if !set_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if key.is_empty() || key.len() > 128 {
                        return Err(mlua::Error::RuntimeError(
                            "save key must be 1–128 bytes".to_owned(),
                        ));
                    }
                    let value = match value {
                        mlua::Value::Nil => None,
                        mlua::Value::Boolean(value) => Some(serde_json::Value::Bool(value)),
                        mlua::Value::Integer(value) => Some(serde_json::Value::from(value)),
                        mlua::Value::Number(value) if value.is_finite() => {
                            Some(serde_json::Value::from(value))
                        }
                        mlua::Value::String(value) => {
                            Some(serde_json::Value::String(value.to_str()?.to_owned()))
                        }
                        _ => {
                            return Err(mlua::Error::RuntimeError(
                                "save values must be a string, number, boolean, or nil".to_owned(),
                            ));
                        }
                    };
                    if value.as_ref().is_some_and(|value| !valid_save_value(value)) {
                        return Err(mlua::Error::RuntimeError(
                            "save value exceeds 16 KiB".to_owned(),
                        ));
                    }
                    if writes_for_set.get() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua save write limit exceeded".to_owned(),
                        ));
                    }
                    writes_for_set.set(writes_for_set.get() + 1);
                    let mut next = values_for_set.borrow().clone();
                    if let Some(value) = value {
                        next.insert(key, value);
                    } else {
                        next.remove(&key);
                    }
                    if next.len() > MAX_SAVE_ENTRIES
                        || serde_json::to_vec(&next)
                            .map_err(mlua::Error::external)?
                            .len()
                            > MAX_SAVE_BYTES
                    {
                        return Err(mlua::Error::RuntimeError(
                            "Lua save data limit exceeded".to_owned(),
                        ));
                    }
                    *values_for_set.borrow_mut() = next;
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let save_commands = Rc::clone(&commands);
        let save_active = Rc::clone(&active_callback);
        engine
            .set(
                "save",
                lua.create_function(move |_, ()| {
                    if !save_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    let mut pending = save_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::RequestSave);
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let active_object = Rc::new(RefCell::new(None));
        let object_for_lua = Rc::clone(&active_object);
        engine
            .set(
                "object_id",
                lua.create_function(move |_, ()| Ok(object_for_lua.borrow().clone()))
                    .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let audio_commands = Rc::clone(&commands);
        let audio_active = Rc::clone(&active_callback);
        engine
            .set(
                "play_audio",
                lua.create_function(move |_, object_id: String| {
                    if !audio_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if uuid::Uuid::parse_str(&object_id).is_err() {
                        return Err(mlua::Error::RuntimeError(
                            "audio target must be a scene object ID".to_owned(),
                        ));
                    }
                    let mut pending = audio_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS
                        || pending
                            .iter()
                            .filter(|command| matches!(command, GameCommand::PlayAudio { .. }))
                            .count()
                            >= 32
                    {
                        return Err(mlua::Error::RuntimeError(
                            "Lua audio play limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::PlayAudio { object_id });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let ui_commands = Rc::clone(&commands);
        let animation_commands = Rc::clone(&commands);
        let animation_active = Rc::clone(&active_callback);
        engine
            .set(
                "set_animation_playing",
                lua.create_function(move |_, (object_id, playing): (String, bool)| {
                    if !animation_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if uuid::Uuid::parse_str(&object_id).is_err() {
                        return Err(mlua::Error::RuntimeError(
                            "animation target must be a scene object ID".to_owned(),
                        ));
                    }
                    let mut pending = animation_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::SetAnimationPlaying { object_id, playing });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let scene = Rc::new(RefCell::new(SceneSnapshot::default()));
        let ui_text_scene = Rc::clone(&scene);
        let ui_active = Rc::clone(&active_callback);
        engine
            .set(
                "ui_set_text",
                lua.create_function(move |_, (object_id, text): (String, String)| {
                    if !ui_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if uuid::Uuid::parse_str(&object_id).is_err() {
                        return Err(mlua::Error::RuntimeError(
                            "UI target must be a scene object ID".to_owned(),
                        ));
                    }
                    if text.len() > 1024 {
                        return Err(mlua::Error::RuntimeError(
                            "UI text exceeds 1024 bytes".to_owned(),
                        ));
                    }
                    let mut pending = ui_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    // Reads back in the same callback, like other setters.
                    if let Some(known) = ui_text_scene.borrow_mut().ui_texts.get_mut(&object_id) {
                        known.clone_from(&text);
                    }
                    pending.push(GameCommand::UiSetText { object_id, text });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let event_commands = Rc::clone(&commands);
        let event_active = Rc::clone(&active_callback);
        engine
            .set(
                "emit_event",
                lua.create_function(move |_, name: String| {
                    if !event_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                        return Err(mlua::Error::RuntimeError(
                            "event name must be 1–64 printable bytes".to_owned(),
                        ));
                    }
                    let mut pending = event_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::EmitEvent { name });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let scene_commands = Rc::clone(&commands);
        let scene_active = Rc::clone(&active_callback);
        engine
            .set(
                "change_scene",
                lua.create_function(move |_, path: String| {
                    if !scene_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    let mut pending = scene_commands.borrow_mut();
                    if pending
                        .iter()
                        .any(|command| matches!(command, GameCommand::ChangeScene { .. }))
                    {
                        return Ok(());
                    }
                    if !valid_scene_path(Path::new(&path)) {
                        return Err(mlua::Error::RuntimeError(
                            "scene path must be under scenes/ and end in .ron".to_owned(),
                        ));
                    }
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    pending.push(GameCommand::ChangeScene { path });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        scene_commands_api(&lua, &engine, &commands, &active_callback)
            .map_err(ScriptError::Initialize)?;
        let spawn_scene = Rc::clone(&scene);
        let spawn_commands = Rc::clone(&commands);
        let spawn_active = Rc::clone(&active_callback);
        engine
            .set(
                "spawn_empty",
                lua.create_function(move |lua, mut args: MultiValue| {
                    if !spawn_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    let name = string_arg(lua, &mut args, "object name")?;
                    if name.trim().is_empty() || name.len() > 256 {
                        return Err(mlua::Error::RuntimeError(
                            "object name must be nonempty and at most 256 bytes".to_owned(),
                        ));
                    }
                    let position = vec3::vector_arg(&mut args, "position")?.to_array();
                    let mut pending = spawn_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    let object_id = uuid::Uuid::new_v4().to_string();
                    pending.push(GameCommand::SpawnEmpty {
                        object_id: object_id.clone(),
                        name: name.clone(),
                        position,
                    });
                    let mut snapshot = spawn_scene.borrow_mut();
                    snapshot
                        .pending_names
                        .entry(name)
                        .or_insert_with(|| object_id.clone());
                    snapshot.insert_pending_spawn(&object_id, position);
                    Ok(object_id)
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let prefab_scene = Rc::clone(&scene);
        let prefab_commands = Rc::clone(&commands);
        let prefab_active = Rc::clone(&active_callback);
        engine
            .set(
                "spawn_prefab",
                lua.create_function(move |lua, mut args: MultiValue| {
                    if !prefab_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    let path = string_arg(lua, &mut args, "prefab path")?;
                    if !valid_prefab_path(Path::new(&path)) {
                        return Err(mlua::Error::RuntimeError(
                            "prefab path must be under assets/prefabs and end in .prefab"
                                .to_owned(),
                        ));
                    }
                    let position = vec3::vector_arg(&mut args, "position")?.to_array();
                    let mut pending = prefab_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    let object_id = uuid::Uuid::new_v4().to_string();
                    pending.push(GameCommand::SpawnPrefab {
                        object_id: object_id.clone(),
                        path,
                        position,
                    });
                    prefab_scene
                        .borrow_mut()
                        .insert_pending_spawn(&object_id, position);
                    Ok(object_id)
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        for (name, additive) in [("set_position", false), ("translate", true)] {
            let transform_commands = Rc::clone(&commands);
            let transform_scene = Rc::clone(&scene);
            let transform_active = Rc::clone(&active_callback);
            engine
                .set(
                    name,
                    lua.create_function(move |lua, mut args: MultiValue| {
                        if !transform_active.get() {
                            return Err(mlua::Error::RuntimeError(
                                "engine commands may only run in callbacks".to_owned(),
                            ));
                        }
                        let object_id = string_arg(lua, &mut args, "object ID")?;
                        if object_id.is_empty() || object_id.len() > 64 {
                            return Err(mlua::Error::RuntimeError(
                                "object ID must be nonempty and at most 64 bytes".to_owned(),
                            ));
                        }
                        let [x, y, z] = vec3::vector_arg(&mut args, "position")?.to_array();
                        let mut pending = transform_commands.borrow_mut();
                        if pending.len() >= MAX_COMMANDS {
                            return Err(mlua::Error::RuntimeError(
                                "Lua command limit exceeded".to_owned(),
                            ));
                        }
                        let mut snapshot = transform_scene.borrow_mut();
                        if let Some(position) = snapshot.position(&object_id) {
                            let next = if additive {
                                [position[0] + x, position[1] + y, position[2] + z]
                            } else {
                                [x, y, z]
                            };
                            if next.iter().all(|value| value.is_finite()) {
                                snapshot.pending_positions.insert(object_id.clone(), next);
                            }
                        }
                        pending.push(if additive {
                            GameCommand::Translate {
                                object_id,
                                delta: [x, y, z],
                            }
                        } else {
                            GameCommand::SetPosition {
                                object_id,
                                position: [x, y, z],
                            }
                        });
                        Ok(())
                    })
                    .map_err(ScriptError::Initialize)?,
                )
                .map_err(ScriptError::Initialize)?;
        }
        transform_api::register(&lua, &engine, &commands, &scene, &active_callback)
            .map_err(ScriptError::Initialize)?;
        let input = Rc::new(RefCell::new(InputSnapshot::default()));
        let force_seconds = Rc::new(Cell::new(DEFAULT_FIXED_STEP));
        let query_stubs = physics_api::register(
            &lua,
            &engine,
            &commands,
            &scene,
            &active_callback,
            &force_seconds,
        )
        .map_err(ScriptError::Initialize)?;
        camera_api::register(&lua, &engine, &commands, &scene, &active_callback)
            .map_err(ScriptError::Initialize)?;
        state_api::register(&lua, &engine, &commands, &scene, &active_callback)
            .map_err(ScriptError::Initialize)?;
        object_api::register(&lua, &engine, &commands, &scene, &active_callback)
            .map_err(ScriptError::Initialize)?;
        input_api::register(&lua, &engine, &commands, &input, &active_callback)
            .map_err(ScriptError::Initialize)?;
        let named_scene = Rc::clone(&scene);
        engine
            .set(
                "find_object",
                lua.create_function(move |_, name: String| {
                    Ok(named_scene.borrow().named_id(&name))
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let tagged_scene = Rc::clone(&scene);
        engine
            .set(
                "find_objects_by_tag",
                lua.create_function(move |_, tag: String| {
                    Ok(tagged_scene.borrow().tagged_ids(&tag))
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let positioned_scene = Rc::clone(&scene);
        engine
            .set(
                "get_position",
                lua.create_function(move |lua, object_id: String| {
                    positioned_scene
                        .borrow()
                        .position(&object_id)
                        .map(|[x, y, z]| vec3::to_lua(lua, glam::Vec3::new(x, y, z)))
                        .transpose()
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let variable_commands = Rc::clone(&commands);
        let variable_active = Rc::clone(&active_callback);
        let variable_scene = Rc::clone(&scene);
        engine
            .set(
                "set_var",
                lua.create_function(move |_, (name, value): (String, f64)| {
                    if !variable_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                        return Err(mlua::Error::RuntimeError(
                            "variable name must be 1–64 printable bytes".to_owned(),
                        ));
                    }
                    if !value.is_finite() {
                        return Err(mlua::Error::RuntimeError(
                            "variable value must be finite".to_owned(),
                        ));
                    }
                    let mut pending = variable_commands.borrow_mut();
                    if pending.len() >= MAX_COMMANDS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua command limit exceeded".to_owned(),
                        ));
                    }
                    variable_scene
                        .borrow_mut()
                        .pending_variables
                        .insert(name.clone(), value);
                    pending.push(GameCommand::SetVariable { name, value });
                    Ok(())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let read_ui_texts = Rc::clone(&scene);
        engine
            .set(
                "ui_get_text",
                lua.create_function(move |_, object_id: String| {
                    Ok(read_ui_texts.borrow().ui_texts.get(&object_id).cloned())
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let read_variables = Rc::clone(&scene);
        engine
            .set(
                "get_var",
                lua.create_function(move |_, name: String| {
                    // Whole numbers become Lua integers so `tostring` prints `3`, not `3.0`.
                    let value = read_variables.borrow().variable(&name);
                    Ok(
                        if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
                            mlua::Value::Integer(value as i64)
                        } else {
                            mlua::Value::Number(value)
                        },
                    )
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        globals
            .set("engine", engine)
            .map_err(ScriptError::Initialize)?;

        let instructions = Rc::new(Cell::new(0_u32));
        let hook_instructions = Rc::clone(&instructions);
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
            move |_, _| {
                let count = hook_instructions.get().saturating_add(HOOK_INTERVAL);
                hook_instructions.set(count);
                if count > MAX_INSTRUCTIONS {
                    return Err(mlua::Error::RuntimeError(
                        "Lua instruction limit exceeded".to_owned(),
                    ));
                }
                Ok(VmState::Continue)
            },
        )
        .map_err(ScriptError::Initialize)?;
        // `pcall` could otherwise swallow the hook's error and loop forever.
        // The counter stays exhausted until the next callback resets it, so
        // every enclosing protected call re-raises and the callback ends.
        for name in ["pcall", "xpcall"] {
            let protected: Function = globals.get(name).map_err(ScriptError::Initialize)?;
            let budget = Rc::clone(&instructions);
            let guarded = lua
                .create_function(move |_, args: mlua::MultiValue| {
                    let results: mlua::MultiValue = protected.call(args)?;
                    if budget.get() > MAX_INSTRUCTIONS {
                        return Err(mlua::Error::RuntimeError(
                            "Lua instruction limit exceeded".to_owned(),
                        ));
                    }
                    Ok(results)
                })
                .map_err(ScriptError::Initialize)?;
            globals
                .set(name, guarded)
                .map_err(ScriptError::Initialize)?;
        }
        Ok(Self {
            lua,
            scripts: Vec::new(),
            commands,
            active_object,
            input,
            scene,
            active_callback,
            instructions,
            save_values,
            save_writes,
            query_stubs,
            force_seconds,
            fixed_step: DEFAULT_FIXED_STEP,
        })
    }

    /// Runs `f` with `engine.raycast` and `engine.is_grounded` answering from
    /// `queries`. Outside this call they return nil and false. A script that
    /// stores either function and calls it later gets a Lua error.
    ///
    /// Fails only if the scoped functions cannot be installed or removed.
    pub fn with_queries<R>(
        &mut self,
        queries: &dyn crate::ScriptQueries,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Result<R, ScriptError> {
        let lua = self.lua.clone();
        physics_api::with_queries(&lua, &self.query_stubs.clone(), queries, || f(self))
            .map_err(ScriptError::Queries)
    }

    /// Sets the fixed physics step (seconds). `engine.add_force` pushes for
    /// this long in callbacks other than `update` and `fixed_update`.
    /// Non-finite or non-positive values are ignored.
    pub fn set_fixed_step(&mut self, seconds: f32) {
        if seconds.is_finite() && seconds > 0.0 {
            self.fixed_step = seconds;
        }
    }

    /// Serializes bounded game values for the engine's atomic save file.
    pub fn saved_data(&self) -> Result<String, ScriptError> {
        serde_json::to_string(&*self.save_values.borrow())
            .map_err(|error| ScriptError::SaveData(error.to_string()))
    }

    /// Restores values before `start`; malformed or oversized saves leave current values intact.
    pub fn restore_saved_data(&mut self, data: &str) -> Result<(), ScriptError> {
        if data.len() > MAX_SAVE_BYTES {
            return Err(ScriptError::SaveData("save exceeds 64 KiB".into()));
        }
        let next: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(data).map_err(|error| ScriptError::SaveData(error.to_string()))?;
        if next.len() > MAX_SAVE_ENTRIES
            || next
                .iter()
                .any(|(key, value)| key.is_empty() || key.len() > 128 || !valid_save_value(value))
        {
            return Err(ScriptError::SaveData("invalid save key or value".into()));
        }
        *self.save_values.borrow_mut() = next;
        Ok(())
    }

    /// Parses a bundled project script. It must return a callback table.
    pub fn load_script(&mut self, path: &str, source: &str) -> Result<(), ScriptError> {
        self.load_script_with_object(path, source, None)
    }

    /// Sets this frame's input state for a supported Lua key.
    pub fn set_key_state(&mut self, key: ScriptKey, down: bool, pressed: bool) {
        self.input.borrow_mut().keys.insert(
            key,
            ButtonState {
                down,
                pressed,
                released: false,
            },
        );
    }

    /// Replaces the whole Lua-visible input snapshot for the next callbacks.
    pub fn set_input(&mut self, snapshot: InputSnapshot) {
        *self.input.borrow_mut() = snapshot;
    }

    /// Replaces the Lua-visible scene snapshot before a gameplay callback.
    /// Names resolve to the first matching object in iteration order.
    pub fn set_scene_snapshot(
        &mut self,
        objects: impl IntoIterator<Item = (String, Option<String>, Option<[f32; 3]>)>,
    ) {
        self.set_scene_objects(
            objects
                .into_iter()
                .map(|(id, name, position)| SnapshotObject {
                    id,
                    name,
                    position,
                    rotation: glam::Quat::IDENTITY.to_array(),
                    scale: glam::Vec3::ONE.to_array(),
                    velocity: None,
                    visible: true,
                    enabled: true,
                    tags: Vec::new(),
                }),
        );
    }

    /// Replaces the Lua-visible scene snapshot, including rotation and scale.
    /// Only objects with a finite position get transform entries.
    pub fn set_scene_objects(&mut self, objects: impl IntoIterator<Item = SnapshotObject>) {
        let mut scene = self.scene.borrow_mut();
        scene.positions.clear();
        scene.rotations.clear();
        scene.scales.clear();
        scene.velocities.clear();
        scene.names.clear();
        scene.tags.clear();
        scene.visible.clear();
        scene.enabled.clear();
        scene.clear_pending();
        for object in objects {
            let id = object.id;
            scene.visible.insert(id.clone(), object.visible);
            scene.enabled.insert(id.clone(), object.enabled);
            for tag in object.tags {
                scene.tags.entry(tag).or_default().push(id.clone());
            }
            if let Some(name) = object.name {
                scene.names.entry(name).or_insert_with(|| id.clone());
            }
            let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
            if let Some(position) = object.position.filter(|p| finite(p)) {
                // A non-finite or zero-length rotation falls back to identity.
                let rotation = glam::Quat::from_array(object.rotation);
                let rotation = if finite(&object.rotation) && rotation.length_squared() > 0.0 {
                    rotation.normalize().to_array()
                } else {
                    glam::Quat::IDENTITY.to_array()
                };
                let scale = if finite(&object.scale) {
                    object.scale
                } else {
                    glam::Vec3::ONE.to_array()
                };
                if let Some(velocity) = object.velocity.filter(|v| finite(v)) {
                    scene.velocities.insert(id.clone(), velocity);
                }
                scene.rotations.insert(id.clone(), rotation);
                scene.scales.insert(id.clone(), scale);
                scene.positions.insert(id, position);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn scene_rotation(&self, object_id: &str) -> Option<[f32; 4]> {
        self.scene
            .borrow()
            .rotation(object_id)
            .map(|q| q.to_array())
    }

    #[cfg(test)]
    pub(crate) fn scene_scale(&self, object_id: &str) -> Option<[f32; 3]> {
        self.scene.borrow().scale(object_id).map(|v| v.to_array())
    }

    /// Sets the camera object `engine.get_camera` reports. Call before a
    /// callback.
    pub fn set_active_camera(&mut self, object_id: Option<String>) {
        let mut scene = self.scene.borrow_mut();
        scene.active_camera = object_id;
        scene.pending_camera = None;
    }

    /// Sets the game state `engine.get_state` reports (one of `GAME_STATES`).
    /// Call before a callback.
    pub fn set_game_state(&mut self, name: &str) {
        let mut scene = self.scene.borrow_mut();
        scene.game_state = Some(name.to_owned());
        scene.pending_state = None;
    }

    /// Replaces the gameplay variables Lua reads with `engine.get_var`.
    /// Call before a callback, after visual rules have run.
    pub fn set_variables<'a>(&mut self, variables: impl IntoIterator<Item = (&'a str, f64)>) {
        let mut scene = self.scene.borrow_mut();
        scene.variables = variables
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        scene.pending_variables.clear();
    }

    /// Replaces the UI text Lua reads with `engine.ui_get_text`.
    pub fn set_ui_texts<'a>(&mut self, texts: impl IntoIterator<Item = (&'a str, &'a str)>) {
        self.scene.borrow_mut().ui_texts = texts
            .into_iter()
            .map(|(id, text)| (id.to_owned(), text.to_owned()))
            .collect();
    }

    /// Loads one instance of a script attached to a scene object.
    pub fn load_attached_script(
        &mut self,
        path: &str,
        source: &str,
        object_id: &str,
    ) -> Result<(), ScriptError> {
        self.load_script_with_object(path, source, Some(object_id.to_owned()))
    }

    fn load_script_with_object(
        &mut self,
        path: &str,
        source: &str,
        object_id: Option<String>,
    ) -> Result<(), ScriptError> {
        if !valid_script_path(Path::new(path)) {
            return Err(ScriptError::InvalidPath(path.to_owned()));
        }
        self.instructions.set(0);
        *self.active_object.borrow_mut() = None;
        let table: Table = self
            .lua
            .load(source)
            .set_name(path)
            .set_mode(ChunkMode::Text)
            .eval()
            .map_err(|source| ScriptError::Lua {
                path: path.to_owned(),
                source,
            })?;
        let key = self
            .lua
            .create_registry_value(table)
            .map_err(|source| ScriptError::Lua {
                path: path.to_owned(),
                source,
            })?;
        self.scripts.push(LoadedScript {
            path: path.to_owned(),
            table: key,
            object_id,
            awaiting_start: false,
        });
        Ok(())
    }

    /// Drops the scripts attached to the given (destroyed) objects, so they
    /// get no more callbacks and stop using the instruction budget.
    pub fn forget_objects(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        self.scripts.retain(|script| {
            script
                .object_id
                .as_ref()
                .is_none_or(|id| !ids.iter().any(|gone| gone == id))
        });
    }

    /// Invokes each script's `start` callback in load order. A script on a
    /// disabled object starts later, the first time it is seen enabled.
    pub fn start(&mut self) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("start", CallbackArgs::None)
    }

    /// Invokes `start` only for scripts attached to `object_id`, as when a
    /// scripted prefab is spawned during play. Other scripts are not called.
    pub fn start_object(&mut self, object_id: &str) -> Result<Vec<GameCommand>, ScriptError> {
        self.call_scripts("start", CallbackArgs::None, Only::Object(object_id))
    }

    /// Invokes `start` for every script except those attached to `object_ids`,
    /// which already started in an earlier scene.
    pub fn start_except(
        &mut self,
        object_ids: &HashSet<String>,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.call_scripts("start", CallbackArgs::None, Only::Except(object_ids))
    }

    /// How many scripts are loaded. Scripts loaded after this count can be
    /// taken back with [`Self::discard_scripts_from`].
    pub fn script_count(&self) -> usize {
        self.scripts.len()
    }

    /// Removes the scripts loaded after `count`: a scene whose scripts failed
    /// to load leaves the running game as it was.
    pub fn discard_scripts_from(&mut self, count: usize) {
        self.scripts.truncate(count);
    }

    /// Finishes a scene change in this same VM: of the scripts loaded before
    /// `count`, only those attached to `kept` stay.
    pub fn retire_scripts_before(&mut self, count: usize, kept: &HashSet<String>) {
        let mut index = 0;
        self.scripts.retain(|script| {
            let stays = index >= count
                || script
                    .object_id
                    .as_ref()
                    .is_some_and(|id| kept.contains(id));
            index += 1;
            stays
        });
    }

    /// Invokes each script's `update` callback with frame delta seconds.
    pub fn update(&mut self, dt: f32) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("update", CallbackArgs::Delta(dt))
    }

    /// Invokes each script's `fixed_update` callback with fixed delta seconds.
    pub fn fixed_update(&mut self, dt: f32) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("fixed_update", CallbackArgs::Delta(dt))
    }

    /// Delivers a platform or gameplay event to each script in load order.
    pub fn on_event(&mut self, kind: &str, value: &str) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("on_event", CallbackArgs::Event(kind, value))
    }

    /// Delivers a game state change to every script as
    /// `on_state_changed(old, new)`.
    pub fn on_state_changed(
        &mut self,
        old: &str,
        new: &str,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("on_state_changed", CallbackArgs::StateChange(old, new))
    }

    /// Delivers one contact transition to the global script and the two involved objects.
    pub fn on_collision(
        &mut self,
        kind: &str,
        first: &str,
        second: &str,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("on_collision", CallbackArgs::Collision(kind, first, second))
    }

    fn call(
        &mut self,
        name: &str,
        args: CallbackArgs<'_>,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.call_scripts(name, args, Only::All)
    }

    /// Calls one script's callback `name` if it defines it.
    fn invoke(
        &self,
        script: &LoadedScript,
        name: &str,
        args: CallbackArgs<'_>,
        collision_ids: Option<(&str, &str)>,
    ) -> Result<(), ScriptError> {
        let lua_error = |source| ScriptError::Lua {
            path: script.path.clone(),
            source,
        };
        let table: Table = self.lua.registry_value(&script.table).map_err(lua_error)?;
        let function: Option<Function> = table.get(name).map_err(lua_error)?;
        let Some(function) = function else {
            return Ok(());
        };
        *self.active_object.borrow_mut() = script.object_id.clone();
        self.active_callback.set(true);
        self.force_seconds.set(match args {
            CallbackArgs::Delta(dt) if dt.is_finite() && dt >= 0.0 => dt,
            CallbackArgs::Delta(_) => 0.0,
            _ => self.fixed_step,
        });
        let result = match args {
            CallbackArgs::None => function.call::<()>(()),
            CallbackArgs::Delta(dt) => function.call::<()>(dt),
            CallbackArgs::Event(kind, value) => function.call::<()>((kind, value)),
            CallbackArgs::StateChange(old, new) => function.call::<()>((old, new)),
            CallbackArgs::Collision(kind, _, _) => match collision_ids {
                Some((first, second)) => function.call::<()>((kind, first, second)),
                None => Ok(()),
            },
        };
        self.active_callback.set(false);
        *self.active_object.borrow_mut() = None;
        result.map_err(lua_error)
    }

    /// Runs one callback across scripts — all of them, or only those attached
    /// to `only_object` — as one atomic dispatch with a shared budget.
    fn call_scripts(
        &mut self,
        name: &str,
        args: CallbackArgs<'_>,
        only: Only<'_>,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.commands.borrow_mut().clear();
        self.scene.borrow_mut().clear_pending();
        self.save_writes.set(0);
        self.instructions.set(0);
        let original_save = self.save_values.borrow().clone();
        for index in 0..self.scripts.len() {
            if !only.includes(self.scripts[index].object_id.as_deref()) {
                continue;
            }
            // Scripts on objects disabled when the dispatch began get no
            // callbacks; their `start` waits until the object is enabled.
            let disabled = self.scripts[index]
                .object_id
                .as_deref()
                .is_some_and(|id| self.scene.borrow().disabled_at_dispatch_start(id));
            if disabled {
                if name == "start" {
                    self.scripts[index].awaiting_start = true;
                }
                continue;
            }
            let script = &self.scripts[index];
            let collision_ids = if let CallbackArgs::Collision(_, first, second) = args {
                match script.object_id.as_deref() {
                    None => Some((first, second)),
                    Some(id) if id == first => Some((first, second)),
                    Some(id) if id == second => Some((second, first)),
                    Some(_) => continue,
                }
            } else {
                None
            };
            let late_start = name != "start" && script.awaiting_start;
            if name == "start" || late_start {
                self.scripts[index].awaiting_start = false;
            }
            let script = &self.scripts[index];
            let result = if late_start {
                self.invoke(script, "start", CallbackArgs::None, None)
                    .and_then(|()| self.invoke(script, name, args, collision_ids))
            } else {
                self.invoke(script, name, args, collision_ids)
            };
            if let Err(error) = result {
                self.commands.borrow_mut().clear();
                self.scene.borrow_mut().clear_pending();
                *self.save_values.borrow_mut() = original_save;
                return Err(error);
            }
        }
        let mut scene = self.scene.borrow_mut();
        let written = std::mem::take(&mut scene.pending_variables);
        scene.variables.extend(written);
        drop(scene);
        Ok(std::mem::take(&mut *self.commands.borrow_mut()))
    }
}

fn valid_script_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.starts_with("assets/scripts")
        && path.extension().and_then(|ext| ext.to_str()) == Some("lua")
}

fn valid_prefab_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.starts_with("assets/prefabs")
        && path.extension().and_then(|ext| ext.to_str()) == Some("prefab")
}

fn valid_scene_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    !path.is_absolute()
        && !text.contains('\\')
        && !text.contains("//")
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.starts_with("scenes")
        && path.extension().and_then(|ext| ext.to_str()) == Some("ron")
}
