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
//!     fn update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
//!         self.gameplay.begin_frame(ctx);
//!         self.gameplay.dispatch_ui_clicks(ctx);
//!     }
//!     fn post_fixed_update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
//!         for contact in self.gameplay.sample_collisions(ctx) {
//!             tracing::info!(?contact, "contact");
//!         }
//!     }
//! }
//! ```

mod collisions;
mod commands;
mod prefab;
mod presentation;
mod scenes;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use engine_renderer::{Camera, LightSet};
use engine_scene::{AssetLibrary, RuleEvent, RuntimeResolver, Scene, SceneError};
use engine_scripting::GameCommand;
use engine_ui::NodeId;

use crate::app::{GameContext, GameError};

pub use collisions::{CollisionEvent, ContactKind, ContactTracker};
pub use commands::{PrefabSpawner, apply_world_commands, find_object};
pub use prefab::{instantiate_prefab, valid_prefab_path};
pub use presentation::{scene_camera, scene_lights, spawn_audio_listener};
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
    pending_scene: Option<String>,
    initial_camera: Option<Camera>,
    default_lights: Option<LightSet>,
    /// Audio bus the scene's ambient sounds play on, created once per game.
    ambient_bus: Option<usize>,
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
            pending_scene: None,
            initial_camera: None,
            default_lights: None,
            ambient_bus: None,
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
        *ctx.camera_mut() = scene_camera(&self.scene, camera);
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

        let camera = *ctx.camera_mut();
        spawn_audio_listener(ctx.world_mut(), &camera);
        if let Some(library) = &self.assets {
            ctx.spawn_scene_colliders(&self.scene, &report, library);
            ctx.set_scene_sprite_atlas(library)?;
        }
        self.refresh_lights(ctx);
        self.ui_nodes = self.scene.mount_ui(ctx.ui_mut());
        Ok(())
    }

    /// Per-frame housekeeping: applies a pending scene change, then refreshes
    /// lights so moved point lights follow their objects. Call at the start of
    /// `update`. A failed scene change is logged and the current scene keeps running.
    pub fn begin_frame(&mut self, ctx: &mut GameContext<'_>) {
        if let Some(path) = self.pending_scene.take()
            && let Err(error) = self.change_scene(ctx, &path)
        {
            tracing::error!(scene = %path, %error, "scene change failed");
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
            }
            if has_sprites {
                ctx.set_scene_sprite_atlas(library)?;
            }
        }
        let poses = commands::moved_body_poses(ctx.world(), &commands::moved_object_ids(&commands));
        commands::sync_moved_bodies(ctx.physics_mut(), &poses);

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
        let (world, ui) = ctx.rule_targets_mut();
        self.scene
            .apply_rule_event(RuleEvent::Named(name), world, ui, &self.ui_nodes)
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
            let (world, ui) = ctx.rule_targets_mut();
            self.scene
                .apply_rule_event(RuleEvent::UiClicked(id), world, ui, &self.ui_nodes);
        }
        clicked
    }

    /// Contact enter/exit events since the last call. Call from `post_fixed_update`.
    pub fn sample_collisions(&mut self, ctx: &GameContext<'_>) -> Vec<CollisionEvent> {
        self.contacts.sample(ctx.world(), ctx.physics())
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
        self.pending_scene = None;
        self.scene = scene;
        self.setup(ctx)?;
        Ok(())
    }
}
