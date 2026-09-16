//! Runtime save games: persisting a live world so a player can close the
//! game and come back to it.
//!
//! ## Why this is not the scene format
//!
//! [`crate::Scene`] looks like it would do — it already serializes
//! entities, transforms and asset references. It must not be used here,
//! for one disqualifying reason: **`Scene::from_world` captures only
//! *named* entities.** That is right for an authoring format, where the
//! hierarchy panel lists things by name, and fatal for a save, where an
//! unnamed projectile, a scattered rock or a spawned enemy is exactly as
//! real as anything the designer named.
//!
//! `Scene` also carries editor concepts (`locked`) and sorts by name for
//! clean diffs. Neither belongs in a save file. So this module has its
//! own entity envelope, reusing only the plain data mirrors
//! ([`TransformData`], [`CameraData`], [`AssetRef`]) that both formats
//! legitimately share.
//!
//! ## What a snapshot does not capture
//!
//! Stated plainly, because the limits decide what kind of save a game can
//! offer:
//!
//! - **Animation playback position.** An entity mid-stride restores
//!   standing still.
//! - **Physics velocity, and the bodies themselves.** Poses survive
//!   because they live in `Transform`; momentum does not, and a game must
//!   re-create its rigid bodies on load — it knows what shapes they are,
//!   this format does not.
//! - **Audio playback position.** A half-played sound restarts.
//!
//! Enough for save-on-exit and save-at-a-checkpoint, which is what most
//! games want. Not enough for "quicksave mid-jump and resume exactly".

use std::path::Path;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::{Entity, World};
use engine_ecs::components::{AssetSource, Camera, Disabled, Name, Static, Transform};
use serde::{Serialize, de::DeserializeOwned};

use crate::capture::capture_renderables;
use crate::error::SceneError;
use crate::format::{CameraData, MeshRendererData, SpriteData, TransformData};
use crate::resolve::{InstantiateReport, SceneResolver, insert_components};

/// The save format version this engine writes and understands.
///
/// Bumped whenever the shape changes incompatibly. A save carrying a
/// higher number is refused with a clear message rather than parsed into
/// something wrong.
pub const CURRENT_SAVE_VERSION: u32 = 1;

/// One entity as stored in a save.
///
/// Deliberately separate from [`crate::SceneEntity`] — see the module
/// docs. The overlap in fields is real but the *capture rules* differ,
/// and tying them together would mean an editor change silently altering
/// the save format.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedEntity {
    /// Display name, if the entity has one. Unlike a scene, an entity
    /// without a name is still saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index of this entity's parent within the owning snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// World or parent-relative transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformData>,
    /// Camera, if this entity is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraData>,
    /// Mesh and material references, so geometry comes back on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_renderer: Option<MeshRendererData>,
    /// Sprite reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<SpriteData>,
    /// The asset file this entity came from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_source: Option<String>,
    /// That asset's stable id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// Whether the entity was disabled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Whether the entity was marked static.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
}

/// Every entity in a world, captured for saving.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorldSnapshot {
    /// The entities, in capture order. [`SavedEntity::parent`] indexes
    /// into this list.
    pub entities: Vec<SavedEntity>,
}

impl WorldSnapshot {
    /// Captures **every** entity carrying at least one savable component.
    ///
    /// Named or not — that is the whole difference from
    /// [`crate::Scene::from_world`], and the reason this exists.
    ///
    /// Entities with no savable component at all are skipped, which keeps
    /// `bevy_ecs`'s internal bookkeeping entities out of the file without
    /// needing a name to filter on.
    pub fn capture(world: &mut World) -> Self {
        let mut query = world.query::<(
            Entity,
            Option<&Name>,
            Option<&Transform>,
            Option<&Camera>,
            Option<&AssetSource>,
            Option<&Disabled>,
            Option<&Static>,
        )>();

        let collected: Vec<(Entity, SavedEntity)> = query
            .iter(world)
            .filter_map(
                |(entity, name, transform, camera, asset_source, disabled, is_static)| {
                    // Something must be worth saving, or this is
                    // bookkeeping rather than game state.
                    if name.is_none() && transform.is_none() && camera.is_none() {
                        return None;
                    }
                    Some((
                        entity,
                        SavedEntity {
                            name: name.map(|name| name.0.clone()),
                            parent: None,
                            transform: transform.map(|t| TransformData::from(t.0)),
                            camera: camera.map(|c| CameraData::from(c.0)),
                            mesh_renderer: None,
                            sprite: None,
                            asset_source: asset_source.map(|source| source.path.clone()),
                            asset_id: asset_source.and_then(|source| source.id.clone()),
                            disabled: disabled.is_some(),
                            is_static: is_static.is_some(),
                        },
                    ))
                },
            )
            .collect();

        // Index map first, so parent links can be resolved to positions
        // in this list.
        let index_of: std::collections::HashMap<Entity, usize> = collected
            .iter()
            .enumerate()
            .map(|(index, (entity, _))| (*entity, index))
            .collect();

        let entities = collected
            .into_iter()
            .map(|(entity, mut saved)| {
                let (mesh_renderer, sprite) = capture_renderables(world, entity);
                saved.mesh_renderer = mesh_renderer;
                saved.sprite = sprite;
                // A parent that was itself skipped leaves this entity a
                // root, rather than dangling at an index that means
                // something else.
                saved.parent = world
                    .get::<ChildOf>(entity)
                    .and_then(|child_of| index_of.get(&child_of.parent()).copied());
                saved
            })
            .collect();

        Self { entities }
    }

    /// Spawns every saved entity into `world`, resolving asset references
    /// through `resolver`.
    ///
    /// Does not clear `world` first — the caller decides whether a load
    /// replaces the world or merges into it.
    pub fn restore(
        &self,
        world: &mut World,
        resolver: &mut impl SceneResolver,
    ) -> InstantiateReport {
        let mut unresolved = 0;
        let spawned: Vec<Entity> = self
            .entities
            .iter()
            .map(|saved| {
                let mut entity_mut = world.spawn_empty();
                insert_components(
                    &mut entity_mut,
                    &saved.as_scene_entity(),
                    resolver,
                    &mut unresolved,
                );
                entity_mut.id()
            })
            .collect();

        for (index, saved) in self.entities.iter().enumerate() {
            if let Some(parent_index) = saved.parent
                && parent_index != index
                && let Some(&parent) = spawned.get(parent_index)
            {
                world.entity_mut(spawned[index]).insert(ChildOf(parent));
            }
        }

        InstantiateReport {
            spawned,
            unresolved,
        }
    }
}

impl SavedEntity {
    /// Borrows this entity's data in the shape
    /// [`insert_components`] expects.
    ///
    /// The two formats share a spawn path even though they do not share a
    /// capture path — respawning components is genuinely the same job.
    fn as_scene_entity(&self) -> crate::SceneEntity {
        crate::SceneEntity {
            name: self.name.clone(),
            parent: None,
            transform: self.transform,
            camera: self.camera,
            mesh_renderer: self.mesh_renderer.clone(),
            sprite: self.sprite.clone(),
            asset_source: self.asset_source.clone(),
            asset_id: self.asset_id.clone(),
            disabled: self.disabled,
            is_static: self.is_static,
            locked: false,
            // A save restores a live world, and a live emitter's sound is
            // already loaded — re-authoring one from the save file would
            // restart it.
            audio_emitter: None,
            // A save restores where a body *was*, not how it was built:
            // the scene that spawned it already described its collider,
            // and rebuilding one here would double it up.
            collider: None,
        }
    }
}

/// A save file: the world, plus whatever the game itself wants to
/// remember.
///
/// Generic over the game's own type rather than a dynamic value, so a
/// game gets its real struct back on load instead of digging through
/// untyped data. `T` can be `()` for a game with no extra state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SaveGame<T> {
    /// Format version — see [`CURRENT_SAVE_VERSION`].
    pub version: u32,
    /// Every entity in the world at save time.
    pub world: WorldSnapshot,
    /// The game's own state: score, inventory, quest flags, clock.
    pub game: T,
}

impl<T> SaveGame<T> {
    /// Captures `world` alongside the game's own `game` state.
    pub fn capture(world: &mut World, game: T) -> Self {
        Self {
            version: CURRENT_SAVE_VERSION,
            world: WorldSnapshot::capture(world),
            game,
        }
    }

    /// Spawns this save's entities into `world`.
    pub fn restore(
        &self,
        world: &mut World,
        resolver: &mut impl SceneResolver,
    ) -> InstantiateReport {
        self.world.restore(world, resolver)
    }
}

impl<T: Serialize> SaveGame<T> {
    /// Serializes to pretty RON.
    ///
    /// # Errors
    ///
    /// [`SceneError::Serialize`] if the game's own data cannot be encoded.
    pub fn to_ron_string(&self) -> Result<String, SceneError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| SceneError::Serialize(err.to_string()))
    }

    /// Writes this save to `path`, atomically.
    ///
    /// The write goes to a temporary file and is renamed into place, so a
    /// crash or power loss mid-save leaves the *previous* save intact
    /// rather than a half-written one. That matters more here than
    /// anywhere else in the engine: this is the player's progress.
    ///
    /// # Errors
    ///
    /// [`SceneError::Serialize`] if encoding fails; [`SceneError::Io`] if
    /// the write or rename does.
    pub fn save_to_file(&self, path: &Path) -> Result<(), SceneError> {
        let text = self.to_ron_string()?;
        crate::io::write_atomic(path, text.as_bytes())
            .map_err(|err| SceneError::Io(err.to_string()))
    }
}

impl<T: DeserializeOwned> SaveGame<T> {
    /// Parses a save from RON text.
    ///
    /// The version is read and checked **before** the body is parsed, so
    /// a save from a newer engine reports its version rather than failing
    /// with a confusing shape error.
    ///
    /// # Errors
    ///
    /// [`SceneError::UnsupportedVersion`] if the save is newer than this
    /// engine; [`SceneError::Deserialize`] if the text is not a valid
    /// save.
    pub fn from_ron_str(text: &str) -> Result<Self, SceneError> {
        check_version(text)?;
        ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))
    }

    /// Reads and parses a save from `path`.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] if the file cannot be read, plus anything
    /// [`SaveGame::from_ron_str`] returns.
    pub fn load_from_file(path: &Path) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path).map_err(|err| SceneError::Io(err.to_string()))?;
        Self::from_ron_str(&text)
    }
}

/// Reads just the `version` field and rejects anything newer than this
/// engine understands.
///
/// A cheap pre-parse: without it, a future save fails somewhere deep in
/// field matching and the user is told their file is corrupt when it is
/// merely newer.
fn check_version(text: &str) -> Result<(), SceneError> {
    let value: ron::Value =
        ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))?;
    let ron::Value::Map(map) = value else {
        return Err(SceneError::Deserialize(
            "a save must be a struct, not a bare value".to_string(),
        ));
    };
    let found = map
        .iter()
        .find(|(key, _)| matches!(key, ron::Value::String(name) if name == "version"))
        .and_then(|(_, value)| match value {
            ron::Value::Number(number) => Some(number.into_f64() as u32),
            _ => None,
        })
        .ok_or_else(|| SceneError::Deserialize("a save must carry a version field".to_string()))?;

    if found > CURRENT_SAVE_VERSION {
        return Err(SceneError::UnsupportedVersion {
            found,
            max: CURRENT_SAVE_VERSION,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::NullResolver;
    use engine_ecs::components::{MeshSource, Name};
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Progress {
        steps: u32,
        visited: Vec<String>,
        position: (f32, f32),
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("vge-save-{name}-{}-{n}.ron", std::process::id()))
    }

    #[test]
    fn unnamed_entities_are_saved() {
        // The whole reason this format exists rather than reusing
        // `Scene`, which drops them.
        let mut world = World::new();
        world.spawn(Transform::from(MathTransform::from_translation(Vec3::X)));
        world.spawn(Transform::from(MathTransform::from_translation(Vec3::Y)));
        world.spawn((Name::new("Named"), Transform::from(MathTransform::IDENTITY)));

        let snapshot = WorldSnapshot::capture(&mut world);

        assert_eq!(
            snapshot.entities.len(),
            3,
            "a save must keep unnamed entities; Scene would have kept only 1",
        );
        assert_eq!(
            snapshot
                .entities
                .iter()
                .filter(|e| e.name.is_none())
                .count(),
            2,
        );
    }

    #[test]
    fn entities_with_nothing_worth_saving_are_skipped() {
        let mut world = World::new();
        world.spawn_empty();
        world.spawn(Transform::from(MathTransform::IDENTITY));

        assert_eq!(WorldSnapshot::capture(&mut world).entities.len(), 1);
    }

    #[test]
    fn a_hundred_entities_round_trip_with_their_transforms() {
        let mut world = World::new();
        for i in 0..100 {
            world.spawn((
                Name::new(format!("Entity{i}")),
                Transform::from(MathTransform::from_translation(Vec3::new(
                    i as f32, 0.0, 0.0,
                ))),
            ));
        }

        let save = SaveGame::capture(&mut world, ());
        let text = save.to_ron_string().expect("serialize");
        let loaded: SaveGame<()> = SaveGame::from_ron_str(&text).expect("parse");

        let mut restored = World::new();
        let report = loaded.restore(&mut restored, &mut NullResolver);
        assert_eq!(report.spawned.len(), 100);

        let mut query = restored.query::<(&Name, &Transform)>();
        let mut seen = 0;
        for (name, transform) in query.iter(&restored) {
            let index: f32 = name.0.trim_start_matches("Entity").parse().expect("index");
            assert!((transform.0.translation.x - index).abs() < 1e-4);
            seen += 1;
        }
        assert_eq!(seen, 100);
    }

    #[test]
    fn hierarchy_survives_the_round_trip() {
        let mut world = World::new();
        let parent = world
            .spawn((Name::new("Parent"), Transform::default()))
            .id();
        world.spawn((Name::new("Child"), Transform::default(), ChildOf(parent)));

        let save = SaveGame::capture(&mut world, ());
        let text = save.to_ron_string().expect("serialize");
        let loaded: SaveGame<()> = SaveGame::from_ron_str(&text).expect("parse");

        let mut restored = World::new();
        let report = loaded.restore(&mut restored, &mut NullResolver);

        let child = report
            .spawned
            .iter()
            .find(|&&e| {
                restored
                    .get::<Name>(e)
                    .is_some_and(|name| name.0 == "Child")
            })
            .copied()
            .expect("child restored");
        assert!(
            restored.get::<ChildOf>(child).is_some(),
            "the parent link must survive",
        );
    }

    #[test]
    fn game_blob_round_trips() {
        let mut world = World::new();
        let progress = Progress {
            steps: 4218,
            visited: vec!["beach".into(), "hilltop".into()],
            position: (12.5, -3.25),
        };

        let save = SaveGame::capture(&mut world, progress.clone());
        let text = save.to_ron_string().expect("serialize");
        let loaded: SaveGame<Progress> = SaveGame::from_ron_str(&text).expect("parse");

        assert_eq!(loaded.game, progress);
    }

    #[test]
    fn corrupt_save_is_an_error_not_a_panic() {
        let result: Result<SaveGame<()>, _> = SaveGame::from_ron_str("this is not RON {{{");
        assert!(matches!(result, Err(SceneError::Deserialize(_))));
    }

    #[test]
    fn a_save_without_a_version_is_rejected() {
        let result: Result<SaveGame<()>, _> = SaveGame::from_ron_str("(world: (entities: []))");
        assert!(matches!(result, Err(SceneError::Deserialize(_))));
    }

    #[test]
    fn future_version_is_rejected_with_a_clear_error() {
        let text = "(version: 999, world: (entities: []), game: ())";
        let result: Result<SaveGame<()>, _> = SaveGame::from_ron_str(text);
        match result {
            Err(SceneError::UnsupportedVersion { found, max }) => {
                assert_eq!(found, 999);
                assert_eq!(max, CURRENT_SAVE_VERSION);
            }
            other => panic!("expected a version error, got {other:?}"),
        }
    }

    #[test]
    fn save_and_load_through_a_file() {
        let path = temp_path("roundtrip");
        let mut world = World::new();
        world.spawn((Name::new("Player"), Transform::default()));

        SaveGame::capture(&mut world, 7u32)
            .save_to_file(&path)
            .expect("save");
        let loaded: SaveGame<u32> = SaveGame::load_from_file(&path).expect("load");

        assert_eq!(loaded.game, 7);
        assert_eq!(loaded.world.entities.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let path = temp_path("atomic");
        let mut world = World::new();
        world.spawn((Name::new("Player"), Transform::default()));
        SaveGame::capture(&mut world, ())
            .save_to_file(&path)
            .expect("save");

        let directory = path.parent().expect("parent");
        let stray: Vec<_> = std::fs::read_dir(directory)
            .expect("read dir")
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with('.') && name.ends_with(".tmp") && name.contains("vge-save-atomic")
            })
            .collect();
        assert!(
            stray.is_empty(),
            "a partial file was left behind: {stray:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_over_an_existing_save_replaces_it_wholly() {
        let path = temp_path("replace");
        let mut world = World::new();
        world.spawn((Name::new("First"), Transform::default()));
        SaveGame::capture(&mut world, 1u32)
            .save_to_file(&path)
            .expect("first save");

        let mut second = World::new();
        second.spawn((Name::new("Second"), Transform::default()));
        SaveGame::capture(&mut second, 2u32)
            .save_to_file(&path)
            .expect("second save");

        let loaded: SaveGame<u32> = SaveGame::load_from_file(&path).expect("load");
        assert_eq!(loaded.game, 2);
        assert_eq!(loaded.world.entities[0].name.as_deref(), Some("Second"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_missing_file_is_an_io_error() {
        let result: Result<SaveGame<()>, _> =
            SaveGame::load_from_file(Path::new("/definitely/not/here.ron"));
        assert!(matches!(result, Err(SceneError::Io(_))));
    }

    #[test]
    fn renderable_references_survive_so_geometry_comes_back() {
        let mut world = World::new();
        world.spawn((
            Name::new("Crate"),
            Transform::default(),
            MeshSource::new(
                "3fa85f64-5717-4562-b3fc-2c963f66afa6",
                "9c858901-8a57-4791-81fe-4c455b099bc9",
            ),
        ));

        let save = SaveGame::capture(&mut world, ());
        let mesh = save.world.entities[0]
            .mesh_renderer
            .as_ref()
            .expect("the mesh reference must be saved");
        assert_eq!(mesh.mesh.id, "3fa85f64-5717-4562-b3fc-2c963f66afa6");
    }
}
