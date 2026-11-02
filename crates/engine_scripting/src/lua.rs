use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path};
use std::rc::Rc;

use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, RegistryKey, StdLib, Table, VmState};
use serde::{Deserialize, Serialize};

use crate::GameCommand;

const MAX_MEMORY: usize = 64 * 1024 * 1024;
const MAX_COMMANDS: usize = 1_024;
const MAX_LOG_BYTES: usize = 64 * 1024;
const HOOK_INTERVAL: u32 = 1_000;
const MAX_INSTRUCTIONS: u32 = 1_000_000;
const MAX_SAVE_BYTES: usize = 64 * 1024;
const MAX_SAVE_ENTRIES: usize = 128;

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

/// Input keys exposed to Lua by their exact names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptKey {
    /// A key.
    A,
    /// D key.
    D,
    /// S key.
    S,
    /// W key.
    W,
    /// Space bar.
    Space,
    /// Escape key.
    Escape,
    /// Enter key.
    Enter,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Left Shift.
    ShiftLeft,
    /// Right Shift.
    ShiftRight,
}

impl ScriptKey {
    /// Every key currently available to Lua.
    pub const ALL: [Self; 13] = [
        Self::A,
        Self::D,
        Self::S,
        Self::W,
        Self::Space,
        Self::Escape,
        Self::Enter,
        Self::ArrowLeft,
        Self::ArrowRight,
        Self::ArrowUp,
        Self::ArrowDown,
        Self::ShiftLeft,
        Self::ShiftRight,
    ];

    /// Name accepted by `engine.key_down` and `engine.key_pressed`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::D => "D",
            Self::S => "S",
            Self::W => "W",
            Self::Space => "Space",
            Self::Escape => "Escape",
            Self::Enter => "Enter",
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::ShiftLeft => "ShiftLeft",
            Self::ShiftRight => "ShiftRight",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "A" => Self::A,
            "D" => Self::D,
            "S" => Self::S,
            "W" => Self::W,
            "Space" => Self::Space,
            "Escape" => Self::Escape,
            "Enter" => Self::Enter,
            "ArrowLeft" => Self::ArrowLeft,
            "ArrowRight" => Self::ArrowRight,
            "ArrowUp" => Self::ArrowUp,
            "ArrowDown" => Self::ArrowDown,
            "ShiftLeft" => Self::ShiftLeft,
            "ShiftRight" => Self::ShiftRight,
            _ => return None,
        })
    }
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
}

#[derive(Default)]
struct SceneSnapshot {
    positions: HashMap<String, [f32; 3]>,
    names: HashMap<String, String>,
    variables: BTreeMap<String, f64>,
    pending_positions: HashMap<String, [f32; 3]>,
    pending_names: HashMap<String, String>,
    pending_variables: BTreeMap<String, f64>,
}

impl SceneSnapshot {
    fn clear_pending(&mut self) {
        self.pending_positions.clear();
        self.pending_names.clear();
        self.pending_variables.clear();
    }

    fn variable(&self, name: &str) -> f64 {
        self.pending_variables
            .get(name)
            .or_else(|| self.variables.get(name))
            .copied()
            .unwrap_or(0.0)
    }

    fn position(&self, object_id: &str) -> Option<[f32; 3]> {
        self.pending_positions
            .get(object_id)
            .or_else(|| self.positions.get(object_id))
            .copied()
    }

    fn named_id(&self, name: &str) -> Option<String> {
        self.names
            .get(name)
            .or_else(|| self.pending_names.get(name))
            .cloned()
    }
}

enum CallbackArgs<'a> {
    None,
    Delta(f32),
    Event(&'a str, &'a str),
    Collision(&'a str, &'a str, &'a str),
}

/// One bounded Lua VM for one game; scripts are tables of lifecycle callbacks.
pub struct LuaRuntime {
    lua: Lua,
    scripts: Vec<LoadedScript>,
    commands: Rc<RefCell<Vec<GameCommand>>>,
    active_object: Rc<RefCell<Option<String>>>,
    key_states: Rc<RefCell<HashMap<ScriptKey, (bool, bool)>>>,
    scene: Rc<RefCell<SceneSnapshot>>,
    active_callback: Rc<Cell<bool>>,
    instructions: Rc<Cell<u32>>,
    save_values: Rc<RefCell<BTreeMap<String, serde_json::Value>>>,
    save_writes: Rc<Cell<usize>>,
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
        let scene = Rc::new(RefCell::new(SceneSnapshot::default()));
        let spawn_scene = Rc::clone(&scene);
        let spawn_commands = Rc::clone(&commands);
        let spawn_active = Rc::clone(&active_callback);
        engine
            .set(
                "spawn_empty",
                lua.create_function(move |_, (name, x, y, z): (String, f32, f32, f32)| {
                    if !spawn_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if name.trim().is_empty() || name.len() > 256 {
                        return Err(mlua::Error::RuntimeError(
                            "object name must be nonempty and at most 256 bytes".to_owned(),
                        ));
                    }
                    let position = [x, y, z];
                    if !position.iter().all(|value| value.is_finite()) {
                        return Err(mlua::Error::RuntimeError(
                            "position must be finite".to_owned(),
                        ));
                    }
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
                    snapshot
                        .pending_positions
                        .insert(object_id.clone(), position);
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
                lua.create_function(move |_, (path, x, y, z): (String, f32, f32, f32)| {
                    if !prefab_active.get() {
                        return Err(mlua::Error::RuntimeError(
                            "engine commands may only run in callbacks".to_owned(),
                        ));
                    }
                    if !valid_prefab_path(Path::new(&path)) {
                        return Err(mlua::Error::RuntimeError(
                            "prefab path must be under assets/prefabs and end in .prefab"
                                .to_owned(),
                        ));
                    }
                    let position = [x, y, z];
                    if !position.iter().all(|value| value.is_finite()) {
                        return Err(mlua::Error::RuntimeError(
                            "position must be finite".to_owned(),
                        ));
                    }
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
                        .pending_positions
                        .insert(object_id.clone(), position);
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
                    lua.create_function(move |_, (object_id, x, y, z): (String, f32, f32, f32)| {
                        if !transform_active.get() {
                            return Err(mlua::Error::RuntimeError(
                                "engine commands may only run in callbacks".to_owned(),
                            ));
                        }
                        if object_id.is_empty() || object_id.len() > 64 {
                            return Err(mlua::Error::RuntimeError(
                                "object ID must be nonempty and at most 64 bytes".to_owned(),
                            ));
                        }
                        if ![x, y, z].iter().all(|value| value.is_finite()) {
                            return Err(mlua::Error::RuntimeError(
                                "position must be finite".to_owned(),
                            ));
                        }
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
        let key_states = Rc::new(RefCell::new(HashMap::<ScriptKey, (bool, bool)>::new()));
        let held_states = Rc::clone(&key_states);
        engine
            .set(
                "key_down",
                lua.create_function(move |_, name: String| {
                    let key = ScriptKey::from_name(&name)
                        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown key: {name}")))?;
                    Ok(held_states.borrow().get(&key).is_some_and(|state| state.0))
                })
                .map_err(ScriptError::Initialize)?,
            )
            .map_err(ScriptError::Initialize)?;
        let pressed_states = Rc::clone(&key_states);
        engine
            .set(
                "key_pressed",
                lua.create_function(move |_, name: String| {
                    let key = ScriptKey::from_name(&name)
                        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown key: {name}")))?;
                    Ok(pressed_states
                        .borrow()
                        .get(&key)
                        .is_some_and(|state| state.1))
                })
                .map_err(ScriptError::Initialize)?,
            )
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
        let positioned_scene = Rc::clone(&scene);
        engine
            .set(
                "get_position",
                lua.create_function(move |lua, object_id: String| {
                    positioned_scene
                        .borrow()
                        .position(&object_id)
                        .map(|[x, y, z]| lua.create_table_from([("x", x), ("y", y), ("z", z)]))
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
            key_states,
            scene,
            active_callback,
            instructions,
            save_values,
            save_writes,
        })
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
        self.key_states.borrow_mut().insert(key, (down, pressed));
    }

    /// Replaces the Lua-visible scene snapshot before a gameplay callback.
    /// Names resolve to the first matching object in iteration order.
    pub fn set_scene_snapshot(
        &mut self,
        objects: impl IntoIterator<Item = (String, Option<String>, Option<[f32; 3]>)>,
    ) {
        let mut scene = self.scene.borrow_mut();
        scene.positions.clear();
        scene.names.clear();
        scene.clear_pending();
        for (id, name, position) in objects {
            if let Some(name) = name {
                scene.names.entry(name).or_insert_with(|| id.clone());
            }
            if let Some(position) = position.filter(|p| p.iter().all(|value| value.is_finite())) {
                scene.positions.insert(id, position);
            }
        }
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
        });
        Ok(())
    }

    /// Invokes each script's `start` callback in load order.
    pub fn start(&mut self) -> Result<Vec<GameCommand>, ScriptError> {
        self.call("start", CallbackArgs::None)
    }

    /// Invokes `start` only for scripts attached to `object_id`, as when a
    /// scripted prefab is spawned during play. Other scripts are not called.
    pub fn start_object(&mut self, object_id: &str) -> Result<Vec<GameCommand>, ScriptError> {
        self.call_scripts("start", CallbackArgs::None, Some(object_id))
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
        self.call_scripts(name, args, None)
    }

    /// Runs one callback across scripts — all of them, or only those attached
    /// to `only_object` — as one atomic dispatch with a shared budget.
    fn call_scripts(
        &mut self,
        name: &str,
        args: CallbackArgs<'_>,
        only_object: Option<&str>,
    ) -> Result<Vec<GameCommand>, ScriptError> {
        self.commands.borrow_mut().clear();
        self.scene.borrow_mut().clear_pending();
        self.save_writes.set(0);
        self.instructions.set(0);
        let original_save = self.save_values.borrow().clone();
        for script in &self.scripts {
            if only_object.is_some() && script.object_id.as_deref() != only_object {
                continue;
            }
            let collision_ids = if let CallbackArgs::Collision(_, first, second) = args {
                match script.object_id.as_deref() {
                    None => Some((first, second)),
                    Some(id) if id == first => Some((first, second)),
                    Some(id) if id == second => Some((second, first)),
                    Some(_) => None,
                }
            } else {
                None
            };
            if matches!(args, CallbackArgs::Collision(..)) && collision_ids.is_none() {
                continue;
            }
            let table: Table =
                self.lua
                    .registry_value(&script.table)
                    .map_err(|source| ScriptError::Lua {
                        path: script.path.clone(),
                        source,
                    })?;
            let function: Option<Function> =
                table.get(name).map_err(|source| ScriptError::Lua {
                    path: script.path.clone(),
                    source,
                })?;
            if let Some(function) = function {
                *self.active_object.borrow_mut() = script.object_id.clone();
                self.active_callback.set(true);
                let result = match args {
                    CallbackArgs::None => function.call::<()>(()),
                    CallbackArgs::Delta(dt) => function.call::<()>(dt),
                    CallbackArgs::Event(kind, value) => function.call::<()>((kind, value)),
                    CallbackArgs::Collision(kind, _, _) => match collision_ids {
                        Some((first, second)) => function.call::<()>((kind, first, second)),
                        None => Ok(()),
                    },
                };
                self.active_callback.set(false);
                *self.active_object.borrow_mut() = None;
                if let Err(source) = result {
                    self.commands.borrow_mut().clear();
                    self.scene.borrow_mut().clear_pending();
                    *self.save_values.borrow_mut() = original_save;
                    return Err(ScriptError::Lua {
                        path: script.path.clone(),
                        source,
                    });
                }
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
