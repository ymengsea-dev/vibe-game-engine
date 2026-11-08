//! Standalone RustyEngine runtime.
//!
//! Runs an *exported* game folder: a `main.ron` scene plus an
//! `assets.pak` bundle (or a loose `assets/` directory as a fallback).
//! Nothing here links the editor or AI crates — `engine` is depended on
//! with `default-features = false` so the `studio` feature is off
//! (NFR-004).
//!
//! Usage: `player [<export-dir>]` or `player --project <dir> [--scene <relative-path>]`.
//!
//! ## Shape
//!
//! A thin [`Game`] implementation, nothing more. [`run_game`] owns the
//! window, GPU, render pipelines, world and frame loop. The scene itself —
//! instantiation, gameplay commands, visual rules, collisions and scene
//! changes — runs through [`GameplayRuntime`], the same engine runtime a
//! Rust game uses. This file only adds what is Lua-specific: loading scripts,
//! feeding the VM input and scene snapshots, running callbacks, reporting
//! script errors to Studio, and the save file. Deliberate: anything a Lua
//! game can do but a Rust game cannot is a gap in `engine::gameplay`.
//!
//! ## Failure stance
//!
//! An export is untrusted input. A missing or corrupt bundle is a clean
//! error and a non-zero exit; a single broken asset reference inside an
//! otherwise-good scene is a warning and a skipped entity, so one bad
//! file never costs you the rest of the level.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::gameplay::platform_key;
use engine::gameplay::{GameplayError, GameplayRuntime, SceneSource, SpawnedObject};
use engine::prelude::{
    AssetLibrary, EXPORT_SCENE_FILE, Game, GameCommand, GameConfig, GameContext, GameError,
    GameplayLanguage, LuaRuntime, Project, ProjectError, Scene, SceneError, ScriptError,
    executable_dir, open_export, run_game,
};
use engine::scripting::{ScriptKey, SnapshotObject};

fn script_error_record(error: &ScriptError) -> Result<Option<String>, serde_json::Error> {
    error
        .diagnostic()
        .map(|diagnostic| {
            serde_json::to_string(&diagnostic)
                .map(|json| format!("{}{}", engine::scripting::LUA_DIAGNOSTIC_PREFIX, json))
        })
        .transpose()
}

fn report_script_error(error: &ScriptError) {
    match script_error_record(error) {
        Ok(Some(record)) => {
            if let Err(io_error) = writeln!(std::io::stderr().lock(), "{record}") {
                tracing::warn!(error = %io_error, "could not send Lua diagnostic to Studio");
            }
        }
        Ok(None) => {}
        Err(json_error) => tracing::warn!(error = %json_error, "could not encode Lua diagnostic"),
    }
}

/// Opens the export's assets: a packed `assets.pak` if there is one,
/// else a loose `assets/` directory.
///
/// Indexing and resolution both come from the engine
/// ([`AssetLibrary`], [`RuntimeResolver`]) rather than living here. They
/// used to: this file carried its own id index, its own `.meta` parser
/// and its own resolver, all of which the next game would have had to
/// copy.
fn open_assets(export_dir: &Path) -> Option<AssetLibrary> {
    // The packed case is the engine's own `open_export`, the same call a
    // shipped game makes — the player must not have a second way of
    // reading an export, or the two drift and only one gets tested.
    match open_export(export_dir) {
        Some(Ok(content)) => return Some(content.library),
        Some(Err(err)) => {
            tracing::warn!(error = %err, "the export is unreadable; trying loose files");
        }
        None => {}
    }

    let loose = export_dir.join("assets");
    if loose.is_dir() {
        tracing::info!(dir = %loose.display(), "using loose asset directory");
        match AssetLibrary::index(&loose) {
            Ok(library) => return Some(library),
            Err(err) => tracing::warn!(error = %err, "asset directory could not be indexed"),
        }
    }
    None
}

/// Errors that stop the player before a window ever opens.
#[derive(Debug, thiserror::Error)]
enum PlayerError {
    /// No `main.ron` in the export directory or its bundle.
    #[error("no main.ron scene found in {0}")]
    MissingScene(PathBuf),
    /// `main.ron` exists but isn't a usable scene.
    #[error("scene could not be loaded: {0}")]
    Scene(#[from] SceneError),
    /// The scene text wasn't valid UTF-8.
    #[error("main.ron is not valid UTF-8 text")]
    SceneEncoding,
    /// A Lua project has no starter gameplay file in its asset bundle.
    #[error("Lua script {0} is missing")]
    MissingLuaScript(String),
    /// A Lua project script is not UTF-8 source text.
    #[error("Lua script {0} is not valid UTF-8")]
    LuaEncoding(String),
    /// Scene attachment has not been assigned an object identity.
    #[error("Lua attachment {0} has no scene object ID")]
    MissingObjectId(String),
    /// Lua initialization or script loading failed.
    #[error(transparent)]
    Script(#[from] ScriptError),
    /// Project manifest or project data could not be loaded.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// This entry point is for Lua projects; Rust games run their own binary.
    #[error("the project uses Rust gameplay; run its game binary instead")]
    RustProject,
    /// A scene attached code from the other project language.
    #[error("Lua project cannot run Rust script {0}")]
    WrongScriptLanguage(String),
    /// The export uses a newer or corrupt Lua package format.
    #[error("unsupported Lua export marker in {0}")]
    InvalidLuaMarker(PathBuf),
    /// A save is unreadable; launching would risk overwriting progress.
    #[error("Lua save at {0} is invalid: {1}; move it aside before starting a new game")]
    InvalidSave(PathBuf, String),
    /// A gameplay command, prefab or scene change failed.
    #[error(transparent)]
    Gameplay(#[from] GameplayError),
}

fn load_lua_scripts(assets: &AssetLibrary, scene: &Scene) -> Result<LuaRuntime, PlayerError> {
    scene.validate()?;
    let mut runtime = LuaRuntime::new()?;
    load_lua_script(&mut runtime, assets, "assets/scripts/main.lua", None)?;
    for entity in &scene.entities {
        if let Some(path) = entity.script.as_deref() {
            if !path.ends_with(".lua") {
                return Err(PlayerError::WrongScriptLanguage(path.to_owned()));
            }
            let object_id = entity
                .object_id
                .as_deref()
                .ok_or_else(|| PlayerError::MissingObjectId(path.to_owned()))?;
            load_lua_script(&mut runtime, assets, path, Some(object_id))?;
        }
    }
    Ok(runtime)
}

fn load_lua_script(
    runtime: &mut LuaRuntime,
    assets: &AssetLibrary,
    path: &str,
    object_id: Option<&str>,
) -> Result<(), PlayerError> {
    let relative = path
        .strip_prefix("assets/")
        .ok_or_else(|| ScriptError::InvalidPath(path.to_owned()))?;
    let bytes = assets
        .read_path(relative)?
        .ok_or_else(|| PlayerError::MissingLuaScript(path.to_owned()))?;
    let source = String::from_utf8(bytes).map_err(|_| PlayerError::LuaEncoding(path.to_owned()))?;
    let result = match object_id {
        Some(id) => runtime.load_attached_script(path, &source, id),
        None => runtime.load_script(path, &source),
    };
    result.map_err(|error| {
        report_script_error(&error);
        PlayerError::Script(error)
    })?;
    Ok(())
}

/// Reads and parses the export's scene.
fn load_scene(export_dir: &Path) -> Result<Scene, PlayerError> {
    let path = export_dir.join(EXPORT_SCENE_FILE);
    let bytes =
        std::fs::read(&path).map_err(|_| PlayerError::MissingScene(export_dir.to_path_buf()))?;
    let text = String::from_utf8(bytes).map_err(|_| PlayerError::SceneEncoding)?;
    let scene = Scene::from_ron_str(&text)?;
    scene.validate()?;
    Ok(scene)
}

/// Scripted prefab `start` callbacks run per dispatch before the rest wait a turn.
const MAX_PREFAB_STARTS_PER_DISPATCH: usize = 64;

/// The exported game, as a [`Game`]: the shared [`GameplayRuntime`] plus the
/// Lua VM that drives it. Without Lua (a non-Lua export) it simply shows the scene.
struct Player {
    gameplay: GameplayRuntime,
    lua: Option<LuaRuntime>,
    /// Scripted prefab instances whose `start` has not run yet.
    pending_starts: VecDeque<String>,
    save_path: Option<PathBuf>,
    saved_values: Option<String>,
    invalid_save: Option<String>,
}

fn exported_save_path(data_root: &Path, id: uuid::Uuid) -> PathBuf {
    data_root
        .join("games")
        .join(id.to_string())
        .join("save.ron")
}

fn user_data_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("VGE Studio"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|path| PathBuf::from(path).join("Library/Application Support/VGE Studio"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .map(|path| path.join("vge-studio"))
    }
}

impl Player {
    fn new(gameplay: GameplayRuntime, lua: Option<LuaRuntime>, save_path: Option<PathBuf>) -> Self {
        let saved_values = lua.as_ref().map(|_| "{}".to_owned());
        Self {
            gameplay,
            lua,
            pending_starts: VecDeque::new(),
            save_path,
            saved_values,
            invalid_save: None,
        }
    }

    /// Loads the target scene and its scripts without touching the running game,
    /// so a broken target leaves the current scene playing.
    fn prepare_scene_change(&self, path: &str) -> Result<(Scene, LuaRuntime), PlayerError> {
        let scene = self.gameplay.load_scene(path)?;
        let assets = self
            .gameplay
            .assets()
            .ok_or_else(|| PlayerError::MissingLuaScript("assets/scripts/main.lua".into()))?;
        let mut lua = load_lua_scripts(assets, &scene)?;
        let previous = self
            .lua
            .as_ref()
            .ok_or_else(|| GameplayError::SceneRequest(path.into(), "Lua stopped".into()))?;
        lua.restore_saved_data(&previous.saved_data()?)?;
        Ok((scene, lua))
    }

    fn perform_scene_change(
        &mut self,
        ctx: &mut GameContext<'_>,
        path: &str,
    ) -> Result<(), PlayerError> {
        let (scene, lua) = self.prepare_scene_change(path)?;
        self.remember_lua_values();
        self.pending_starts.clear();
        self.gameplay.replace_scene(ctx, scene)?;
        self.lua = Some(lua);
        let started = self.run_callback(ctx, |lua| lua.start());
        self.gameplay.start_rules(ctx);
        self.flush_rule_spawns(ctx);
        started.map_err(|error| GameplayError::SceneRequest(path.into(), error))?;
        Ok(())
    }

    /// Syncs input and the scene snapshot, runs one Lua callback, and applies
    /// its commands; then starts any scripted prefabs those commands spawned.
    /// Any failure stops Lua for the rest of the run.
    fn run_callback(
        &mut self,
        ctx: &mut GameContext<'_>,
        callback: impl FnOnce(&mut LuaRuntime) -> Result<Vec<GameCommand>, ScriptError>,
    ) -> Result<(), String> {
        let mut result = self.run_once(ctx, callback);
        // Spawned scripts may spawn more; cap the chain so a prefab that spawns
        // itself cannot stall a frame. The remainder starts on the next callback.
        let mut budget = MAX_PREFAB_STARTS_PER_DISPATCH;
        while result.is_ok() && budget > 0 {
            let Some(object_id) = self.pending_starts.pop_front() else {
                break;
            };
            budget -= 1;
            result = self.run_once(ctx, |lua| lua.start_object(&object_id));
        }
        if result.is_err() {
            self.lua = None;
            self.pending_starts.clear();
        }
        self.remember_lua_values();
        result
    }

    fn run_once(
        &mut self,
        ctx: &mut GameContext<'_>,
        callback: impl FnOnce(&mut LuaRuntime) -> Result<Vec<GameCommand>, ScriptError>,
    ) -> Result<(), String> {
        let Some(lua) = &mut self.lua else {
            return Ok(());
        };
        lua.set_fixed_step(ctx.fixed_step_seconds());
        update_lua_input(lua, ctx.input(), ctx.mouse_locked());
        sync_lua_scene(lua, ctx.world(), ctx.physics());
        lua.set_active_camera(self.gameplay.active_camera_id(ctx.world()));
        lua.set_game_state(self.gameplay.game_state(ctx.world()).name());
        // The queries borrow `ctx` only while the callback runs; they are gone
        // before `apply` takes it mutably.
        let commands = {
            let queries = self.gameplay.queries(ctx);
            lua.with_queries(&queries, callback).map_err(|error| {
                report_script_error(&error);
                error.to_string()
            })?
        }
        .map_err(|error| {
            report_script_error(&error);
            error.to_string()
        })?;
        let mut spawned = self
            .gameplay
            .apply(ctx, commands)
            .map_err(|error| error.to_string())?;
        // Prefabs spawned by visual rules the commands triggered (such as an
        // emitted event) start their scripts too.
        spawned.extend(self.gameplay.take_rule_spawns());
        self.forget_destroyed();
        self.attach_spawned_scripts(&spawned)
            .map_err(|error| error.to_string())
    }

    /// Loads and starts the scripts of prefabs that visual rules spawned
    /// outside a Lua callback (scene start, keys, timers, clicks, contacts).
    fn flush_rule_spawns(&mut self, ctx: &mut GameContext<'_>) {
        self.forget_destroyed();
        let spawned = self.gameplay.take_rule_spawns();
        if spawned.is_empty() || self.lua.is_none() {
            return;
        }
        if let Err(error) = self.attach_spawned_scripts(&spawned) {
            tracing::error!(%error, "rule-spawned script could not be loaded");
            self.lua = None;
            self.pending_starts.clear();
            return;
        }
        if self.pending_starts.is_empty() {
            return;
        }
        // A callback that does nothing runs the queued `start`s.
        if let Err(error) = self.run_callback(ctx, |_| Ok(Vec::new())) {
            tracing::error!(%error, "rule-spawned script start stopped");
        }
    }

    /// Drops the scripts of objects the engine destroyed (by Lua, rules or
    /// anything else) so they stop running and stop using the budget.
    fn forget_destroyed(&mut self) {
        let destroyed = self.gameplay.take_destroyed();
        if let Some(lua) = self.lua.as_mut() {
            lua.forget_objects(&destroyed);
        }
    }

    /// Delivers the latest game state change, if any, to every script. A
    /// change made by that callback is delivered with the next batch, so a
    /// script that flips the state back cannot loop within a frame.
    fn deliver_state_change(&mut self, ctx: &mut GameContext<'_>) {
        let Some((old, new)) = self.gameplay.take_state_change() else {
            return;
        };
        if self.lua.is_none() {
            return;
        }
        if let Err(error) =
            self.run_callback(ctx, |lua| lua.on_state_changed(old.name(), new.name()))
        {
            tracing::error!(%error, "Lua state change callback stopped");
        }
    }

    /// Loads the scripts of newly spawned prefab instances and queues their `start`.
    fn attach_spawned_scripts(&mut self, spawned: &[SpawnedObject]) -> Result<(), PlayerError> {
        let (Some(lua), Some(assets)) = (self.lua.as_mut(), self.gameplay.assets()) else {
            return Ok(());
        };
        for object in spawned {
            let Some(path) = object.script.as_deref() else {
                continue;
            };
            if !path.ends_with(".lua") {
                return Err(PlayerError::WrongScriptLanguage(path.to_owned()));
            }
            load_lua_script(lua, assets, path, Some(&object.object_id))?;
            self.pending_starts.push_back(object.object_id.clone());
        }
        Ok(())
    }

    fn remember_lua_values(&mut self) {
        if let Some(lua) = &self.lua {
            match lua.saved_data() {
                Ok(data) => self.saved_values = Some(data),
                Err(error) => tracing::warn!(error = %error, "could not capture Lua save values"),
            }
        }
    }
}

fn validate_lua_save(player: &mut Player) -> Result<(), PlayerError> {
    let (Some(path), Some(lua)) = (player.save_path.as_ref(), player.lua.as_mut()) else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    let save = engine::scene::SaveGame::<String>::load_from_file(path)
        .map_err(|error| PlayerError::InvalidSave(path.clone(), error.to_string()))?;
    lua.restore_saved_data(&save.game)
        .map_err(|error| PlayerError::InvalidSave(path.clone(), error.to_string()))?;
    player.saved_values = Some(save.game);
    Ok(())
}

fn parse_lua_marker(bytes: &[u8]) -> Result<Option<uuid::Uuid>, ()> {
    if bytes == engine::project::build::LUA_EXPORT_VERSION {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let id = text
        .strip_prefix("2\n")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or(())?;
    let id = uuid::Uuid::parse_str(id).map_err(|_| ())?;
    if id.is_nil() {
        return Err(());
    }
    Ok(Some(id))
}

/// The Run window quits on Esc. In Scene View Play, Esc belongs to Studio
/// (it frees a locked mouse) and Stop or Cmd-P ends the game.
fn exits_on_escape(embedded: bool) -> bool {
    !embedded
}

fn update_lua_input(
    lua: &mut LuaRuntime,
    input: &engine::platform::InputState,
    mouse_locked: bool,
) {
    use engine::platform::MouseButton;
    use engine::scripting::{
        ButtonState, InputSnapshot, ScriptGamepadAxis, ScriptGamepadButton, ScriptMouseButton,
    };
    let mut snapshot = InputSnapshot {
        mouse_position: input.cursor_position().map(|(x, y)| (x as f32, y as f32)),
        mouse_delta: input.mouse_delta(),
        wheel: input.scroll_delta(),
        mouse_locked,
        ..InputSnapshot::default()
    };
    for key in ScriptKey::ALL {
        let platform = platform_key(*key);
        snapshot.keys.insert(
            *key,
            ButtonState {
                down: input.is_key_held(platform),
                pressed: input.is_key_pressed(platform),
                released: input.is_key_released(platform),
            },
        );
    }
    for button in ScriptMouseButton::ALL {
        let platform = match button {
            ScriptMouseButton::Left => MouseButton::Left,
            ScriptMouseButton::Right => MouseButton::Right,
            ScriptMouseButton::Middle => MouseButton::Middle,
        };
        snapshot.mouse_buttons.insert(
            *button,
            ButtonState {
                down: input.is_mouse_button_held(platform),
                pressed: input.is_mouse_button_pressed(platform),
                released: input.is_mouse_button_released(platform),
            },
        );
    }
    for button in ScriptGamepadButton::ALL {
        let platform = platform_gamepad_button(*button);
        snapshot.gamepad_buttons.insert(
            *button,
            ButtonState {
                down: input.is_button_held(platform),
                pressed: input.is_button_pressed(platform),
                released: input.is_button_released(platform),
            },
        );
    }
    // Sticks are dead-zoned as a pair (0.15 radial); triggers per axis.
    let left = input.stick(engine::platform::Stick::Left);
    let right = input.stick(engine::platform::Stick::Right);
    for axis in ScriptGamepadAxis::ALL {
        let value = match axis {
            ScriptGamepadAxis::LeftX => left.0,
            ScriptGamepadAxis::LeftY => left.1,
            ScriptGamepadAxis::RightX => right.0,
            ScriptGamepadAxis::RightY => right.1,
            ScriptGamepadAxis::LeftTrigger | ScriptGamepadAxis::RightTrigger => {
                input.axis(platform_gamepad_axis(*axis))
            }
        };
        snapshot.gamepad_axes.insert(*axis, value);
    }
    lua.set_input(snapshot);
}

fn platform_gamepad_button(
    button: engine::scripting::ScriptGamepadButton,
) -> engine::platform::GamepadButton {
    use engine::platform::GamepadButton as P;
    use engine::scripting::ScriptGamepadButton as S;
    match button {
        S::South => P::South,
        S::East => P::East,
        S::West => P::West,
        S::North => P::North,
        S::Start => P::Start,
        S::Select => P::Select,
        S::LeftShoulder => P::LeftBumper,
        S::RightShoulder => P::RightBumper,
        S::LeftStick => P::LeftStick,
        S::RightStick => P::RightStick,
        S::DPadUp => P::DPadUp,
        S::DPadDown => P::DPadDown,
        S::DPadLeft => P::DPadLeft,
        S::DPadRight => P::DPadRight,
    }
}

fn platform_gamepad_axis(
    axis: engine::scripting::ScriptGamepadAxis,
) -> engine::platform::GamepadAxis {
    use engine::platform::GamepadAxis as P;
    use engine::scripting::ScriptGamepadAxis as S;
    match axis {
        S::LeftX => P::LeftStickX,
        S::LeftY => P::LeftStickY,
        S::RightX => P::RightStickX,
        S::RightY => P::RightStickY,
        S::LeftTrigger => P::LeftTrigger,
        S::RightTrigger => P::RightTrigger,
    }
}

fn sync_lua_scene(
    lua: &mut LuaRuntime,
    world: &engine::prelude::World,
    physics: &engine::physics::PhysicsWorld,
) {
    // shortcut: snapshot authored objects for each callback; use dirty-object sync if large scenes make this costly.
    lua.set_scene_objects(world.iter_entities().filter_map(|entity| {
        let id = entity.get::<engine::scene::SceneObjectId>()?;
        let name = entity
            .get::<engine::ecs::components::Name>()
            .map(|name| name.0.clone());
        let transform = entity
            .get::<engine::ecs::components::Transform>()
            .map(|transform| transform.0);
        Some(SnapshotObject {
            id: id.0.clone(),
            name,
            position: transform.map(|t| t.translation.to_array()),
            rotation: transform.map_or([0.0, 0.0, 0.0, 1.0], |t| t.rotation.to_array()),
            scale: transform.map_or([1.0; 3], |t| t.scale.to_array()),
            velocity: entity
                .get::<engine::ecs::components::RigidBody>()
                .and_then(|body| physics.rapier.bodies.get(body.0))
                .filter(|body| body.is_dynamic())
                .map(|body| body.linvel().to_array()),
            visible: entity.get::<engine::ecs::components::Hidden>().is_none(),
            enabled: entity.get::<engine::ecs::components::Disabled>().is_none(),
        })
    }));
    if let Some(variables) = world.get_resource::<engine::scene::RuleVariables>() {
        lua.set_variables(variables.iter());
    }
}

fn lua_key_event(event: &engine::platform::PlatformEvent) -> Option<(&'static str, &'static str)> {
    let engine::platform::PlatformEvent::KeyboardInput {
        key,
        pressed,
        repeat,
    } = event
    else {
        return None;
    };
    if *repeat {
        return None;
    }
    ScriptKey::ALL
        .iter()
        .copied()
        .find(|candidate| platform_key(*candidate) == *key)
        .map(|key| {
            (
                if *pressed {
                    "key_pressed"
                } else {
                    "key_released"
                },
                key.name(),
            )
        })
}

fn load_project(root: &Path, selected_scene: Option<&Path>) -> Result<Player, PlayerError> {
    let project = Project::open(root)?;
    if project.language() != GameplayLanguage::Lua {
        return Err(PlayerError::RustProject);
    }
    let scene_path = match selected_scene {
        Some(relative) => project.resolve(relative)?,
        None => project.main_scene_path(),
    };
    let mut scene = Scene::load_from_file(&scene_path)?;
    scene.validate()?;
    scene.assign_missing_object_ids();
    for script in std::iter::once("assets/scripts/main.lua").chain(
        scene
            .entities
            .iter()
            .filter_map(|entity| entity.script.as_deref()),
    ) {
        project.resolve(Path::new(script))?;
    }
    let assets = AssetLibrary::index(&project.assets_dir())?;
    let lua = load_lua_scripts(&assets, &scene)?;
    let save_path = project.resolve(Path::new(".studio/play-save.ron"))?;
    let gameplay =
        GameplayRuntime::new(scene, Some(assets)).with_scene_source(SceneSource::Project(project));
    let mut player = Player::new(gameplay, Some(lua), Some(save_path));
    validate_lua_save(&mut player)?;
    Ok(player)
}

/// Which Lua callbacks run this frame. Pausing the game state freezes
/// `update` and `fixed_update`; UI clicks and state changes still reach scripts
/// so a menu can unpause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePhases {
    ui_clicks: bool,
    update: bool,
    fixed_update: bool,
}

impl FramePhases {
    fn for_state(paused: bool) -> Self {
        Self {
            ui_clicks: true,
            update: !paused,
            fixed_update: !paused,
        }
    }
}

impl Game for Player {
    fn frame_demand(&self, now: Instant) -> engine::prelude::FrameDemand {
        if self.lua.is_some() {
            engine::prelude::FrameDemand::AnimateUntil(now + Duration::from_millis(16))
        } else {
            engine::prelude::FrameDemand::Idle
        }
    }

    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        self.gameplay.setup(ctx)
    }

    fn load_data(&mut self, data: &str) {
        if let Some(lua) = &mut self.lua {
            match lua.restore_saved_data(data) {
                Ok(()) => self.saved_values = Some(data.to_owned()),
                Err(error) => self.invalid_save = Some(error.to_string()),
            }
        }
    }

    fn save_data(&self) -> Option<String> {
        self.saved_values.clone()
    }

    fn on_ready(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        if let Some(error) = &self.invalid_save {
            return Err(GameError::Setup(format!(
                "loaded Lua save is invalid; preserving it: {error}"
            )));
        }
        let started = self.run_callback(ctx, |lua| lua.start());
        self.gameplay.start_rules(ctx);
        self.flush_rule_spawns(ctx);
        started.map_err(GameError::Setup)
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if let Some(path) = self.gameplay.take_scene_request()
            && let Err(error) = self.perform_scene_change(ctx, &path)
        {
            tracing::error!(scene = %path, error = %error, "scene change failed");
        }
        if exits_on_escape(ctx.is_embedded())
            && ctx.input().is_key_pressed(engine::prelude::KeyCode::Escape)
        {
            ctx.request_exit();
        }
        let phases = FramePhases::for_state(ctx.simulation_paused());
        let clicked = self.gameplay.dispatch_ui_clicks(ctx);
        self.flush_rule_spawns(ctx);
        for id in clicked {
            if phases.ui_clicks
                && let Err(error) = self.run_callback(ctx, |lua| lua.on_event("ui_clicked", &id))
            {
                tracing::error!(%error, "Lua UI click callback stopped");
            }
        }
        self.gameplay.update_rules(ctx, dt);
        self.flush_rule_spawns(ctx);
        self.deliver_state_change(ctx);
        // A click or state callback may have paused or resumed the game.
        let phases = FramePhases::for_state(ctx.simulation_paused());
        if phases.update
            && let Err(error) = self.run_callback(ctx, |lua| lua.update(dt))
        {
            tracing::error!(%error, "Lua update stopped");
        }
        self.deliver_state_change(ctx);
        self.gameplay.refresh_lights(ctx);
        // Last, so the view reflects scripts and scene changes this frame.
        self.gameplay.present_camera(ctx, dt);
    }

    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if !FramePhases::for_state(ctx.simulation_paused()).fixed_update {
            return;
        }
        if let Err(error) = self.run_callback(ctx, |lua| lua.fixed_update(dt)) {
            tracing::error!(%error, "Lua fixed_update stopped");
        }
    }

    fn post_fixed_update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
        let contacts = self.gameplay.sample_collisions(ctx);
        self.flush_rule_spawns(ctx);
        for contact in contacts {
            if self.lua.is_none() {
                break;
            }
            let result = self.run_callback(ctx, |lua| {
                lua.on_collision(contact.kind.name(), &contact.first, &contact.second)
            });
            if let Err(error) = result {
                tracing::error!(%error, "Lua collision callback stopped");
            }
        }
    }

    fn on_event(&mut self, ctx: &mut GameContext<'_>, event: &engine::platform::PlatformEvent) {
        if let Some((kind, key)) = lua_key_event(event)
            && let Err(error) = self.run_callback(ctx, |lua| lua.on_event(kind, key))
        {
            tracing::error!(%error, "Lua event callback stopped");
        }
    }
}

/// Loads the export and runs it.
fn run(export_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let player = load_export(export_dir)?;
    let mut config = GameConfig::new("RustyEngine Player", 1280, 720);
    config.save_path = player.save_path.clone();
    run_game(config, player)?;
    Ok(())
}

fn load_export(export_dir: &Path) -> Result<Player, PlayerError> {
    let assets = open_assets(export_dir);
    match &assets {
        Some(library) => tracing::info!(assets = library.len(), "asset index ready"),
        None => tracing::warn!("no assets.pak or assets/ directory found"),
    }

    let mut scene = load_scene(export_dir)?;
    scene.assign_missing_object_ids();
    let marker = export_dir.join(engine::project::build::LUA_EXPORT_MARKER);
    let (lua, save_path) = if marker.exists() {
        let version =
            std::fs::read(&marker).map_err(|_| PlayerError::InvalidLuaMarker(marker.clone()))?;
        let id = parse_lua_marker(&version).map_err(|()| PlayerError::InvalidLuaMarker(marker))?;
        let lua = load_lua_scripts(
            assets
                .as_ref()
                .ok_or_else(|| PlayerError::MissingLuaScript("assets/scripts/main.lua".into()))?,
            &scene,
        )?;
        let path = id.and_then(|id| user_data_root().map(|root| exported_save_path(&root, id)));
        (Some(lua), path)
    } else {
        (None, None)
    };
    let gameplay = GameplayRuntime::new(scene, assets)
        .with_scene_source(SceneSource::Export(export_dir.to_path_buf()));
    let mut player = Player::new(gameplay, lua, save_path);
    validate_lua_save(&mut player)?;
    Ok(player)
}

fn run_project(
    root: &Path,
    selected_scene: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let player = load_project(root, selected_scene)?;
    let mut config = GameConfig::new("RustyEngine Lua Game", 1280, 720);
    config.save_path = player.save_path.clone();
    run_game(config, player)?;
    Ok(())
}

fn default_export_dir() -> PathBuf {
    executable_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [flag, root] if flag == "--project" => run_project(Path::new(root), None),
        [flag, root, scene_flag, scene] if flag == "--project" && scene_flag == "--scene" => {
            run_project(Path::new(root), Some(Path::new(scene)))
        }
        [dir] => run(Path::new(dir)),
        [] => run(&default_export_dir()),
        _ => Err(
            "usage: player [<export-dir>] | --project <project-dir> [--scene <relative-scene>]"
                .into(),
        ),
    };
    if let Err(err) = result {
        tracing::error!(error = %err, "player exited with an error");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn paused_player_skips_update_but_handles_ui_clicks() {
        let paused = super::FramePhases::for_state(true);
        assert!(paused.ui_clicks);
        assert!(!paused.update);
        assert!(!paused.fixed_update);
        let playing = super::FramePhases::for_state(false);
        assert!(playing.ui_clicks && playing.update && playing.fixed_update);
    }

    #[test]
    fn lua_state_round_trips_through_the_player_calls() {
        let mut lua = super::LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { on_state_changed = function(o, n) engine.log(o .. '>' .. n .. ':' .. engine.get_state()) end }",
        )
        .expect("script");
        lua.set_game_state("paused");
        match lua
            .on_state_changed("playing", "paused")
            .expect("cb")
            .as_slice()
        {
            [super::GameCommand::Log(text)] => assert_eq!(text, "playing>paused:paused"),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn left_x_for_raw_stick(x: f32, y: f32) -> String {
        use engine::platform::{GamepadAxes, GamepadAxis, GamepadUpdate, InputState};
        let mut axes = GamepadAxes::default();
        axes.set(GamepadAxis::LeftStickX, x);
        axes.set(GamepadAxis::LeftStickY, y);
        let mut input = InputState::new();
        input.apply_gamepad(&GamepadUpdate {
            axes,
            connected: 1,
            ..GamepadUpdate::default()
        });
        let mut lua = super::LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function() engine.log(tostring(engine.gamepad_axis('left_x'))) end }",
        )
        .expect("script");
        super::update_lua_input(&mut lua, &input, false);
        match lua.update(0.1).expect("update").as_slice() {
            [super::GameCommand::Log(text)] => text.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn lua_sticks_use_the_radial_dead_zone() {
        // |(0.12, 0.12)| = 0.17 > 0.15: passes although each axis is under it.
        assert_ne!(left_x_for_raw_stick(0.12, 0.12), "0.0");
        assert_eq!(left_x_for_raw_stick(0.10, 0.0), "0.0");
    }

    #[test]
    fn sync_lua_scene_reports_visible_and_enabled() {
        use engine::ecs::components::{Disabled, Hidden};
        let mut world = engine::prelude::World::new();
        let spawn = |world: &mut engine::prelude::World, id: &str| {
            world.spawn(engine::scene::SceneObjectId(id.into())).id()
        };
        spawn(&mut world, "plain");
        let hidden = spawn(&mut world, "hidden");
        world.entity_mut(hidden).insert(Hidden);
        let disabled = spawn(&mut world, "disabled");
        world.entity_mut(disabled).insert(Disabled);
        let physics = engine::physics::PhysicsWorld::default();
        let mut lua = super::LuaRuntime::new().expect("runtime");
        super::sync_lua_scene(&mut lua, &world, &physics);
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function()
                local t = {}
                for _, id in ipairs({'plain', 'hidden', 'disabled'}) do
                    t[#t + 1] = tostring(engine.is_visible(id)) .. '/' .. tostring(engine.is_enabled(id))
                end
                engine.log(table.concat(t, ' '))
            end }",
        )
        .expect("script");
        match lua.update(0.1).expect("update").as_slice() {
            [super::GameCommand::Log(text)] => {
                assert_eq!(text, "true/true false/true true/false");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn escape_quits_the_run_window_but_not_scene_view_play() {
        assert!(super::exits_on_escape(false));
        assert!(!super::exits_on_escape(true));
    }

    #[test]
    fn lua_export_markers_keep_v1_readable_and_reject_bad_v2_identity() {
        assert_eq!(super::parse_lua_marker(b"1\n"), Ok(None));
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            super::parse_lua_marker(format!("2\n{id}\n").as_bytes()),
            Ok(Some(id))
        );
        assert!(super::parse_lua_marker(b"2\nnot-a-uuid\n").is_err());
        assert!(super::parse_lua_marker(b"3\n").is_err());
    }

    #[test]
    fn lua_project_play_save_is_separate_from_exported_game_save() {
        let root = temp_dir("lua-save-path");
        let project = Project::create(&root, "Save Paths").expect("project");
        let player = load_project(&root, None).expect("test player");
        assert_eq!(player.save_path, Some(root.join(".studio/play-save.ron")));
        let id = project.manifest().project_id;
        let exported = exported_save_path(&root.join("user-data"), id);
        assert_eq!(
            exported,
            root.join("user-data/games")
                .join(id.to_string())
                .join("save.ron")
        );
        assert_ne!(player.save_path.as_deref(), Some(exported.as_path()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_player_requests_timed_frames_for_update_callbacks() {
        let root = temp_dir("lua-frame-demand");
        Project::create(&root, "Lua Frames").unwrap();
        let player = load_project(&root, None).unwrap();
        let now = std::time::Instant::now();
        assert!(matches!(
            player.frame_demand(now),
            engine::prelude::FrameDemand::AnimateUntil(deadline) if deadline > now
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_play_audio_triggers_a_scene_emitter() {
        let Ok(mut audio) = engine::audio::AudioContext::new() else {
            return; // Headless CI has no output device.
        };
        let sound = engine::audio::StaticSound::from_bytes(
            include_bytes!("../../../games/island/assets/audio/footstep.wav").to_vec(),
            false,
        )
        .expect("fixture sound");
        let id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let mut world = engine::prelude::World::new();
        engine::gameplay::spawn_audio_listener(
            &mut world,
            &engine::renderer::Camera::new(
                engine::prelude::glam::Vec3::ZERO,
                engine::prelude::glam::Vec3::NEG_Z,
                16.0 / 9.0,
            ),
        );
        let emitter = world
            .spawn((
                engine::scene::SceneObjectId(id.into()),
                engine::ecs::components::Transform::default(),
                engine::ecs::components::GlobalTransform::default(),
                engine::ecs::AudioEmitter::silent(sound),
            ))
            .id();
        apply_commands(
            &mut world,
            vec![GameCommand::PlayAudio {
                object_id: id.into(),
            }],
        );
        engine::ecs::update_audio(&mut world, &mut audio, &mut []);
        assert!(
            world
                .get::<engine::ecs::AudioEmitter>(emitter)
                .is_some_and(engine::ecs::AudioEmitter::has_started)
        );
    }

    #[test]
    fn lua_animation_command_pauses_the_target_sprite() {
        let id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let mut world = engine::prelude::World::new();
        let target = world
            .spawn((
                engine::scene::SceneObjectId(id.into()),
                engine::scene::SceneSpriteAnimation::new(engine::scene::SpriteAnimationData {
                    frames: 2,
                    fps: 8.0,
                    looping: true,
                }),
            ))
            .id();
        apply_commands(
            &mut world,
            vec![GameCommand::SetAnimationPlaying {
                object_id: id.into(),
                playing: false,
            }],
        );
        assert!(
            !world
                .get::<engine::scene::SceneSpriteAnimation>(target)
                .unwrap()
                .playing
        );
    }

    #[test]
    fn player_places_an_audio_listener_at_the_active_camera() {
        let mut world = engine::prelude::World::new();
        let eye = engine::prelude::glam::Vec3::new(3.0, 4.0, 5.0);
        let camera =
            engine::renderer::Camera::new(eye, eye + engine::prelude::glam::Vec3::X, 16.0 / 9.0);
        let listener = engine::gameplay::spawn_audio_listener(&mut world, &camera);
        assert!(world.get::<engine::ecs::AudioListener>(listener).is_some());
        assert_eq!(
            world
                .get::<engine::ecs::components::Transform>(listener)
                .expect("listener transform")
                .0
                .translation,
            eye
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::GlobalTransform>(listener)
                .expect("listener global transform")
                .0
                .translation,
            eye
        );
        let forward = world
            .get::<engine::ecs::components::Transform>(listener)
            .expect("listener transform")
            .0
            .rotation
            * engine::prelude::glam::Vec3::NEG_Z;
        assert!((forward - engine::prelude::glam::Vec3::X).length() < 1e-5);
    }

    #[test]
    fn player_presents_the_active_camera_each_frame() {
        // `GameContext` needs a GPU, so this exercises the two functions
        // `present_camera` is built from, on a plain `World`.
        use engine::ecs::components::{ActiveCamera, Camera, GlobalTransform, Transform};
        use engine::prelude::glam::Vec3;
        let mut world = engine::prelude::World::new();
        let component = engine::renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0);
        let entity = world
            .spawn((
                Transform::from(engine::prelude::Transform::from_translation(Vec3::new(
                    0.0, 1.0, 5.0,
                ))),
                Camera::from(component),
                ActiveCamera,
            ))
            .id();
        let mut eyes = Vec::new();
        for x in [0.0, 3.0] {
            world.get_mut::<Transform>(entity).unwrap().0.translation.x = x;
            engine::ecs::propagate_transforms(&mut world);
            let chosen = engine::gameplay::choose_active_camera(&mut world, &[entity]).unwrap();
            let matrix = world.get::<GlobalTransform>(chosen).unwrap().0.to_matrix();
            eyes.push(engine::gameplay::view_from_world(&component, matrix, 2.0).eye);
        }
        assert_eq!(eyes[0], Vec3::new(0.0, 1.0, 5.0));
        assert_eq!(eyes[1], Vec3::new(3.0, 1.0, 5.0));
    }

    #[test]
    fn scene_lighting_override_and_legacy_fallback_share_the_player_path() {
        let mut defaults = engine::renderer::LightSet::new();
        defaults.ambient.intensity = 0.7;
        let mut scene = Scene::default();
        let mut world = engine::prelude::World::new();
        assert_eq!(
            engine::gameplay::scene_lights(&scene, &mut world, &defaults),
            defaults
        );

        world.spawn((
            engine::ecs::components::Transform::from(engine::prelude::Transform::from_translation(
                engine::prelude::glam::Vec3::new(2.0, 3.0, 4.0),
            )),
            engine::scene::ScenePointLight(engine::scene::PointLightData::default()),
        ));
        assert_eq!(
            engine::gameplay::scene_lights(&scene, &mut world, &defaults).point[0]
                .position
                .to_array(),
            [2.0, 3.0, 4.0]
        );

        scene.lighting = Some(engine::scene::LightingData {
            sun_direction: [0.0, -1.0, 0.0],
            sun_color: [0.2, 0.4, 0.8],
            sun_intensity: 2.0,
            ambient_intensity: 0.1,
        });
        let result = engine::gameplay::scene_lights(&scene, &mut world, &defaults);
        assert_eq!(result.directional[0].color.to_array(), [0.2, 0.4, 0.8]);
        assert_eq!(result.ambient.intensity, 0.1);
    }

    #[test]
    fn scene_request_loads_a_project_scene_and_rejects_traversal() {
        let root = temp_dir("lua-change-scene-source");
        let project = Project::create(&root, "Scene Source").expect("project");
        let mut second = Scene::default();
        second.entities.push(engine::scene::SceneEntity {
            name: Some("Second".into()),
            ..Default::default()
        });
        second
            .save_to_file(&project.scenes_dir().join("second.ron"))
            .expect("second scene");
        let source = SceneSource::Project(project);
        let loaded = source.load("scenes/second.ron").expect("load scene");
        assert_eq!(loaded.entities[0].name.as_deref(), Some("Second"));
        assert!(loaded.entities[0].object_id.is_some());
        assert!(source.load("../other.ron").is_err());
        assert!(source.load("scenes\\second.ron").is_err());
        assert!(source.load("scenes//second.ron").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exported_scene_request_reads_only_its_packed_scenes() {
        let root = temp_dir("lua-change-scene-export");
        let project = Project::create(&root, "Scene Export").expect("project");
        let mut second = Scene::default();
        second.entities.push(engine::scene::SceneEntity {
            name: Some("Packed".into()),
            ..Default::default()
        });
        second
            .save_to_file(&project.scenes_dir().join("second.ron"))
            .expect("second scene");
        std::fs::create_dir_all(project.scenes_dir().join("scenes")).expect("nested scenes");
        let mut nested = Scene::default();
        nested.entities.push(engine::scene::SceneEntity {
            name: Some("Nested".into()),
            ..Default::default()
        });
        nested
            .save_to_file(&project.scenes_dir().join("scenes/third.ron"))
            .expect("nested scene");
        let export = root.join("out");
        std::fs::create_dir_all(&export).expect("export directory");
        engine::asset::pack_dir(&project.scenes_dir(), &export.join("scenes.pak"))
            .expect("pack scenes");
        let source = SceneSource::Export(export);
        let loaded = source.load("scenes/second.ron").expect("packed scene");
        assert_eq!(loaded.entities[0].name.as_deref(), Some("Packed"));
        let nested = source
            .load("scenes/scenes/third.ron")
            .expect("nested packed scene");
        assert_eq!(nested.entities[0].name.as_deref(), Some("Nested"));
        assert!(source.load("scenes/missing.ron").is_err());
        assert!(source.load("assets/scripts/main.lua").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scene_switch_preparation_restores_values_and_rejects_missing_scripts_without_mutation() {
        let root = temp_dir("lua-scene-switch-prepare");
        let project = Project::create(&root, "Scene Switch").expect("project");
        let script = project.assets_dir().join("scripts/actor.lua");
        std::fs::write(
            &script,
            "return { start = function() engine.log('scene=' .. tostring(engine.save_get('score'))) end }",
        )
        .expect("actor script");
        let mut second = Scene::default();
        second.entities.push(engine::scene::SceneEntity {
            name: Some("Second".into()),
            script: Some("assets/scripts/actor.lua".into()),
            ..Default::default()
        });
        second
            .save_to_file(&project.scenes_dir().join("second.ron"))
            .expect("second scene");
        let mut player = load_project(&root, None).expect("player");
        player
            .lua
            .as_mut()
            .expect("Lua")
            .restore_saved_data("{\"score\":7}")
            .expect("saved values");
        let (loaded, mut lua) = player
            .prepare_scene_change("scenes/second.ron")
            .expect("prepared scene");
        assert_eq!(loaded.entities[0].name.as_deref(), Some("Second"));
        assert_eq!(
            lua.start().expect("new start"),
            vec![
                GameCommand::Log("Game started".into()),
                GameCommand::Log("scene=7".into())
            ]
        );
        assert!(
            player.gameplay.scene().entities.is_empty(),
            "preparation does not replace the current scene"
        );

        std::fs::remove_file(&script).expect("remove attachment");
        assert!(player.prepare_scene_change("scenes/second.ron").is_err());
        assert!(player.gameplay.scene().entities.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scene_requests_defer_and_keep_the_first_pending_target() {
        let root = temp_dir("lua-scene-request-queue");
        Project::create(&root, "Queued Scene").expect("project");
        let mut player = load_project(&root, None).expect("player");
        player.gameplay.request_scene_change("scenes/first.ron");
        player.gameplay.request_scene_change("scenes/second.ron");
        assert_eq!(
            player.gameplay.take_scene_request().as_deref(),
            Some("scenes/first.ron")
        );
        assert_eq!(player.gameplay.take_scene_request(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scene_swap_clears_old_entities_and_bodies_but_preserves_gravity() {
        let mut world = engine::prelude::World::new();
        let parent = world.spawn(engine::ecs::components::Name::new("Old")).id();
        world.spawn((
            engine::ecs::components::Name::new("Child"),
            engine::prelude::ChildOf(parent),
        ));
        world.insert_resource(engine::scene::SceneNavGrid(
            engine::scene::NavGridData::new(2, 2, 1.0, [0.0, 0.0]),
        ));
        let mut assets = engine::renderer::RenderAssets::new();
        let resource_count = world.resource_entities().iter().count();
        let gravity = engine::prelude::glam::Vec3::new(0.0, -3.0, 0.0);
        let mut physics = engine::physics::PhysicsWorld::new(gravity);
        let _ = engine::ecs::components::RigidBody::spawn(
            &mut physics,
            engine::physics::RigidBodyBuilder::dynamic(),
            engine::physics::ColliderBuilder::ball(0.5),
        );

        engine::gameplay::clear_world(&mut world, &mut assets);
        engine::gameplay::reset_physics(&mut physics);

        assert!(
            world
                .iter_entities()
                .all(|entity| entity.get::<engine::ecs::components::Name>().is_none())
        );
        assert_eq!(world.resource_entities().iter().count(), resource_count);
        assert!(
            world
                .get_resource::<engine::scene::SceneNavGrid>()
                .is_none()
        );
        assert_eq!(physics.rapier.bodies.len(), 0);
        assert_eq!(physics.rapier.gravity, gravity);
    }

    #[test]
    fn restored_lua_values_are_visible_to_start_without_replacing_the_scene() {
        let root = temp_dir("lua-save-restore");
        let project = Project::create(&root, "Save Restore").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "return { start = function() engine.log(tostring(engine.save_get('score'))) end }",
        )
        .expect("script");
        let mut player = load_project(&root, None).expect("player");
        Game::load_data(&mut player, "{\"score\":42}");
        assert_eq!(
            player.lua.as_mut().expect("Lua").start().expect("start"),
            vec![GameCommand::Log("42".into())]
        );
        assert_eq!(Game::save_data(&player).as_deref(), Some("{\"score\":42}"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_game_values_round_trip_through_the_engine_save_file() {
        let root = temp_dir("lua-save-roundtrip");
        let project = Project::create(&root, "Save Roundtrip").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "return { start = function() local n = (engine.save_get('visits') or 0) + 1; engine.save_set('visits', n); engine.log(tostring(n)); engine.save() end }",
        )
        .expect("script");
        let mut first = load_project(&root, None).expect("first player");
        assert_eq!(
            first.lua.as_mut().expect("Lua").start().expect("start"),
            vec![GameCommand::Log("1".into()), GameCommand::RequestSave]
        );
        first.remember_lua_values();
        let mut world = engine::prelude::World::new();
        let save_path = first.save_path.as_ref().expect("play save path");
        engine::scene::SaveGame::capture(&mut world, Game::save_data(&first).expect("values"))
            .save_to_file(save_path)
            .expect("atomic save");

        let mut second = load_project(&root, None).expect("reopened player");
        let saved =
            engine::scene::SaveGame::<String>::load_from_file(save_path).expect("read save");
        Game::load_data(&mut second, &saved.game);
        assert_eq!(
            second.lua.as_mut().expect("Lua").start().expect("start"),
            vec![GameCommand::Log("2".into()), GameCommand::RequestSave]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_player_rejects_corrupt_save_without_overwriting_it() {
        let root = temp_dir("lua-corrupt-save");
        let project = Project::create(&root, "Corrupt Save").expect("project");
        let path = project.studio_dir().join("play-save.ron");
        let corrupt = b"not a save";
        std::fs::write(&path, corrupt).expect("corrupt save");
        assert!(matches!(
            load_project(&root, None),
            Err(PlayerError::InvalidSave(_, _))
        ));
        assert_eq!(std::fs::read(&path).expect("preserved"), corrupt);

        let mut world = engine::prelude::World::new();
        engine::scene::SaveGame::capture(&mut world, "{\"score\":{}}".to_owned())
            .save_to_file(&path)
            .expect("structurally valid save");
        let invalid_values = std::fs::read(&path).expect("save bytes");
        assert!(matches!(
            load_project(&root, None),
            Err(PlayerError::InvalidSave(_, _))
        ));
        assert_eq!(std::fs::read(&path).expect("preserved"), invalid_values);
        let _ = std::fs::remove_dir_all(root);
    }
    use super::*;

    #[test]
    fn player_encodes_lua_errors_as_one_diagnostic_line() {
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { start = function()\n  error('bad start')\nend }",
        )
        .expect("script");
        let error = lua.start().expect_err("callback error");
        let line = script_error_record(&error)
            .expect("serialize")
            .expect("source diagnostic");
        let payload = line
            .strip_prefix(engine::scripting::LUA_DIAGNOSTIC_PREFIX)
            .expect("prefix");
        let diagnostic: engine::scripting::LuaScriptDiagnostic =
            serde_json::from_str(payload).expect("diagnostic JSON");
        assert_eq!(diagnostic.path, "assets/scripts/main.lua");
        assert_eq!(diagnostic.line, 2);
        assert!(diagnostic.message.contains("bad start"));
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn player_forwards_keyboard_edges_to_lua() {
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function() engine.log(tostring(engine.key_down('A')) .. ',' .. tostring(engine.key_pressed('A'))) end }",
        )
        .expect("script");
        let mut input = engine::platform::InputState::new();
        input.apply_event(&engine::platform::PlatformEvent::KeyboardInput {
            key: engine::platform::KeyCode::KeyA,
            pressed: true,
            repeat: false,
        });
        update_lua_input(&mut lua, &input, false);
        assert_eq!(
            lua.update(0.1).expect("update"),
            vec![GameCommand::Log("true,true".into())]
        );
        input.end_frame();
        update_lua_input(&mut lua, &input, false);
        assert_eq!(
            lua.update(0.1).expect("update"),
            vec![GameCommand::Log("true,false".into())]
        );
    }

    #[test]
    fn authored_button_click_dispatches_its_stable_id_to_lua() {
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { on_event = function(kind, value) if kind == 'ui_clicked' then engine.log(value) end end }",
        )
        .expect("script");
        assert_eq!(
            lua.on_event("ui_clicked", "button-id")
                .expect("click event"),
            vec![GameCommand::Log("button-id".into())]
        );
    }

    #[test]
    fn lua_ui_text_command_changes_only_the_named_widget() {
        let mut ui = engine::ui::Ui::new(400.0, 200.0);
        let first = ui.add_screen(
            Default::default(),
            engine::ui::Widget::Label {
                text: "Before".into(),
                color: engine::ui::Color::WHITE,
            },
        );
        let other = ui.add_screen(
            Default::default(),
            engine::ui::Widget::Label {
                text: "Keep".into(),
                color: engine::ui::Color::WHITE,
            },
        );
        let ids = std::collections::HashMap::from([
            (first, "score-id".to_owned()),
            (other, "other-id".to_owned()),
        ]);
        assert!(engine::scene::set_authored_ui_text(
            &mut ui, &ids, "score-id", "Score: 1"
        ));
        assert!(
            matches!(&ui.node(first).unwrap().widget, engine::ui::Widget::Label { text, .. } if text == "Score: 1")
        );
        assert!(
            matches!(&ui.node(other).unwrap().widget, engine::ui::Widget::Label { text, .. } if text == "Keep")
        );
    }

    #[test]
    fn physical_contact_emits_one_enter_and_one_exit_transition() {
        let mut physics = engine::physics::PhysicsWorld::default();
        let mut world = engine::prelude::World::new();
        let (moving_body, moving_collider) = engine::ecs::components::RigidBody::spawn(
            &mut physics,
            engine::physics::RigidBodyBuilder::dynamic(),
            engine::physics::ColliderBuilder::cuboid(1.0, 1.0, 1.0),
        );
        let (_, floor_collider) = engine::ecs::components::RigidBody::spawn(
            &mut physics,
            engine::physics::RigidBodyBuilder::fixed(),
            engine::physics::ColliderBuilder::cuboid(1.0, 1.0, 1.0),
        );
        world.spawn((
            engine::scene::SceneObjectId("moving".into()),
            moving_collider,
        ));
        world.spawn((engine::scene::SceneObjectId("floor".into()), floor_collider));
        let mut contacts = engine::gameplay::ContactTracker::default();
        let event = |kind| engine::gameplay::CollisionEvent {
            kind,
            first: "floor".into(),
            second: "moving".into(),
        };
        physics.step(1.0 / 60.0).expect("step");
        assert_eq!(
            contacts.sample(&world, &physics),
            vec![event(engine::gameplay::ContactKind::Enter)]
        );
        assert!(contacts.sample(&world, &physics).is_empty());
        physics
            .rapier
            .bodies
            .get_mut(moving_body.0)
            .expect("moving body")
            .set_translation(engine::prelude::glam::Vec3::new(10.0, 0.0, 0.0), true);
        physics.step(1.0 / 60.0).expect("step");
        assert_eq!(
            contacts.sample(&world, &physics),
            vec![event(engine::gameplay::ContactKind::Exit)]
        );
    }

    #[test]
    fn lua_input_snapshot_maps_platform_state() {
        use engine::platform::{InputState, KeyCode, MouseButton, PlatformEvent};
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function() local dx, dy = engine.mouse_delta() \
             engine.log(tostring(engine.key_pressed('Q')) .. ' ' .. tostring(engine.mouse_down('right')) .. ' ' .. dx .. ' ' .. dy .. ' ' .. tostring(engine.is_mouse_locked())) end }",
        )
        .expect("script");
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyQ,
            pressed: true,
            repeat: false,
        });
        input.apply_event(&PlatformEvent::MouseButtonInput {
            button: MouseButton::Right,
            pressed: true,
        });
        input.apply_event(&PlatformEvent::MouseMotion {
            delta_x: 3.0,
            delta_y: -4.0,
        });
        update_lua_input(&mut lua, &input, true);
        assert_eq!(
            lua.update(0.1).expect("update"),
            vec![GameCommand::Log("true true 3.0 -4.0 true".into())]
        );
    }

    #[test]
    fn keyboard_events_have_stable_lua_names_and_ignore_repeat() {
        let pressed = engine::platform::PlatformEvent::KeyboardInput {
            key: engine::platform::KeyCode::KeyA,
            pressed: true,
            repeat: false,
        };
        assert_eq!(lua_key_event(&pressed), Some(("key_pressed", "A")));
        let repeated = engine::platform::PlatformEvent::KeyboardInput {
            key: engine::platform::KeyCode::KeyA,
            pressed: true,
            repeat: true,
        };
        assert_eq!(lua_key_event(&repeated), None);
        let released = engine::platform::PlatformEvent::KeyboardInput {
            key: engine::platform::KeyCode::KeyA,
            pressed: false,
            repeat: false,
        };
        assert_eq!(lua_key_event(&released), Some(("key_released", "A")));
    }

    #[test]
    fn player_binds_attached_lua_to_the_scene_object_id() {
        let root = temp_dir("lua-object-binding");
        let project = Project::create(&root, "Lua Binding").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/actor.lua"),
            "return { start = function() engine.log(engine.object_id()) end }",
        )
        .expect("script");
        let mut scene = Scene::default();
        scene.entities.push(engine::scene::SceneEntity {
            name: Some("Actor".into()),
            script: Some("assets/scripts/actor.lua".into()),
            ..Default::default()
        });
        scene
            .save_to_file(&project.main_scene_path())
            .expect("scene");
        let mut player = load_project(&root, None).expect("load");
        let id = player.gameplay.scene().entities[0]
            .object_id
            .as_ref()
            .expect("object ID");
        assert_eq!(
            player.lua.as_mut().expect("Lua").start().expect("start"),
            vec![
                GameCommand::Log("Game started".into()),
                GameCommand::Log(id.clone())
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_transform_command_moves_only_the_named_scene_object() {
        let mut world = engine::prelude::World::new();
        let first = world
            .spawn((
                engine::scene::SceneObjectId("first".into()),
                engine::ecs::components::Transform::default(),
            ))
            .id();
        let second = world
            .spawn((
                engine::scene::SceneObjectId("second".into()),
                engine::ecs::components::Transform::default(),
            ))
            .id();
        apply_commands(
            &mut world,
            vec![GameCommand::SetPosition {
                object_id: "first".into(),
                position: [2.0, 3.0, 4.0],
            }],
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::Transform>(first)
                .expect("first transform")
                .0
                .translation
                .to_array(),
            [2.0, 3.0, 4.0]
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::Transform>(second)
                .expect("second transform")
                .0
                .translation
                .to_array(),
            [0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn lua_can_raycast_during_update() {
        use engine::gameplay::GameplayQueries;
        use engine::physics::{ColliderBuilder, PhysicsWorld, RigidBodyBuilder};
        let mut physics = PhysicsWorld::default();
        let mut world = engine::prelude::World::new();
        let (body, collider) = engine::ecs::components::RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::fixed().translation([0.0, 0.0, -5.0].into()),
            ColliderBuilder::cuboid(2.0, 2.0, 0.5),
        );
        world.spawn((
            engine::scene::SceneObjectId("wall-id".into()),
            engine::ecs::components::Transform::default(),
            body,
            collider,
        ));
        physics.step(0.0).expect("refresh broad phase");
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function(dt)
               local hit = engine.raycast(vec3(0,0,0), vec3(0,0,-1), 100)
               engine.log(hit.object)
             end }",
        )
        .expect("script");
        sync_lua_scene(&mut lua, &world, &physics);
        let queries = GameplayQueries::new(&world, &physics);
        let commands = lua
            .with_queries(&queries, |lua| lua.update(0.0))
            .expect("scope")
            .expect("frame");
        assert_eq!(commands, vec![GameCommand::Log("wall-id".into())]);
    }

    #[test]
    fn lua_snapshot_includes_dynamic_velocity() {
        use engine::physics::{ColliderBuilder, PhysicsWorld, RigidBodyBuilder};
        let mut physics = PhysicsWorld::default();
        let mut world = engine::prelude::World::new();
        for (id, builder) in [
            ("ball", RigidBodyBuilder::dynamic()),
            ("wall", RigidBodyBuilder::fixed()),
        ] {
            let (body, collider) = engine::ecs::components::RigidBody::spawn(
                &mut physics,
                builder,
                ColliderBuilder::ball(0.5),
            );
            if id == "ball"
                && let Some(rb) = physics.rapier.bodies.get_mut(body.0)
            {
                rb.set_linvel([1.0_f32, 2.0, 3.0].into(), true);
            }
            world.spawn((
                engine::scene::SceneObjectId(id.into()),
                engine::ecs::components::Transform::default(),
                body,
                collider,
            ));
        }
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function(dt) engine.log(tostring(engine.get_velocity('ball'))); engine.log(tostring(engine.get_velocity('wall'))) end }",
        )
        .expect("script");
        sync_lua_scene(&mut lua, &world, &physics);
        assert_eq!(
            lua.update(0.0).expect("frame"),
            vec![
                GameCommand::Log("vec3(1, 2, 3)".into()),
                GameCommand::Log("vec3(0, 0, 0)".into())
            ]
        );
    }

    #[test]
    fn lua_scene_snapshot_reads_live_world_after_movement() {
        let mut world = engine::prelude::World::new();
        world.spawn((
            engine::scene::SceneObjectId("actor".into()),
            engine::ecs::components::Name::new("Actor"),
            engine::ecs::components::Transform::default(),
        ));
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { update = function(dt) local id = engine.find_object('Actor'); engine.log(tostring(engine.get_position(id).x == dt)) end }",
        )
        .expect("script");

        sync_lua_scene(&mut lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.update(0.0).expect("first frame"),
            vec![GameCommand::Log("true".into())]
        );
        apply_commands(
            &mut world,
            vec![GameCommand::SetPosition {
                object_id: "actor".into(),
                position: [2.0, 0.0, 0.0],
            }],
        );
        sync_lua_scene(&mut lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.update(2.0).expect("next frame"),
            vec![GameCommand::Log("true".into())]
        );
    }

    #[test]
    fn lua_and_visual_rules_share_gameplay_variables() {
        let mut world = engine::prelude::World::new();
        let mut lua = LuaRuntime::new().expect("runtime");
        lua.load_script(
            "assets/scripts/main.lua",
            "return { start = function() engine.set_var('coins', 2) end, update = function() engine.log(tostring(engine.get_var('coins'))) end }",
        )
        .expect("script");
        sync_lua_scene(&mut lua, &world, &engine::physics::PhysicsWorld::default());
        let commands = lua.start().expect("start");
        apply_commands(&mut world, commands);
        assert_eq!(
            world
                .resource::<engine::scene::RuleVariables>()
                .get("coins"),
            2.0
        );

        // A visual rule adds a coin; Lua sees it after the next sync.
        let scene = Scene {
            entities: vec![engine::scene::SceneEntity {
                rules: vec![engine::scene::RuleData::new(
                    engine::scene::RuleTrigger::Named("coin".into()),
                    engine::scene::RuleAction::AddToVariable {
                        name: "coins".into(),
                        amount: 1.0,
                    },
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut ui = engine::ui::Ui::new(800.0, 600.0);
        scene.apply_rule_event(
            engine::scene::RuleEvent::Named("coin"),
            &mut world,
            &mut ui,
            &std::collections::HashMap::new(),
        );
        sync_lua_scene(&mut lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.update(0.016).expect("update"),
            vec![GameCommand::Log("3".into())]
        );
    }

    #[test]
    fn lua_spawn_empty_creates_a_named_object_that_can_be_moved() {
        let mut world = engine::prelude::World::new();
        let id = "00000000-0000-0000-0000-000000000001";
        apply_commands(
            &mut world,
            vec![
                GameCommand::SpawnEmpty {
                    object_id: id.into(),
                    name: "Marker".into(),
                    position: [1.0, 2.0, 3.0],
                },
                GameCommand::Translate {
                    object_id: id.into(),
                    delta: [2.0, 0.0, 0.0],
                },
            ],
        );
        let entity = world
            .iter_entities()
            .find(|entity| {
                entity
                    .get::<engine::scene::SceneObjectId>()
                    .is_some_and(|object_id| object_id.0 == id)
            })
            .expect("spawned object");
        assert_eq!(
            entity
                .get::<engine::ecs::components::Name>()
                .expect("name")
                .0,
            "Marker"
        );
        assert_eq!(
            entity
                .get::<engine::ecs::components::Transform>()
                .expect("transform")
                .0
                .translation
                .to_array(),
            [3.0, 2.0, 3.0]
        );
    }

    #[test]
    fn lua_spawn_empty_rejects_an_invalid_object_id() {
        let mut world = engine::prelude::World::new();
        apply_commands(
            &mut world,
            vec![GameCommand::SpawnEmpty {
                object_id: "not-a-uuid".into(),
                name: "Marker".into(),
                position: [0.0, 0.0, 0.0],
            }],
        );
        assert!(
            world
                .iter_entities()
                .all(|entity| { entity.get::<engine::scene::SceneObjectId>().is_none() })
        );
    }

    #[test]
    fn spawned_scripted_prefab_gets_its_own_script_instance_and_start() {
        let root = temp_dir("lua-scripted-prefab");
        let project = Project::create(&root, "Scripted Prefab").expect("project");
        let prefab_dir = project.assets_dir().join("prefabs");
        std::fs::create_dir_all(&prefab_dir).expect("prefab directory");
        std::fs::write(
            project.assets_dir().join("scripts/coin.lua"),
            "return { start = function() engine.log('coin ' .. engine.object_id()) end }",
        )
        .expect("coin script");
        engine::scene::Prefab::new(engine::scene::SceneEntity {
            name: Some("Coin".into()),
            script: Some("assets/scripts/coin.lua".into()),
            ..Default::default()
        })
        .save_to_file(&prefab_dir.join("Coin.prefab"))
        .expect("save prefab");
        let mut player = load_project(&root, None).expect("player");
        let library = player.gameplay.assets().expect("assets");
        let mut world = engine::prelude::World::new();
        let id = uuid::Uuid::new_v4().to_string();
        let (_, instance) = engine::gameplay::instantiate_prefab(
            &mut world,
            library,
            &mut engine::scene::NullResolver,
            "assets/prefabs/Coin.prefab",
            &id,
            [0.0; 3],
        )
        .expect("scripted prefab spawns");
        assert_eq!(
            instance.entities[0].script.as_deref(),
            Some("assets/scripts/coin.lua")
        );

        player
            .attach_spawned_scripts(&[SpawnedObject {
                object_id: id.clone(),
                script: instance.entities[0].script.clone(),
            }])
            .expect("script attached");
        assert_eq!(player.pending_starts, VecDeque::from([id.clone()]));
        let lua = player.lua.as_mut().expect("Lua");
        assert_eq!(
            lua.start_object(&id).expect("instance start"),
            vec![GameCommand::Log(format!("coin {id}"))]
        );

        assert!(matches!(
            player.attach_spawned_scripts(&[SpawnedObject {
                object_id: uuid::Uuid::new_v4().to_string(),
                script: Some("src/coin.rs".into()),
            }]),
            Err(PlayerError::WrongScriptLanguage(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_spawn_prefab_instantiates_a_packaged_template_at_requested_position() {
        let root = temp_dir("lua-prefab-spawn");
        let project = Project::create(&root, "Lua Prefab Spawn").expect("project");
        let prefab_dir = project.assets_dir().join("prefabs");
        std::fs::create_dir_all(&prefab_dir).expect("prefab directory");
        let prefab = engine::scene::Prefab::new(engine::scene::SceneEntity {
            name: Some("Crate".into()),
            collider: Some(engine::scene::ColliderData::new(
                engine::scene::ColliderShape::Cuboid {
                    half_extents: [1.0, 1.0, 1.0],
                },
                engine::scene::BodyKind::Fixed,
            )),
            transform: Some(engine::scene::TransformData {
                translation: [9.0, 9.0, 9.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [2.0, 2.0, 2.0],
            }),
            ..Default::default()
        });
        prefab
            .save_to_file(&prefab_dir.join("Crate.prefab"))
            .expect("save prefab");
        let library = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut world = engine::prelude::World::new();
        let object_id = uuid::Uuid::new_v4().to_string();
        let (entity, instance) = engine::gameplay::instantiate_prefab(
            &mut world,
            &library,
            &mut engine::scene::NullResolver,
            "assets/prefabs/Crate.prefab",
            &object_id,
            [1.0, 2.0, 3.0],
        )
        .expect("spawn prefab");
        assert_eq!(
            world
                .get::<engine::scene::SceneObjectId>(entity)
                .map(|id| id.0.as_str()),
            Some(object_id.as_str())
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::Name>(entity)
                .map(|name| name.0.as_str()),
            Some("Crate")
        );
        let transform = world
            .get::<engine::ecs::components::Transform>(entity)
            .expect("transform");
        assert_eq!(transform.0.translation.to_array(), [1.0, 2.0, 3.0]);
        assert_eq!(transform.0.scale.to_array(), [2.0, 2.0, 2.0]);
        assert!(instance.entities[0].collider.is_some());
        assert_eq!(
            instance.entities[0].transform.map(|data| data.translation),
            Some([1.0, 2.0, 3.0])
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_spawn_prefab_rejects_untrusted_or_missing_template() {
        let root = temp_dir("lua-prefab-invalid");
        let project = Project::create(&root, "Lua Prefab Invalid").expect("project");
        let library = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut world = engine::prelude::World::new();
        let initial_entities = world.iter_entities().count();
        for path in [
            "assets/prefabs/../scripts/main.lua",
            "assets/prefabs/Missing.prefab",
        ] {
            assert!(
                engine::gameplay::instantiate_prefab(
                    &mut world,
                    &library,
                    &mut engine::scene::NullResolver,
                    path,
                    &uuid::Uuid::new_v4().to_string(),
                    [0.0, 0.0, 0.0]
                )
                .is_err(),
                "accepted {path}"
            );
        }
        assert_eq!(world.iter_entities().count(), initial_entities);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_project_spawns_a_prefab_and_queries_it_on_the_next_callback() {
        let root = temp_dir("lua-prefab-script");
        let project = Project::create(&root, "Lua Prefab Script").expect("project");
        let prefab_dir = project.assets_dir().join("prefabs");
        std::fs::create_dir_all(&prefab_dir).expect("prefab directory");
        engine::scene::Prefab::new(engine::scene::SceneEntity {
            name: Some("Crate".into()),
            ..Default::default()
        })
        .save_to_file(&prefab_dir.join("Crate.prefab"))
        .expect("save prefab");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "local id; return { start = function() id = engine.spawn_prefab('assets/prefabs/Crate.prefab', 1, 2, 3); engine.translate(id, 2, 0, 0) end, update = function() local p = engine.get_position(id); engine.log(tostring(engine.find_object('Crate') == id and p.x == 3 and p.y == 2 and p.z == 3)) end }",
        )
        .expect("script");
        let mut player = load_project(&root, None).expect("player");
        let mut world = engine::prelude::World::new();
        let lua = player.lua.as_mut().expect("Lua runtime");
        let assets = player.gameplay.assets().expect("assets");
        sync_lua_scene(lua, &world, &engine::physics::PhysicsWorld::default());
        let spawned = engine::gameplay::apply_world_commands(
            &mut world,
            &lua.start().expect("start"),
            &mut |world: &mut engine::prelude::World,
                  path: &str,
                  object_id: &str,
                  position: [f32; 3]| {
                engine::gameplay::instantiate_prefab(
                    world,
                    assets,
                    &mut engine::scene::NullResolver,
                    path,
                    object_id,
                    position,
                )
            },
        )
        .expect("commands");
        assert_eq!(spawned.len(), 1);
        sync_lua_scene(lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.update(0.1).expect("update"),
            vec![GameCommand::Log("true".into())]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn authored_scene_object_is_visible_to_lua_after_instantiation() {
        let root = temp_dir("lua-scene-query");
        let project = Project::create(&root, "Lua Scene Query").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "return { start = function() local id = engine.find_object('Door'); local p = engine.get_position(id); engine.log(tostring(p.x == 2 and p.y == 3 and p.z == 4)) end }",
        )
        .expect("script");
        let mut scene = Scene::default();
        scene.entities.push(engine::scene::SceneEntity {
            name: Some("Door".into()),
            transform: Some(engine::scene::TransformData {
                translation: [2.0, 3.0, 4.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
            }),
            ..Default::default()
        });
        scene
            .save_to_file(&project.main_scene_path())
            .expect("scene");
        let mut player = load_project(&root, None).expect("player");
        let mut world = engine::prelude::World::new();
        player
            .gameplay
            .scene()
            .instantiate_with_resolver(&mut world, &mut engine::scene::NullResolver);
        let lua = player.lua.as_mut().expect("Lua runtime");
        sync_lua_scene(lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.start().expect("start"),
            vec![GameCommand::Log("true".into())]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_project_can_spawn_and_query_an_empty_runtime_object() {
        let root = temp_dir("lua-runtime-spawn");
        let project = Project::create(&root, "Lua Runtime Spawn").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "local marker; return { start = function() marker = engine.spawn_empty('Marker', 1, 2, 3) end, update = function() local id = engine.find_object('Marker'); local p = engine.get_position(marker); engine.log(tostring(id == marker and p.x == 1 and p.y == 2 and p.z == 3)) end }",
        )
        .expect("script");
        let mut player = load_project(&root, None).expect("player");
        let mut world = engine::prelude::World::new();
        let lua = player.lua.as_mut().expect("Lua runtime");
        sync_lua_scene(lua, &world, &engine::physics::PhysicsWorld::default());
        apply_commands(&mut world, lua.start().expect("start"));
        sync_lua_scene(lua, &world, &engine::physics::PhysicsWorld::default());
        assert_eq!(
            lua.update(0.1).expect("update"),
            vec![GameCommand::Log("true".into())]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_translate_adds_to_the_existing_parent_relative_position() {
        let mut world = engine::prelude::World::new();
        let entity = world
            .spawn((
                engine::scene::SceneObjectId("actor".into()),
                engine::ecs::components::Transform::default(),
            ))
            .id();
        apply_commands(
            &mut world,
            vec![
                GameCommand::SetPosition {
                    object_id: "actor".into(),
                    position: [2.0, 3.0, 4.0],
                },
                GameCommand::Translate {
                    object_id: "actor".into(),
                    delta: [1.5, 0.0, -1.0],
                },
            ],
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::Transform>(entity)
                .expect("transform")
                .0
                .translation
                .to_array(),
            [3.5, 3.0, 3.0]
        );
        apply_commands(
            &mut world,
            vec![
                GameCommand::SetPosition {
                    object_id: "actor".into(),
                    position: [f32::MAX, 0.0, 0.0],
                },
                GameCommand::Translate {
                    object_id: "actor".into(),
                    delta: [f32::MAX, 0.0, 0.0],
                },
            ],
        );
        assert_eq!(
            world
                .get::<engine::ecs::components::Transform>(entity)
                .expect("finite transform")
                .0
                .translation
                .x,
            f32::MAX
        );
    }

    #[test]
    fn lua_player_loads_scene_attached_scripts_from_assets() {
        let root = temp_dir("lua-attachment");
        let project = Project::create(&root, "Lua Attachments").expect("project");
        std::fs::write(
            project.assets_dir().join("scripts/actor.lua"),
            "return { start = function() engine.log('actor started') end }",
        )
        .expect("script");
        let mut scene = Scene::default();
        scene.entities.push(engine::scene::SceneEntity {
            name: Some("Actor".to_owned()),
            script: Some("assets/scripts/actor.lua".to_owned()),
            ..Default::default()
        });
        scene
            .save_to_file(&project.main_scene_path())
            .expect("scene");
        scene.assign_missing_object_ids();
        let assets = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut lua = load_lua_scripts(&assets, &scene).expect("attached script loads");
        assert_eq!(
            lua.start().expect("start"),
            vec![
                GameCommand::Log("Game started".into()),
                GameCommand::Log("actor started".into()),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attaching_the_main_script_creates_a_distinct_object_instance() {
        let root = temp_dir("lua-main-attachment");
        let project = Project::create(&root, "Lua Main Attachment").expect("project");
        let mut scene = Scene {
            entities: vec![engine::scene::SceneEntity {
                script: Some("assets/scripts/main.lua".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        scene.assign_missing_object_ids();
        let assets = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut lua = load_lua_scripts(&assets, &scene).expect("load");
        assert_eq!(
            lua.start().expect("start"),
            vec![
                GameCommand::Log("Game started".into()),
                GameCommand::Log("Game started".into()),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_player_reports_missing_attached_script() {
        let root = temp_dir("lua-missing-attachment");
        let project = Project::create(&root, "Lua Attachments").expect("project");
        let assets = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut scene = Scene {
            entities: vec![engine::scene::SceneEntity {
                script: Some("assets/scripts/missing.lua".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        scene.assign_missing_object_ids();
        assert!(matches!(
            load_lua_scripts(&assets, &scene),
            Err(PlayerError::MissingLuaScript(path)) if path == "assets/scripts/missing.lua"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_player_rejects_rust_attachment() {
        let root = temp_dir("lua-rust-attachment");
        let project = Project::create(&root, "Lua Attachments").expect("project");
        let assets = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let scene = Scene {
            entities: vec![engine::scene::SceneEntity {
                script: Some("src/actor.rs".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(matches!(
            load_lua_scripts(&assets, &scene),
            Err(PlayerError::WrongScriptLanguage(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn lua_project_does_not_load_script_symlink_outside_project() {
        let root = temp_dir("lua-script-symlink");
        let project = Project::create(&root, "Lua Symlink").expect("project");
        let outside = temp_dir("outside-lua-script");
        let external = outside.join("external.lua");
        std::fs::write(&external, "return {}").expect("external source");
        std::os::unix::fs::symlink(&external, project.assets_dir().join("scripts/external.lua"))
            .expect("symlink");
        let mut scene = Scene::default();
        scene.entities.push(engine::scene::SceneEntity {
            script: Some("assets/scripts/external.lua".into()),
            ..Default::default()
        });
        scene
            .save_to_file(&project.main_scene_path())
            .expect("scene");
        assert!(matches!(
            load_project(&root, None),
            Err(PlayerError::Project(ProjectError::PathEscapesRoot(_)))
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn exported_lua_game_loads_script_from_packaged_assets() {
        let root = temp_dir("lua-export-load");
        let project = engine::project::Project::create(&root, "Lua Export").expect("project");
        let out = root.join("out");
        std::fs::create_dir_all(out.join("assets/scripts")).expect("assets");
        std::fs::copy(project.main_scene_path(), out.join(EXPORT_SCENE_FILE)).expect("scene");
        std::fs::copy(
            project.assets_dir().join("scripts/main.lua"),
            out.join("assets/scripts/main.lua"),
        )
        .expect("script");
        std::fs::write(
            out.join(engine::project::build::LUA_EXPORT_MARKER),
            engine::project::build::LUA_EXPORT_VERSION,
        )
        .expect("marker");
        let mut player = load_export(&out).expect("load exported Lua game");
        assert_eq!(
            player
                .lua
                .as_mut()
                .expect("Lua runtime")
                .start()
                .expect("start"),
            vec![GameCommand::Log("Game started".into())]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exported_lua_game_spawns_a_prefab_from_the_asset_bundle() {
        let root = temp_dir("lua-export-prefab");
        let project = Project::create(&root, "Lua Export Prefab").expect("project");
        let prefab_dir = project.assets_dir().join("prefabs");
        std::fs::create_dir_all(&prefab_dir).expect("prefab directory");
        engine::scene::Prefab::new(engine::scene::SceneEntity {
            name: Some("Bundled Crate".into()),
            ..Default::default()
        })
        .save_to_file(&prefab_dir.join("Crate.prefab"))
        .expect("save prefab");
        std::fs::write(
            project.assets_dir().join("scripts/main.lua"),
            "return { start = function() engine.spawn_prefab('assets/prefabs/Crate.prefab', 4, 5, 6) end }",
        )
        .expect("script");
        let out = root.join("out");
        std::fs::create_dir_all(&out).expect("export directory");
        std::fs::copy(project.main_scene_path(), out.join(EXPORT_SCENE_FILE)).expect("scene");
        engine::asset::pack_dir(&project.assets_dir(), &out.join(engine::scene::BUNDLE_FILE))
            .expect("asset bundle");
        std::fs::write(
            out.join(engine::project::build::LUA_EXPORT_MARKER),
            engine::project::build::LUA_EXPORT_VERSION,
        )
        .expect("marker");
        let mut player = load_export(&out).expect("export");
        let assets = player.gameplay.assets().expect("bundled assets");
        let lua = player.lua.as_mut().expect("Lua runtime");
        let mut world = engine::prelude::World::new();
        let spawned = engine::gameplay::apply_world_commands(
            &mut world,
            &lua.start().expect("start"),
            &mut |world: &mut engine::prelude::World,
                  path: &str,
                  object_id: &str,
                  position: [f32; 3]| {
                engine::gameplay::instantiate_prefab(
                    world,
                    assets,
                    &mut engine::scene::NullResolver,
                    path,
                    object_id,
                    position,
                )
            },
        )
        .expect("prefab command");
        assert_eq!(spawned.len(), 1);
        assert_eq!(
            world
                .get::<engine::ecs::components::Name>(spawned[0].0)
                .map(|name| name.0.as_str()),
            Some("Bundled Crate")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_project_starter_loads_without_cargo() {
        let root = temp_dir("lua-project");
        let project = engine::project::Project::create(&root, "Lua Starter").expect("project");
        let assets = AssetLibrary::index(&project.assets_dir()).expect("assets");
        let mut lua = load_lua_scripts(&assets, &Scene::default()).expect("Lua loads");
        assert_eq!(
            lua.start().expect("start"),
            vec![engine::scripting::GameCommand::Log("Game started".into())]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_project_opens_as_a_playable_player() {
        let root = temp_dir("lua-play");
        engine::project::Project::create(&root, "Lua Starter").expect("project");
        let player = load_project(&root, None).expect("load project without Cargo");
        assert!(player.lua.is_some());
        assert!(player.gameplay.assets().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_player_uses_the_selected_scene() {
        let root = temp_dir("lua-selected-scene");
        let project = Project::create(&root, "Lua Scenes").expect("project");
        let selected = root.join("scenes/second.ron");
        let mut scene = Scene::default();
        scene.entities.push(engine::scene::SceneEntity {
            name: Some("Second Scene".into()),
            ..Default::default()
        });
        scene.save_to_file(&selected).expect("scene");
        let player = load_project(&root, Some(Path::new("scenes/second.ron")))
            .expect("selected scene loads");
        assert_eq!(
            player.gameplay.scene().entities[0].name.as_deref(),
            Some("Second Scene")
        );
        assert!(matches!(
            load_project(&root, Some(Path::new("../outside.ron"))),
            Err(PlayerError::Project(ProjectError::PathEscapesRoot(_)))
        ));
        let _ = std::fs::remove_dir_all(project.root());
    }

    /// Applies world commands with no asset package; prefab requests fail.
    fn apply_commands(world: &mut engine::prelude::World, commands: Vec<GameCommand>) {
        engine::gameplay::apply_world_commands(world, &commands, &mut |_, path, _, _| {
            Err(GameplayError::Prefab(path.into(), "no assets".into()))
        })
        .expect("command batch");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vge-player-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn default_export_dir_is_next_to_the_executable() {
        let expected = engine::prelude::executable_dir().unwrap_or_else(|| PathBuf::from("."));

        assert_eq!(default_export_dir(), expected);
    }

    #[test]
    fn missing_scene_is_a_clean_error() {
        let dir = temp_dir("empty");
        assert!(matches!(
            load_scene(&dir),
            Err(PlayerError::MissingScene(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_scene_is_an_error_not_a_panic() {
        let dir = temp_dir("bad");
        std::fs::write(dir.join("main.ron"), "this is not RON at all {{{").expect("write");
        assert!(matches!(load_scene(&dir), Err(PlayerError::Scene(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loose_directory_is_used_when_there_is_no_pak() {
        let dir = temp_dir("loose");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/thing.gltf"), b"{}").expect("write");
        std::fs::write(
            dir.join("assets/thing.gltf.meta"),
            "(id: \"b8d5d33b-086e-4494-b2bd-0cb556828369\", source_hash: 1)",
        )
        .expect("write");

        let library = open_assets(&dir).expect("a loose dir is a valid source");
        assert_eq!(library.len(), 1);
        assert!(
            library
                .path_for("b8d5d33b-086e-4494-b2bd-0cb556828369")
                .is_some(),
            "a loose asset must resolve to a path, so external references work",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pak_falls_back_to_loose_files_instead_of_failing() {
        let dir = temp_dir("corruptpak");
        std::fs::write(dir.join("assets.pak"), b"not a bundle at all").expect("write");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");

        assert!(
            open_assets(&dir).is_some(),
            "an unreadable bundle must fall back, not give up",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_assets_at_all_is_none_not_a_panic() {
        let dir = temp_dir("noassets");
        assert!(open_assets(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_runs_scene_start_and_key_rules() {
        // The window-bound hooks (`start_rules` after each start, `update_rules`
        // in `update`) cannot run without a GPU context; this covers what the
        // player feeds them: every key name a rule can use maps to one
        // distinct platform key, and such rules load.
        let mut seen = std::collections::HashSet::new();
        for key in ScriptKey::ALL {
            assert!(seen.insert(platform_key(*key)), "{} maps twice", key.name());
        }
        let scene = Scene {
            entities: vec![engine::scene::SceneEntity {
                object_id: Some("00000000-0000-4000-8000-0000000000a1".into()),
                rules: vec![
                    engine::scene::RuleData::new(
                        engine::scene::RuleTrigger::SceneStart,
                        engine::scene::RuleAction::SetGameState {
                            state: engine::scene::GameStateName::Menu,
                        },
                    ),
                    engine::scene::RuleData::new(
                        engine::scene::RuleTrigger::KeyPressed {
                            key: "Space".into(),
                        },
                        engine::scene::RuleAction::SetGameState {
                            state: engine::scene::GameStateName::Playing,
                        },
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        scene.validate().expect("valid rules");
        let mut world = engine::prelude::World::new();
        scene.instantiate(&mut world);
        let mut ui = engine::ui::Ui::new(800.0, 600.0);
        let nodes = std::collections::HashMap::new();
        let start = scene.apply_rule_event(
            engine::scene::RuleEvent::SceneStart,
            &mut world,
            &mut ui,
            &nodes,
        );
        assert_eq!(start.deferred.len(), 1);
        let key = scene.apply_rule_event(
            engine::scene::RuleEvent::KeyPressed("Space"),
            &mut world,
            &mut ui,
            &nodes,
        );
        assert_eq!(key.deferred.len(), 1);
    }
}
