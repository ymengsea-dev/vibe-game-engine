//! Editor-side prefab helpers: capture a selected object and everything under
//! it into an `engine_scene::Prefab` and write it to a `.prefab` file under the
//! project's assets directory.
//!
//! Instantiating a `.prefab` back into the scene lives on
//! [`crate::EditorState::spawn_prefab`] (it needs the live world and the
//! project paths). The pure helpers here ([`capture`], [`sanitize_stem`],
//! [`unique_path`]) are unit-tested; the file write in [`write_prefab`]
//! is covered by a temp-dir test and the editor boot smoke.
//!
//! Live GPU handles are not captured; asset references are.

use std::path::{Path, PathBuf};

use std::collections::HashSet;

use engine_ecs::prelude::{Entity, World};
use engine_scene::{Prefab, Scene, SceneError, ScenePrefabLink};

use crate::object_clipboard::ensure_object_id;

use crate::error::EditorError;

/// Subdirectory of the assets root that [`write_prefab`] puts new
/// `.prefab` files in.
pub const PREFAB_DIR: &str = "prefabs";

/// File stem used when a captured entity has no `Name`.
const DEFAULT_STEM: &str = "Prefab";

/// Builds a [`Prefab`] from `entity` and everything under it, through the same
/// capture a scene save uses, so a prefab holds all that a scene object does.
///
/// # Errors
///
/// [`EditorError::PrefabEntityMissing`] if `entity` is not alive in `world`, or
/// has no name and so is not a scene object.
pub fn capture(world: &mut World, entity: Entity) -> Result<Prefab, EditorError> {
    if !world.entities().contains(entity) {
        return Err(EditorError::PrefabEntityMissing);
    }
    let root_id = ensure_object_id(world, entity).ok_or(EditorError::PrefabEntityMissing)?;
    let members = subtree(world, entity);
    let entities = Scene::capture_entities(world, &members);
    Prefab::from_captured(entities, &root_id).ok_or(EditorError::PrefabEntityMissing)
}

/// `entity` and every object under it.
fn subtree(world: &World, entity: Entity) -> HashSet<Entity> {
    let mut members = HashSet::from([entity]);
    let mut stack = vec![entity];
    while let Some(at) = stack.pop() {
        for child in crate::hierarchy::children_of(world, at) {
            if members.insert(child) {
                stack.push(child);
            }
        }
    }
    members
}

/// Captures `entity` and writes it as a new `.prefab` file under
/// `assets_dir`/[`PREFAB_DIR`], with a non-clobbering name derived from
/// the entity's `Name` (or `"Prefab"`). Creates the directory if
/// needed. The object it came from becomes a linked copy of the new file.
/// Returns the file's absolute path.
///
/// # Errors
///
/// [`EditorError::PrefabEntityMissing`] if the entity is gone;
/// [`EditorError::Prefab`] if the directory can't be created or the file
/// can't be validated / serialized / written.
pub fn write_prefab(
    world: &mut World,
    entity: Entity,
    assets_dir: &Path,
) -> Result<PathBuf, EditorError> {
    let prefab = capture(world, entity)?;
    let stem = prefab
        .entity()
        .name
        .as_deref()
        .map(sanitize_stem)
        .unwrap_or_else(|| DEFAULT_STEM.to_string());

    let dir = assets_dir.join(PREFAB_DIR);
    std::fs::create_dir_all(&dir).map_err(|err| SceneError::Io(err.to_string()))?;

    let path = unique_path(&dir, &stem);
    prefab.save_to_file(&path)?;
    if let Some(file) = path.file_name().and_then(|name| name.to_str()) {
        let link_path = format!("assets/{PREFAB_DIR}/{file}");
        for member in subtree(world, entity) {
            if let Some(id) = ensure_object_id(world, member) {
                world
                    .entity_mut(member)
                    .insert(ScenePrefabLink(engine_scene::PrefabLink {
                        path: link_path.clone(),
                        member: id,
                        overrides: Vec::new(),
                    }));
            }
        }
    }
    Ok(path)
}

/// Turns an arbitrary entity name into a safe file stem: keeps
/// alphanumerics, space, `-` and `_`; replaces anything else with `_`;
/// trims surrounding whitespace; falls back to `"Prefab"` if nothing
/// usable is left.
pub fn sanitize_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        DEFAULT_STEM.to_string()
    } else {
        trimmed.to_string()
    }
}

/// First free path of the form `<dir>/<stem>.prefab`, then
/// `<dir>/<stem> 1.prefab`, `<dir>/<stem> 2.prefab`, ... — so saving the
/// same entity twice does not overwrite the first file.
pub fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.prefab"));
    if !first.exists() {
        return first;
    }
    (1u32..)
        .map(|n| dir.join(format!("{stem} {n}.prefab")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::{AssetSource, Name, Transform};
    use engine_scene::SceneObjectId;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-engine_editor-prefab-test-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn capture_includes_renderable_references() {
        // Same carrier components `Scene::from_world` reads, via the
        // shared `capture_renderables` helper — a prefab and a scene must
        // capture the same entity identically.
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Crate"),
                engine_ecs::components::MeshSource::new(
                    "3fa85f64-5717-4562-b3fc-2c963f66afa6",
                    "9c858901-8a57-4791-81fe-4c455b099bc9",
                ),
            ))
            .id();

        let prefab = capture(&mut world, entity).expect("capture should succeed");

        let mesh_renderer = prefab
            .entity()
            .mesh_renderer
            .as_ref()
            .expect("prefabs must carry their mesh reference");
        assert_eq!(
            mesh_renderer.mesh.id,
            "3fa85f64-5717-4562-b3fc-2c963f66afa6"
        );
        assert_eq!(
            mesh_renderer.material.id,
            "9c858901-8a57-4791-81fe-4c455b099bc9"
        );
    }

    #[test]
    fn capture_keeps_an_instanced_foliage_group() {
        let foliage = engine_scene::FoliageData {
            mesh: engine_scene::AssetRef {
                id: uuid::Uuid::new_v4().to_string(),
            },
            material: engine_scene::AssetRef {
                id: uuid::Uuid::new_v4().to_string(),
            },
            instances: vec![engine_scene::TransformData::from(
                engine_utils::Transform::IDENTITY,
            )],
        };
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Grass"),
                engine_scene::SceneFoliage(foliage.clone()),
            ))
            .id();
        let prefab = capture(&mut world, entity).unwrap();
        assert_eq!(prefab.entity().foliage, Some(foliage));
    }

    #[test]
    fn capture_copies_name_transform_and_asset_source() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Turret"),
                Transform::from(MathTransform::from_translation(Vec3::new(1.0, 2.0, 3.0))),
                AssetSource::with_id("props/turret.gltf", "abc"),
            ))
            .id();

        let prefab = capture(&mut world, entity).unwrap();
        let data = prefab.entity();
        assert_eq!(data.name.as_deref(), Some("Turret"));
        assert_eq!(data.transform.unwrap().translation, [1.0, 2.0, 3.0]);
        assert_eq!(data.asset_source.as_deref(), Some("props/turret.gltf"));
        assert_eq!(data.asset_id.as_deref(), Some("abc"));
        assert!(!data.disabled);
    }

    #[test]
    fn capture_preserves_a_scene_collider_for_runtime_prefab_spawns() {
        let mut world = World::new();
        let collider = engine_scene::ColliderData::new(
            engine_scene::ColliderShape::Cuboid {
                half_extents: [0.5, 1.0, 0.5],
            },
            engine_scene::BodyKind::Dynamic,
        );
        let entity = world
            .spawn((
                Name::new("Crate"),
                engine_scene::SceneCollider(collider.clone()),
            ))
            .id();
        let prefab = capture(&mut world, entity).expect("capture");
        assert_eq!(prefab.entity().collider, Some(collider));
    }

    #[test]
    fn capture_keeps_camera_follow() {
        use engine_ecs::components::{CameraFollow, FollowMode};
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Cam"),
                CameraFollow::new("hero", FollowMode::THIRD_PERSON),
            ))
            .id();
        let prefab = capture(&mut world, entity).unwrap();
        let data = prefab.entity().camera_follow.clone().expect("follow kept");
        assert_eq!(data.target, "hero");
    }

    #[test]
    fn capture_on_a_despawned_entity_errors() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("Gone")).id();
        world.despawn(entity);
        assert!(matches!(
            capture(&mut world, entity),
            Err(EditorError::PrefabEntityMissing)
        ));
    }

    #[test]
    fn sanitize_stem_strips_separators_and_trims() {
        assert_eq!(sanitize_stem("Player/Body"), "Player_Body");
        assert_eq!(sanitize_stem("  Enemy 01  "), "Enemy 01");
        assert_eq!(sanitize_stem("***"), "___");
        assert_eq!(sanitize_stem("   "), "Prefab");
        assert_eq!(sanitize_stem(""), "Prefab");
    }

    #[test]
    fn unique_path_increments_past_existing_files() {
        let dir = temp_dir("unique");
        let first = unique_path(&dir, "Crate");
        assert_eq!(first, dir.join("Crate.prefab"));
        std::fs::write(&first, b"x").unwrap();

        let second = unique_path(&dir, "Crate");
        assert_eq!(second, dir.join("Crate 1.prefab"));
        std::fs::write(&second, b"x").unwrap();

        assert_eq!(unique_path(&dir, "Crate"), dir.join("Crate 2.prefab"));
    }

    #[test]
    fn write_prefab_creates_the_file_under_the_prefabs_dir() {
        let assets = temp_dir("write");
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Barrel"),
                Transform::from(MathTransform::from_translation(Vec3::ZERO)),
            ))
            .id();

        let path = write_prefab(&mut world, entity, &assets).unwrap();
        assert_eq!(path, assets.join(PREFAB_DIR).join("Barrel.prefab"));
        assert!(path.exists());

        let loaded = Prefab::load_from_file(&path).unwrap();
        assert_eq!(loaded.entity().name.as_deref(), Some("Barrel"));
    }

    #[test]
    fn capture_takes_children_scripts_and_lights_and_links_the_source() {
        let assets = temp_dir("children");
        let mut world = World::new();
        let top = world
            .spawn((
                Name::new("Zed"),
                engine_ecs::components::Script::new("assets/scripts/z.lua"),
                engine_scene::ScenePointLight(engine_scene::PointLightData::default()),
            ))
            .id();
        let kid = world
            .spawn((Name::new("Arm"), engine_ecs::prelude::ChildOf(top)))
            .id();

        let path = write_prefab(&mut world, top, &assets).unwrap();
        let loaded = Prefab::load_from_file(&path).unwrap();
        let list: Vec<_> = loaded.entities().collect();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name.as_deref(), Some("Zed"));
        assert_eq!(list[0].script.as_deref(), Some("assets/scripts/z.lua"));
        assert!(list[0].point_light.is_some());
        assert_eq!(list[1].parent, Some(0));

        let link = &world.get::<ScenePrefabLink>(kid).unwrap().0;
        assert_eq!(link.path, "assets/prefabs/Zed.prefab");
        assert_eq!(link.member, world.get::<SceneObjectId>(kid).unwrap().0);
    }

    #[test]
    fn write_prefab_on_a_blank_name_uses_the_default_stem() {
        let assets = temp_dir("nameless");
        let mut world = World::new();
        let entity = world.spawn(Name::new("   ")).id();

        let path = write_prefab(&mut world, entity, &assets).unwrap();
        assert_eq!(path, assets.join(PREFAB_DIR).join("Prefab.prefab"));
    }
}
