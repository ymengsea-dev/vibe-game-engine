//! Editor-side prefab helpers: capture a selected entity into an
//! `engine_scene::Prefab` and write it to a `.prefab` file under the
//! project's assets directory.
//!
//! Instantiating a `.prefab` back into the scene lives on
//! [`crate::EditorState::spawn_prefab`] (it needs the live world and the
//! project paths). The pure helpers here ([`capture`], [`sanitize_stem`],
//! [`unique_path`]) are unit-tested; the file write in [`write_prefab`]
//! is covered by a temp-dir test and the editor boot smoke.
//!
//! ## Not done here
//!
//! - Single-entity prefabs only — `engine_scene::Prefab` wraps one
//!   `SceneEntity`, so a selected entity's children are not captured.
//! - No prefab linking: an instantiated prefab is a plain detached copy,
//!   with no back-reference to the file and no override tracking.
//! - `mesh_renderer` is not captured (it holds live GPU handles, not
//!   describable data yet).

use std::path::{Path, PathBuf};

use engine_ecs::components::{AssetSource, Camera, Disabled, Name, Static, Transform};
use engine_ecs::prelude::{Entity, World};
use engine_scene::{
    CameraData, Prefab, SceneEntity, SceneError, TransformData, capture_renderables,
};

use crate::error::EditorError;

/// Subdirectory of the assets root that [`write_prefab`] puts new
/// `.prefab` files in.
pub const PREFAB_DIR: &str = "prefabs";

/// File stem used when a captured entity has no `Name`.
const DEFAULT_STEM: &str = "Prefab";

/// Builds a single-entity [`Prefab`] from `entity`'s editor-relevant
/// components (`Name` / `Transform` / `Camera` / `AssetSource` /
/// `Disabled` / `Static`). Mirrors the per-entity mapping in
/// `engine_scene::Scene::from_world`; `parent` is left unset — a lone
/// prefab has no sibling list to index into.
///
/// # Errors
///
/// [`EditorError::PrefabEntityMissing`] if `entity` is not alive in
/// `world`.
pub fn capture(world: &World, entity: Entity) -> Result<Prefab, EditorError> {
    if !world.entities().contains(entity) {
        return Err(EditorError::PrefabEntityMissing);
    }
    let asset_source = world.get::<AssetSource>(entity);
    // Shared with `Scene::from_world` so a prefab and a scene capture the
    // same entity identically — these were divergent copies before T-02.
    let (mesh_renderer, sprite) = capture_renderables(world, entity);
    let scene_entity = SceneEntity {
        name: world.get::<Name>(entity).map(|name| name.0.clone()),
        transform: world
            .get::<Transform>(entity)
            .map(|transform| TransformData::from(transform.0)),
        camera: world
            .get::<Camera>(entity)
            .map(|camera| CameraData::from(camera.0)),
        asset_source: asset_source.map(|source| source.path.clone()),
        asset_id: asset_source.and_then(|source| source.id.clone()),
        disabled: world.get::<Disabled>(entity).is_some(),
        is_static: world.get::<Static>(entity).is_some(),
        mesh_renderer,
        sprite,
        ..Default::default()
    };
    Ok(Prefab::new(scene_entity))
}

/// Captures `entity` and writes it as a new `.prefab` file under
/// `assets_dir`/[`PREFAB_DIR`], with a non-clobbering name derived from
/// the entity's `Name` (or `"Prefab"`). Creates the directory if
/// needed. Returns the file's absolute path.
///
/// # Errors
///
/// [`EditorError::PrefabEntityMissing`] if the entity is gone;
/// [`EditorError::Prefab`] if the directory can't be created or the file
/// can't be validated / serialized / written.
pub fn write_prefab(
    world: &World,
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
    use engine_ecs::components::Name;
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

        let prefab = capture(&world, entity).expect("capture should succeed");

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
    fn capture_copies_name_transform_and_asset_source() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Turret"),
                Transform::from(MathTransform::from_translation(Vec3::new(1.0, 2.0, 3.0))),
                AssetSource::with_id("props/turret.gltf", "abc"),
                Static,
            ))
            .id();

        let prefab = capture(&world, entity).unwrap();
        let data = prefab.entity();
        assert_eq!(data.name.as_deref(), Some("Turret"));
        assert_eq!(data.transform.unwrap().translation, [1.0, 2.0, 3.0]);
        assert_eq!(data.asset_source.as_deref(), Some("props/turret.gltf"));
        assert_eq!(data.asset_id.as_deref(), Some("abc"));
        assert!(data.is_static);
        assert!(!data.disabled);
    }

    #[test]
    fn capture_on_a_despawned_entity_errors() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("Gone")).id();
        world.despawn(entity);
        assert!(matches!(
            capture(&world, entity),
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

        let path = write_prefab(&world, entity, &assets).unwrap();
        assert_eq!(path, assets.join(PREFAB_DIR).join("Barrel.prefab"));
        assert!(path.exists());

        let loaded = Prefab::load_from_file(&path).unwrap();
        assert_eq!(loaded.entity().name.as_deref(), Some("Barrel"));
    }

    #[test]
    fn write_prefab_on_a_nameless_entity_uses_the_default_stem() {
        let assets = temp_dir("nameless");
        let mut world = World::new();
        let entity = world
            .spawn(Transform::from(MathTransform::from_translation(Vec3::ZERO)))
            .id();

        let path = write_prefab(&world, entity, &assets).unwrap();
        assert_eq!(path, assets.join(PREFAB_DIR).join("Prefab.prefab"));
    }
}
