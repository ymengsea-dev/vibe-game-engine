//! The gameplay runtime shared by Lua and Rust games.
//!
//! A game describes *what* should happen as [`GameCommand`]s — move this
//! object, spawn that prefab, play a sound, change a caption, send an event,
//! switch scene — and [`GameplayRuntime`] is the one place that turns them
//! into world, physics, UI, audio and scene changes. Lua scripts produce the
//! commands through `engine.*`; Rust code constructs them directly. Either
//! way the same validation, ordering and visual-rule dispatch apply.
//!
//! ```no_run
//! # use engine::prelude::*;
//! # use engine::gameplay::GameplayRuntime;
//! struct MyGame { gameplay: GameplayRuntime }
//!
//! impl Game for MyGame {
//!     fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
//!         self.gameplay.setup(ctx)
//!     }
//!     fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
//!         self.gameplay.begin_frame(ctx);
//!         self.gameplay.dispatch_ui_clicks(ctx);
//!         self.gameplay.present_camera(ctx, dt);
//!     }
//!     fn post_fixed_update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
//!         for contact in self.gameplay.sample_collisions(ctx) {
//!             tracing::info!(?contact, "contact");
//!         }
//!     }
//! }
//! ```

mod bodies;
mod camera;
mod collisions;
mod commands;
mod keys;
pub(crate) mod objects;
mod prefab;
mod presentation;
mod rule_runtime;
mod scenes;
mod state;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use engine_ecs::prelude::Entity;
use engine_renderer::{Camera, LightSet};
use engine_scene::{AssetLibrary, GameStateName, RuleEvent, RuntimeResolver, Scene, SceneError};
use engine_scripting::GameCommand;
use engine_ui::NodeId;

use crate::app::{GameContext, GameError};

pub use bodies::GameplayQueries;
pub use camera::{choose_active_camera, view_from_world};
pub use collisions::{CollisionEvent, ContactKind, ContactTracker};
pub use commands::{PrefabSpawner, apply_world_commands, find_object, find_objects_by_tag};
pub use keys::platform_key;
pub use prefab::{instantiate_prefab, refresh_prefab_copies, valid_prefab_path};
pub use presentation::{scene_lights, spawn_audio_listener};
pub use scenes::{
    SCENE_BUNDLE_FILE, SceneSource, clear_world, kept_object_ids, kept_objects,
    kept_script_objects, reset_physics, valid_scene_path,
};

/// Why a gameplay request could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum GameplayError {
    /// A scene file was unreadable or invalid.
    #[error(transparent)]
    Scene(#[from] SceneError),
    /// A `change_scene` target could not be opened.
    #[error("scene {0} cannot be opened: {1}")]
    SceneRequest(String, String),
    /// This game was started without a [`SceneSource`], so it cannot switch scenes.
    #[error("scene {0} cannot be opened: this game has no scene source")]
    NoSceneSource(String),
    /// A runtime prefab path, package or file cannot be used.
    #[error("prefab {0} is invalid or missing: {1}")]
    Prefab(String, String),
    /// Setting up the scene's renderables, colliders or sprites failed.
    #[error(transparent)]
    Setup(#[from] GameError),
}

impl From<GameplayError> for GameError {
    fn from(error: GameplayError) -> Self {
        match error {
            GameplayError::Setup(inner) => inner,
            other => GameError::Setup(other.to_string()),
        }
    }
}

/// Stores values the player changed with sliders and toggles in their
/// variables, then makes every bound element show its variable's value.
fn store_ui_changes(world: &mut engine_ecs::prelude::World, ui: &mut engine_ui::Ui) {
    for change in ui.take_changes() {
        let Some(name) = ui
            .node(change.node)
            .and_then(|node| node.variable.as_deref())
        else {
            continue;
        };
        // Back to a tidy decimal, so a slider at 0.1 reads 0.1 in `{volume}`.
        let value = (f64::from(change.value) * 1e6).round() / 1e6;
        let mut variables = world.get_resource_or_insert_with(engine_scene::RuleVariables::default);
        if let Err(error) = variables.set(name, value) {
            tracing::warn!(variable = name, %error, "a UI element could not change a variable");
        }
    }
    if let Some(variables) = world.get_resource::<engine_scene::RuleVariables>() {
        engine_scene::sync_ui_variables(ui, variables);
    }
}

/// An object created by [`GameplayRuntime::apply`] from a prefab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedObject {
    /// The new object's stable scene ID.
    pub object_id: String,
    /// The prefab's attached script, for the host to load (`None` if unscripted).
    pub script: Option<String>,
}

/// A queued change to what is loaded on top of the current scene.
enum SceneLoad {
    Load(String),
    Unload(String),
}

/// Runs an authored scene: instantiation, gameplay commands, visual rules,
/// collision events and scene changes.
pub struct GameplayRuntime {
    scene: Scene,
    source: Option<SceneSource>,
    assets: Option<AssetLibrary>,
    ui_nodes: HashMap<NodeId, String>,
    contacts: ContactTracker,
    /// Physics commands already warned about, per scene.
    warned: HashSet<(String, &'static str)>,
    /// Last `move_character` ground result per object, per scene.
    grounded: HashMap<String, bool>,
    pending_scene: Option<String>,
    initial_camera: Option<Camera>,
    /// The camera object the game views through (see `present_camera`).
    active_camera: Option<Entity>,
    /// The camera shake in progress.
    shake: Option<camera::Shake>,
    /// Whether a follow camera last asked for the mouse to be locked.
    follow_mouse: bool,
    /// Random source for shakes, seeded once per scene.
    shake_rng: engine_utils::Rng,
    /// The scene's entities in scene order, for choosing the camera.
    scene_order: Vec<Entity>,
    default_lights: Option<LightSet>,
    /// Audio bus the scene's ambient sounds play on, created once per game.
    ambient_bus: Option<usize>,
    /// The last `(old, new)` game state change not yet taken by the host.
    state_change: Option<(GameStateName, GameStateName)>,
    /// Timer progress and last seen variables/state for rule triggers.
    rule_clock: rule_runtime::RuleClock,
    /// Nesting of rule actions that apply commands.
    rule_guard: rule_runtime::ApplyGuard,
    /// Prefabs created by rule actions, until the host takes them.
    rule_spawns: Vec<SpawnedObject>,
    /// Object IDs removed by `Destroy` since the host last took them.
    destroyed: Vec<String>,
    /// Load and unload requests waiting for the next safe point of a frame.
    scene_loads: VecDeque<SceneLoad>,
    /// The scenes loaded on top of the current one, with their objects' IDs.
    loaded_scenes: Vec<(String, HashSet<String>)>,
    /// The path of the current scene, when known. `None` means the start scene.
    scene_path: Option<String>,
    /// The loading screen scene, once looked for.
    loading_screen: Option<Option<Scene>>,
    /// A scene change waiting until its loading screen has been drawn.
    change_after_loading: Option<String>,
    /// The loading screen's nodes, while it is up.
    loading_nodes: Vec<NodeId>,
    /// Scene data of the objects kept from the last scene, until `setup` adds
    /// it to the new scene.
    carried: Vec<engine_scene::SceneEntity>,
    /// The live objects kept from the last scene.
    carried_objects: Vec<Entity>,
    /// Their IDs: their start rules have already run.
    carried_ids: HashSet<String>,
}

impl GameplayRuntime {
    /// A runtime for `scene`, resolving its assets from `assets` when present.
    /// Without assets the scene still runs, but entities have no geometry.
    pub fn new(mut scene: Scene, assets: Option<AssetLibrary>) -> Self {
        if let Some(library) = &assets {
            refresh_prefab_copies(&mut scene, library);
        }
        Self {
            scene,
            source: None,
            assets,
            ui_nodes: HashMap::new(),
            contacts: ContactTracker::default(),
            warned: HashSet::new(),
            grounded: HashMap::new(),
            pending_scene: None,
            initial_camera: None,
            active_camera: None,
            shake: None,
            follow_mouse: false,
            shake_rng: engine_utils::Rng::new(camera::SHAKE_SEED),
            scene_order: Vec::new(),
            default_lights: None,
            ambient_bus: None,
            state_change: None,
            rule_clock: rule_runtime::RuleClock::default(),
            rule_guard: rule_runtime::ApplyGuard::default(),
            rule_spawns: Vec::new(),
            destroyed: Vec::new(),
            scene_loads: VecDeque::new(),
            loaded_scenes: Vec::new(),
            scene_path: None,
            loading_screen: None,
            change_after_loading: None,
            loading_nodes: Vec::new(),
            carried: Vec::new(),
            carried_objects: Vec::new(),
            carried_ids: HashSet::new(),
        }
    }

    /// World queries (ray casts, grounded checks) for the current frame.
    ///
    /// Hand these to `LuaRuntime::with_queries`, or call them from Rust game code.
    pub fn queries<'a>(&'a self, ctx: &'a GameContext<'_>) -> GameplayQueries<'a> {
        GameplayQueries {
            world: ctx.world(),
            physics: ctx.physics(),
            grounded: Some(&self.grounded),
        }
    }

    /// Says which scene file this runtime starts on, for going to the next
    /// scene. Without it the project's start scene is assumed.
    pub fn with_scene_path(mut self, path: impl Into<String>) -> Self {
        self.scene_path = Some(path.into());
        self
    }

    /// Enables scene changes, reading requested scenes from `source`.
    pub fn with_scene_source(mut self, source: SceneSource) -> Self {
        self.source = Some(source);
        self
    }

    /// The scene currently running.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The asset library scenes and prefabs resolve against.
    pub fn assets(&self) -> Option<&AssetLibrary> {
        self.assets.as_ref()
    }

    /// Mounted UI nodes and the scene object IDs they belong to.
    pub fn ui_nodes(&self) -> &HashMap<NodeId, String> {
        &self.ui_nodes
    }

    /// Instantiates the current scene: camera, lights, audio listener,
    /// ambient sounds, renderables, colliders, sprite atlas and authored UI.
    pub fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        let live_camera = *ctx.camera_mut();
        let initial = *self.initial_camera.get_or_insert(live_camera);
        let mut camera = initial;
        camera.aspect_ratio = live_camera.aspect_ratio;
        *ctx.camera_mut() = camera;
        if self.default_lights.is_none() {
            self.default_lights = Some(ctx.lights_mut().clone());
        }

        if self.ambient_bus.is_none() && !self.scene.environment.is_empty() {
            self.ambient_bus = ctx.add_bus(0.0);
        }
        let ambient_bus = self.ambient_bus;
        let report = {
            let parts = ctx.scene_parts();
            match &self.assets {
                Some(library) => {
                    let mut resolver =
                        RuntimeResolver::new(library, parts.gpu, parts.pipeline, parts.assets);
                    let report = self
                        .scene
                        .instantiate_with_resolver(parts.world, &mut resolver);
                    if let Some(bus) = ambient_bus {
                        engine_scene::spawn_ambient_sounds(
                            &self.scene.environment,
                            parts.world,
                            &mut resolver,
                            bus,
                        );
                    }
                    report
                }
                None => {
                    tracing::warn!("no asset source; entities will spawn without geometry");
                    self.scene
                        .instantiate_with_resolver(parts.world, &mut engine_scene::NullResolver)
                }
            }
        };
        tracing::info!(
            entities = report.spawned.len(),
            unresolved = report.unresolved,
            "scene instantiated"
        );
        if report.unresolved > 0 {
            tracing::warn!(
                unresolved = report.unresolved,
                "some entities are missing their geometry; the rest of the scene still runs"
            );
        }

        // The scene's camera object now exists: view through it.
        self.active_camera = None;
        self.shake = None;
        self.shake_rng = engine_utils::Rng::new(camera::SHAKE_SEED);
        // A camera the new scene brings wins over a kept one.
        self.scene_order = report.spawned.clone();
        self.scene_order.append(&mut self.carried_objects);
        self.present_camera(ctx, 0.0);
        let camera = *ctx.camera_mut();
        spawn_audio_listener(ctx.world_mut(), &camera);
        if let Some(library) = &self.assets {
            ctx.spawn_scene_colliders(&self.scene, &report, library);
            ctx.set_scene_sprite_atlas(library)?;
        }
        // An authored Disabled parent disables its children in the game.
        {
            let (world, physics, _, _) = ctx.object_parts();
            objects::disable_authored_subtrees(world, physics, &report.spawned);
        }
        // Kept objects join the scene data only now, so they get no second
        // collider, but their rules, UI and lights count from here on.
        let offset = self.scene.entities.len();
        self.scene
            .entities
            .extend(self.carried.drain(..).map(|mut entity| {
                entity.parent = entity.parent.map(|parent| parent + offset);
                entity
            }));
        self.refresh_lights(ctx);
        self.ui_nodes = self.scene.mount_ui(ctx.ui_mut());
        if let Some(library) = &self.assets {
            ctx.set_scene_ui_images(&self.scene, library);
        }
        // Variables survive a scene change, so bars start out right.
        self.sync_ui(ctx);
        let start = self.scene.environment.start_state;
        state::init_state(ctx.world_mut(), start);
        ctx.set_simulation_paused(state::simulation_paused(ctx.world()));
        // Timers restart with the scene; variables and state already in force
        // count as seen, so only later changes raise change rules.
        let current = state::current(ctx.world());
        self.rule_clock.reset(
            &self.scene,
            ctx.world().get_resource::<engine_scene::RuleVariables>(),
            current,
        );
        Ok(())
    }

    /// The current game state (`playing` before the first scene is set up).
    pub fn game_state(&self, world: &engine_ecs::prelude::World) -> GameStateName {
        state::current(world)
    }

    /// The last `(old, new)` game state change since the previous call, for the
    /// host to deliver as `on_state_changed`. Changes made in one batch report
    /// only the last.
    pub fn take_state_change(&mut self) -> Option<(GameStateName, GameStateName)> {
        self.state_change.take()
    }

    /// Applies `SetGameState` commands and merges the net change into the one
    /// pending for the host (first `old`, last `new`; none if they match).
    fn record_state_changes(
        &mut self,
        world: &mut engine_ecs::prelude::World,
        commands: &[GameCommand],
    ) -> Option<(GameStateName, GameStateName)> {
        let change = state::apply_state_commands(world, commands);
        self.state_change = state::merge_change(self.state_change, change);
        self.state_change
    }

    /// Per-frame housekeeping: applies a pending scene change, then refreshes
    /// lights so moved point lights follow their objects. Call at the start of
    /// `update`. A failed scene change is logged and the current scene keeps running.
    pub fn begin_frame(&mut self, ctx: &mut GameContext<'_>) {
        let path = self.change_after_loading.take().or_else(|| {
            let path = self.pending_scene.take()?;
            // Drawn this frame; the change happens at the start of the next.
            if self.load_scene(&path).is_ok() && self.show_loading_screen(ctx, &path) {
                self.change_after_loading = Some(path);
                return None;
            }
            Some(path)
        });
        if let Some(path) = path {
            match self.change_scene(ctx, &path) {
                // The new scene's start rules fire once it is set up.
                Ok(()) => self.start_rules(ctx),
                Err(error) => {
                    self.hide_loading_screen(ctx);
                    tracing::error!(scene = %path, %error, "scene change failed");
                }
            }
        }
        self.apply_scene_loads(ctx);
        self.refresh_lights(ctx);
    }

    /// Puts up the loading screen, the Game UI of [`engine_project::LOADING_SCENE`],
    /// on top of everything, if the project has one and `target` isn't it.
    /// Returns whether it is up, so the caller changes scene on the next frame.
    pub fn show_loading_screen(&mut self, ctx: &mut GameContext<'_>, target: &str) -> bool {
        if target == engine_project::LOADING_SCENE {
            return false;
        }
        if self.loading_screen.is_none() {
            self.loading_screen = Some(self.load_scene(engine_project::LOADING_SCENE).ok());
        }
        let Some(Some(screen)) = &self.loading_screen else {
            return false;
        };
        let nodes = screen.append_ui(ctx.ui_mut());
        if nodes.is_empty() {
            return false;
        }
        self.loading_nodes = nodes.into_keys().collect();
        if let Some(library) = &self.assets {
            ctx.set_scene_ui_images(screen, library);
        }
        true
    }

    /// Takes the loading screen down after a scene change that failed.
    pub fn hide_loading_screen(&mut self, ctx: &mut GameContext<'_>) {
        for node in self.loading_nodes.drain(..) {
            ctx.ui_mut().set_visible(node, false);
        }
        if let Some(library) = &self.assets {
            ctx.set_scene_ui_images(&self.scene, library);
        }
    }

    /// Queues the current scene's successor in the project's scene order.
    /// After the last scene this warns and does nothing.
    pub fn request_next_scene(&mut self) {
        let Some(source) = &self.source else {
            tracing::warn!("next scene: this game has no scene source");
            return;
        };
        let order = source.scene_order();
        let current = self.scene_path.as_deref().unwrap_or(&order.start);
        match scenes::scene_after(&order.order, current) {
            Some(next) => self.request_scene_change(&next),
            None => tracing::warn!(scene = %current, "next scene: this is the last scene"),
        }
    }

    /// Queues `path` to be loaded on top of the current scene at the next
    /// [`Self::apply_scene_loads`].
    pub fn request_scene_load(&mut self, path: &str) {
        self.scene_loads.push_back(SceneLoad::Load(path.to_owned()));
    }

    /// Queues a scene loaded on top to be removed at the next
    /// [`Self::apply_scene_loads`].
    pub fn request_scene_unload(&mut self, path: &str) {
        self.scene_loads
            .push_back(SceneLoad::Unload(path.to_owned()));
    }

    /// The scenes loaded on top of the current one, oldest first.
    pub fn loaded_scenes(&self) -> impl Iterator<Item = &str> {
        self.loaded_scenes.iter().map(|(path, _)| path.as_str())
    }

    /// Carries out queued scene loads and unloads. [`Self::begin_frame`] calls
    /// it. A problem is logged and the game goes on. The scripts of what was
    /// loaded come out of [`Self::take_rule_spawns`], and of what was removed
    /// out of [`Self::take_destroyed`].
    pub fn apply_scene_loads(&mut self, ctx: &mut GameContext<'_>) {
        while let Some(request) = self.scene_loads.pop_front() {
            match request {
                SceneLoad::Load(path) => {
                    if let Err(error) = self.load_scene_on_top(ctx, &path) {
                        tracing::error!(scene = %path, %error, "scene could not be loaded");
                    }
                }
                SceneLoad::Unload(path) => self.unload_scene(ctx, &path),
            }
        }
    }

    fn load_scene_on_top(
        &mut self,
        ctx: &mut GameContext<'_>,
        path: &str,
    ) -> Result<(), GameplayError> {
        let is_open = self.scene_path.as_deref() == Some(path)
            || self.loaded_scenes.iter().any(|(open, _)| open == path);
        if is_open {
            tracing::warn!(scene = %path, "this scene is already loaded");
            return Ok(());
        }
        let mut scene = self.load_scene(path)?;
        // Whatever is already alive isn't made twice: a kept object, say.
        let alive: HashSet<String> = scene
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.clone())
            .filter(|id| find_object(ctx.world(), id).is_some())
            .collect();
        scene.remove_objects(&alive);
        let existing: HashSet<String> = self
            .scene
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.clone())
            .collect();
        let report = {
            let parts = ctx.scene_parts();
            match &self.assets {
                Some(library) => {
                    let mut resolver =
                        RuntimeResolver::new(library, parts.gpu, parts.pipeline, parts.assets);
                    scene.instantiate_with_resolver(parts.world, &mut resolver)
                }
                None => {
                    scene.instantiate_with_resolver(parts.world, &mut engine_scene::NullResolver)
                }
            }
        };
        if let Some(library) = &self.assets {
            ctx.spawn_scene_colliders(&scene, &report, library);
            ctx.set_scene_sprite_atlas(library)
                .map_err(|error| GameplayError::SceneRequest(path.to_owned(), error.to_string()))?;
        }
        {
            let (world, physics, _, _) = ctx.object_parts();
            objects::disable_authored_subtrees(world, physics, &report.spawned);
        }
        self.ui_nodes.extend(scene.append_ui(ctx.ui_mut()));
        self.scene_order.extend(report.spawned.iter().copied());
        let ids = scene
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.clone())
            .collect();
        self.loaded_scenes.push((path.to_owned(), ids));
        self.rule_spawns
            .extend(scene.entities.iter().filter_map(|data| {
                Some(SpawnedObject {
                    object_id: data.object_id.clone()?,
                    script: data.script.clone(),
                })
            }));
        let offset = self.scene.entities.len();
        self.scene
            .entities
            .extend(scene.entities.into_iter().map(|mut entity| {
                entity.parent = entity.parent.map(|parent| parent + offset);
                entity
            }));
        if let Some(library) = &self.assets {
            ctx.set_scene_ui_images(&self.scene, library);
        }
        self.sync_ui(ctx);
        self.rule_clock.watch(
            &self.scene,
            ctx.world().get_resource::<engine_scene::RuleVariables>(),
        );
        self.run_rule_event(ctx, RuleEvent::SceneStartExcept(&existing));
        Ok(())
    }

    fn unload_scene(&mut self, ctx: &mut GameContext<'_>, path: &str) {
        let Some(at) = self.loaded_scenes.iter().position(|(open, _)| open == path) else {
            tracing::warn!(scene = %path, "this scene is not loaded on top");
            return;
        };
        let (_, mut ids) = self.loaded_scenes.remove(at);
        ids.retain(|id| !kept_object_ids(ctx.world()).contains(id));
        let commands = ids
            .iter()
            .filter(|id| find_object(ctx.world(), id).is_some())
            .map(|id| GameCommand::Destroy {
                object_id: id.clone(),
            })
            .collect();
        if let Err(error) = self.apply(ctx, commands) {
            tracing::warn!(scene = %path, %error, "scene could not be unloaded cleanly");
        }
        self.scene.remove_objects(&ids);
        if let Some(library) = &self.assets {
            ctx.set_scene_ui_images(&self.scene, library);
        }
    }

    /// Re-applies scene lighting plus the current positions of point lights.
    pub fn refresh_lights(&mut self, ctx: &mut GameContext<'_>) {
        if let Some(defaults) = &self.default_lights {
            let lights = scene_lights(&self.scene, ctx.world_mut(), defaults);
            *ctx.lights_mut() = lights;
        }
    }

    /// Applies a batch of commands in a fixed order: UI captions, world changes
    /// (moves, spawns, audio, animation, variables), then visual-rule events,
    /// then the scene-change request. Spawned prefabs get colliders and sprites;
    /// moved bodies are teleported before the next physics step.
    ///
    /// Returns the prefab instances created, so a host can attach their scripts.
    pub fn apply(
        &mut self,
        ctx: &mut GameContext<'_>,
        commands: Vec<GameCommand>,
    ) -> Result<Vec<SpawnedObject>, GameplayError> {
        for command in &commands {
            if let GameCommand::SetMouseLocked { locked } = command {
                ctx.set_mouse_locked(*locked);
            }
            if let GameCommand::UiSetText { object_id, text } = command
                && !engine_scene::set_authored_ui_text(
                    ctx.ui_mut(),
                    &self.ui_nodes,
                    object_id,
                    text,
                )
            {
                tracing::warn!(%object_id, "UI text target was not found");
            }
        }

        let spawned = {
            let parts = ctx.scene_parts();
            let assets = self.assets.as_ref();
            let mut resolver = assets.map(|library| {
                RuntimeResolver::new(library, parts.gpu, parts.pipeline, parts.assets)
            });
            let mut spawner = |world: &mut _, path: &str, object_id: &str, position| {
                let (Some(library), Some(resolver)) = (assets, resolver.as_mut()) else {
                    return Err(GameplayError::Prefab(
                        path.to_owned(),
                        "asset package is missing".into(),
                    ));
                };
                instantiate_prefab(world, library, resolver, path, object_id, position)
            };
            apply_world_commands(parts.world, &commands, &mut spawner)?
        };
        let spawned_objects = spawned
            .iter()
            .flat_map(|(_, scene)| &scene.entities)
            .filter_map(|data| {
                Some(SpawnedObject {
                    object_id: data.object_id.clone()?,
                    script: data.script.clone(),
                })
            })
            .collect();
        if let Some(library) = &self.assets
            && !spawned.is_empty()
        {
            let has_sprites = spawned
                .iter()
                .any(|(_, scene)| scene.entities.iter().any(|entity| entity.sprite.is_some()));
            for (entities, mut scene) in spawned {
                for (&entity, data) in entities.iter().zip(&mut scene.entities) {
                    if let Some(transform) =
                        ctx.world().get::<engine_ecs::components::Transform>(entity)
                    {
                        data.transform = Some(transform.0.into());
                    }
                }
                let report = engine_scene::InstantiateReport {
                    spawned: entities,
                    unresolved: 0,
                };
                ctx.spawn_scene_colliders(&scene, &report, library);
                let (world, physics, _, _) = ctx.object_parts();
                objects::disable_authored_subtrees(world, physics, &report.spawned);
            }
            if has_sprites {
                ctx.set_scene_sprite_atlas(library)?;
            }
        }
        let poses = commands::moved_body_poses(ctx.world(), &commands::moved_object_ids(&commands));
        commands::sync_moved_bodies(ctx.physics_mut(), &poses);

        // Body commands run last, once spawned bodies exist and teleports are
        // applied (teleports leave velocity alone).
        let fixed_step = ctx.fixed_step_seconds();
        let (world, physics) = ctx.world_and_physics_mut();
        bodies::apply_body_commands(
            world,
            physics,
            &mut self.warned,
            &mut self.grounded,
            &commands,
            fixed_step,
        );
        camera::apply_camera_commands(
            ctx.world_mut(),
            &mut self.active_camera,
            &mut self.shake,
            &mut self.warned,
            &commands,
        );
        {
            let (world, physics, ui, assets) = ctx.object_parts();
            objects::apply_object_commands(
                objects::ObjectTargets { world, physics, ui },
                objects::ObjectState {
                    ui_nodes: &mut self.ui_nodes,
                    contacts: &mut self.contacts,
                    warned: &mut self.warned,
                    grounded: &mut self.grounded,
                    scene_order: &mut self.scene_order,
                    active_camera: &mut self.active_camera,
                    destroyed: &mut self.destroyed,
                },
                &mut |world, entity| scenes::release_entity_assets(world, entity, assets),
                &commands,
            );
        }

        self.record_state_changes(ctx.world_mut(), &commands);
        // The pause starts this frame, not the next.
        ctx.set_simulation_paused(state::simulation_paused(ctx.world()));

        for command in &commands {
            match command {
                GameCommand::EmitEvent { name } => {
                    self.send_event(ctx, name);
                }
                GameCommand::ChangeScene { path } => self.request_scene_change(path),
                GameCommand::LoadScene { path } => self.request_scene_load(path),
                GameCommand::UnloadScene { path } => self.request_scene_unload(path),
                GameCommand::NextScene => self.request_next_scene(),
                GameCommand::RequestSave => ctx.request_save(),
                _ => {}
            }
        }
        Ok(spawned_objects)
    }

    /// Runs visual rules for a named event; returns how many actions ran.
    pub fn send_event(&mut self, ctx: &mut GameContext<'_>, name: &str) -> usize {
        self.run_rule_event(ctx, RuleEvent::Named(name))
    }

    /// Runs the rules for `event`, then applies the actions that change
    /// objects, scenes or the game state. Returns how many actions ran.
    fn run_rule_event(&mut self, ctx: &mut GameContext<'_>, event: RuleEvent<'_>) -> usize {
        let outcome = {
            let (world, ui) = ctx.rule_targets_mut();
            self.scene
                .apply_rule_event(event, world, ui, &self.ui_nodes)
        };
        let count = outcome.applied + outcome.deferred.len();
        if outcome.deferred.is_empty() {
            return count;
        }
        let commands: Vec<GameCommand> = outcome
            .deferred
            .iter()
            .filter_map(|action| rule_runtime::rule_commands(action, ctx.world(), &mut self.warned))
            .collect();
        if commands.is_empty() || !self.rule_guard.enter() {
            return count;
        }
        match self.apply(ctx, commands) {
            Ok(spawned) => self.rule_spawns.extend(spawned),
            Err(error) => tracing::warn!(%error, "a visual rule action failed"),
        }
        self.rule_guard.leave();
        count
    }

    /// Prefab instances that visual rules created since the last call, so the
    /// host can attach their scripts. Call after `start_rules`, `update_rules`,
    /// `dispatch_ui_clicks`, `sample_collisions` and `send_event`.
    pub fn take_rule_spawns(&mut self) -> Vec<SpawnedObject> {
        std::mem::take(&mut self.rule_spawns)
    }

    /// Object IDs destroyed since the last call (by scripts, rules or game
    /// code, descendants included), so the host can drop their scripts. Call
    /// after every `apply` and after the rule entry points.
    pub fn take_destroyed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.destroyed)
    }

    /// Fires `SceneStart` rules. Call once after the scene is set up and its
    /// scripts' `start` has run (Rust games: at the end of `setup`).
    pub fn start_rules(&mut self, ctx: &mut GameContext<'_>) {
        let carried = std::mem::take(&mut self.carried_ids);
        if carried.is_empty() {
            self.run_rule_event(ctx, RuleEvent::SceneStart);
        } else {
            self.run_rule_event(ctx, RuleEvent::SceneStartExcept(&carried));
        }
        // Values set by scripts' `start` and by these rules are the baseline,
        // so they do not raise change rules on the first frame.
        let current = state::current(ctx.world());
        self.rule_clock.rebase(
            ctx.world().get_resource::<engine_scene::RuleVariables>(),
            current,
        );
    }

    /// Runs the rules driven by input, time and change. Call once per frame in
    /// `update`, after UI clicks and before scripts' `update`.
    ///
    /// Order: `KeyPressed` for keys that went down this frame (also while
    /// paused); timers, which stand still while paused; `VariableChanged` for
    /// variables that differ from the previous call; `StateChanged`. Changes
    /// made by these rules are noticed on the next frame.
    pub fn update_rules(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        let pressed: Vec<engine_scripting::ScriptKey> = engine_scripting::ScriptKey::ALL
            .iter()
            .copied()
            .filter(|key| ctx.input().is_key_pressed(keys::platform_key(*key)))
            .collect();
        let paused = ctx.simulation_paused();
        let current = state::current(ctx.world());
        let due = self.rule_clock.due(
            &self.scene,
            ctx.world().get_resource::<engine_scene::RuleVariables>(),
            current,
            &pressed,
            dt,
            paused,
        );
        for event in &due {
            self.run_rule_event(ctx, event.as_event());
        }
    }

    /// Stores values the player changed with sliders and toggles in their
    /// variables, then makes every bound element show its variable's value.
    fn sync_ui(&mut self, ctx: &mut GameContext<'_>) {
        let (world, ui) = ctx.rule_targets_mut();
        store_ui_changes(world, ui);
    }

    /// The current text of an authored UI element: what the player typed into
    /// a text field, or a label, button or toggle's caption.
    pub fn ui_text<'a>(&self, ctx: &'a GameContext<'_>, object_id: &str) -> Option<&'a str> {
        engine_scene::authored_ui_text(ctx.ui(), &self.ui_nodes, object_id)
    }

    /// Every authored UI element that has text, by object ID.
    pub fn ui_texts<'a>(
        &'a self,
        ctx: &'a GameContext<'_>,
    ) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.ui_nodes.values().filter_map(move |id| {
            engine_scene::authored_ui_text(ctx.ui(), &self.ui_nodes, id)
                .map(|text| (id.as_str(), text))
        })
    }

    /// Runs click rules for this frame's clicked authored buttons and returns
    /// their object IDs, in click order, for game code to react to. Pressing
    /// Enter in a text field counts as clicking it.
    ///
    /// Also carries slider and toggle changes to their variables and refreshes
    /// bound elements, so bars follow variables without any game code. A value
    /// a script sets in `update` shows from the next frame.
    pub fn dispatch_ui_clicks(&mut self, ctx: &mut GameContext<'_>) -> Vec<String> {
        self.sync_ui(ctx);
        let clicked: Vec<String> = ctx
            .ui_clicked()
            .iter()
            .filter_map(|node| self.ui_nodes.get(node).cloned())
            .collect();
        for id in &clicked {
            self.run_rule_event(ctx, RuleEvent::UiClicked(id));
        }
        clicked
    }

    /// Contact enter/exit events since the last call, after running their
    /// collision and trigger rules. Call from `post_fixed_update`.
    pub fn sample_collisions(&mut self, ctx: &mut GameContext<'_>) -> Vec<CollisionEvent> {
        let events = self.contacts.sample(ctx.world(), ctx.physics());
        for event in &events {
            for rule_event in rule_runtime::contact_events(event) {
                self.run_rule_event(ctx, rule_event);
            }
        }
        events
    }

    /// Queues a scene change for the next [`Self::begin_frame`]; the first
    /// request wins until it is applied.
    pub fn request_scene_change(&mut self, path: &str) {
        if self.pending_scene.is_none() {
            self.pending_scene = Some(path.to_owned());
        }
    }

    /// Removes and returns the queued scene change, for hosts that prepare
    /// extra state (such as scripts) before calling [`Self::replace_scene`].
    pub fn take_scene_request(&mut self) -> Option<String> {
        self.pending_scene.take()
    }

    /// Reads and validates a scene from the scene source without changing anything.
    pub fn load_scene(&self, path: &str) -> Result<Scene, GameplayError> {
        let mut scene = self
            .source
            .as_ref()
            .ok_or_else(|| GameplayError::NoSceneSource(path.to_owned()))?
            .load(path)?;
        if let Some(library) = &self.assets {
            refresh_prefab_copies(&mut scene, library);
        }
        Ok(scene)
    }

    /// Records which file the current scene came from, for a host that calls
    /// [`Self::replace_scene`] itself.
    pub fn set_scene_path(&mut self, path: &str) {
        self.scene_path = Some(path.to_owned());
    }

    /// Loads `path` and switches to it; on failure the current scene is untouched.
    pub fn change_scene(
        &mut self,
        ctx: &mut GameContext<'_>,
        path: &str,
    ) -> Result<(), GameplayError> {
        let scene = self.load_scene(path)?;
        self.replace_scene(ctx, scene)?;
        self.set_scene_path(path);
        Ok(())
    }

    /// Clears the world (keeping resources such as gameplay variables), physics,
    /// UI and contacts, restores the initial camera, and sets up `scene`.
    ///
    /// Objects marked Keep between scenes stay as they are, with their bodies
    /// and sounds. A copy of one in `scene` is dropped, so going back to the
    /// scene that placed it does not make a second one.
    pub fn replace_scene(
        &mut self,
        ctx: &mut GameContext<'_>,
        mut scene: Scene,
    ) -> Result<(), GameplayError> {
        let keep = kept_objects(ctx.world());
        self.carried = Scene::capture_entities(ctx.world_mut(), &keep);
        self.carried_objects = keep.iter().copied().collect();
        self.carried_ids = self
            .carried
            .iter()
            .filter_map(|entity| entity.object_id.clone())
            .collect();
        scene.remove_objects(&self.carried_ids);
        {
            let (world, physics, _, _) = ctx.object_parts();
            scenes::clear_physics_except(world, physics, &keep);
        }
        {
            let parts = ctx.scene_parts();
            scenes::clear_world_except(parts.world, parts.assets, &keep);
        }
        ctx.ui_mut().clear();
        self.contacts.clear();
        self.warned.clear();
        self.grounded.clear();
        self.pending_scene = None;
        self.rule_spawns.clear();
        self.destroyed.clear();
        self.scene_loads.clear();
        self.loaded_scenes.clear();
        self.loading_nodes.clear();
        self.change_after_loading = None;
        self.scene = scene;
        self.setup(ctx)?;
        Ok(())
    }
}
