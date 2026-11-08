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

use std::collections::{HashMap, HashSet};

use engine_ecs::prelude::Entity;
use engine_renderer::{Camera, LightSet};
use engine_scene::{AssetLibrary, GameStateName, RuleEvent, RuntimeResolver, Scene, SceneError};
use engine_scripting::GameCommand;
use engine_ui::NodeId;

use crate::app::{GameContext, GameError};

pub use bodies::GameplayQueries;
pub use camera::{choose_active_camera, view_from_world};
pub use collisions::{CollisionEvent, ContactKind, ContactTracker};
pub use commands::{PrefabSpawner, apply_world_commands, find_object};
pub use keys::platform_key;
pub use prefab::{instantiate_prefab, valid_prefab_path};
pub use presentation::{scene_lights, spawn_audio_listener};
pub use scenes::{SCENE_BUNDLE_FILE, SceneSource, clear_world, reset_physics, valid_scene_path};

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

/// An object created by [`GameplayRuntime::apply`] from a prefab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedObject {
    /// The new object's stable scene ID.
    pub object_id: String,
    /// The prefab's attached script, for the host to load (`None` if unscripted).
    pub script: Option<String>,
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
}

impl GameplayRuntime {
    /// A runtime for `scene`, resolving its assets from `assets` when present.
    /// Without assets the scene still runs, but entities have no geometry.
    pub fn new(scene: Scene, assets: Option<AssetLibrary>) -> Self {
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
        self.scene_order = report.spawned.clone();
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
        self.refresh_lights(ctx);
        self.ui_nodes = self.scene.mount_ui(ctx.ui_mut());
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
        if let Some(path) = self.pending_scene.take() {
            match self.change_scene(ctx, &path) {
                // The new scene's start rules fire once it is set up.
                Ok(()) => self.start_rules(ctx),
                Err(error) => tracing::error!(scene = %path, %error, "scene change failed"),
            }
        }
        self.refresh_lights(ctx);
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
            .filter_map(|(_, scene)| {
                let data = scene.entities.first()?;
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
            for (entity, mut scene) in spawned {
                if let (Some(transform), Some(data)) = (
                    ctx.world().get::<engine_ecs::components::Transform>(entity),
                    scene.entities.first_mut(),
                ) {
                    data.transform = Some(transform.0.into());
                }
                let report = engine_scene::InstantiateReport {
                    spawned: vec![entity],
                    unresolved: 0,
                };
                ctx.spawn_scene_colliders(&scene, &report, library);
                let (world, physics, _, _) = ctx.object_parts();
                objects::disable_authored_subtrees(world, physics, &[entity]);
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
        self.run_rule_event(ctx, RuleEvent::SceneStart);
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

    /// Runs click rules for this frame's clicked authored buttons and returns
    /// their object IDs, in click order, for game code to react to.
    pub fn dispatch_ui_clicks(&mut self, ctx: &mut GameContext<'_>) -> Vec<String> {
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
        self.source
            .as_ref()
            .ok_or_else(|| GameplayError::NoSceneSource(path.to_owned()))?
            .load(path)
    }

    /// Loads `path` and switches to it; on failure the current scene is untouched.
    pub fn change_scene(
        &mut self,
        ctx: &mut GameContext<'_>,
        path: &str,
    ) -> Result<(), GameplayError> {
        let scene = self.load_scene(path)?;
        self.replace_scene(ctx, scene)
    }

    /// Clears the world (keeping resources such as gameplay variables), physics,
    /// UI and contacts, restores the initial camera, and sets up `scene`.
    pub fn replace_scene(
        &mut self,
        ctx: &mut GameContext<'_>,
        scene: Scene,
    ) -> Result<(), GameplayError> {
        {
            let parts = ctx.scene_parts();
            clear_world(parts.world, parts.assets);
        }
        reset_physics(ctx.physics_mut());
        ctx.ui_mut().clear();
        self.contacts.clear();
        self.warned.clear();
        self.grounded.clear();
        self.pending_scene = None;
        self.rule_spawns.clear();
        self.destroyed.clear();
        self.scene = scene;
        self.setup(ctx)?;
        Ok(())
    }
}
