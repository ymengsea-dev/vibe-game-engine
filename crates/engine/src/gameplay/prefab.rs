//! Runtime instances of packaged prefabs, and bringing placed copies up to date.

use std::collections::HashMap;

use engine_ecs::prelude::{Entity, World};
use engine_scene::{AssetLibrary, Prefab, Scene, SceneResolver, TransformData};

use super::GameplayError;
use super::commands::valid_new_object;

/// Prefab files larger than this are refused before parsing.
const MAX_PREFAB_BYTES: usize = 1024 * 1024;

pub use engine_scene::valid_prefab_path;

/// Instantiates the prefab at `path` as a runtime object with `object_id` at `position`.
///
/// Returns the spawned entities and the scene they came from, in the same order
/// with the top object first, which the caller uses to add colliders. Objects
/// may carry script paths (validated like any scene script); attaching them is
/// the game host's job, because only the host knows its gameplay language.
pub fn instantiate_prefab(
    world: &mut World,
    assets: &AssetLibrary,
    resolver: &mut impl SceneResolver,
    path: &str,
    object_id: &str,
    position: [f32; 3],
) -> Result<(Vec<Entity>, Scene), GameplayError> {
    let fail = |reason: &str| GameplayError::Prefab(path.to_owned(), reason.to_owned());
    if !valid_prefab_path(path) || !valid_new_object(world, object_id, position) {
        return Err(fail("invalid request"));
    }
    let store_path = path
        .strip_prefix("assets/")
        .ok_or_else(|| fail("invalid path"))?;
    let bytes = assets
        .read_path(store_path)
        .map_err(|err| fail(&err.to_string()))?
        .ok_or_else(|| fail("file not found"))?;
    if bytes.len() > MAX_PREFAB_BYTES {
        return Err(fail("file exceeds 1 MiB"));
    }
    let source = std::str::from_utf8(&bytes).map_err(|err| fail(&err.to_string()))?;
    let prefab = Prefab::from_ron_str(source).map_err(|err| fail(&err.to_string()))?;
    let mut scene = Scene {
        entities: prefab.copy_entities(None),
        ..Default::default()
    };
    let top = scene
        .entities
        .first_mut()
        .ok_or_else(|| fail("no entity was spawned"))?;
    let new_id = top.object_id.replace(object_id.to_owned());
    let mut transform = top.transform.unwrap_or(TransformData {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    });
    transform.translation = position;
    top.transform = Some(transform);
    // References to the top object follow it to the requested ID.
    if let Some(new_id) = new_id {
        let map = HashMap::from([(new_id, object_id.to_owned())]);
        for entity in &mut scene.entities {
            engine_scene::remap_object_references(entity, &map);
        }
    }
    scene.validate().map_err(|err| fail(&err.to_string()))?;
    let report = scene.instantiate_with_resolver(world, resolver);
    if report.spawned.is_empty() {
        return Err(fail("no entity was spawned"));
    }
    if report.unresolved > 0 {
        tracing::warn!(
            path,
            unresolved = report.unresolved,
            "prefab has unresolved assets"
        );
    }
    Ok((report.spawned, scene))
}

/// Brings `scene`'s prefab copies up to date with the packaged prefab files. A
/// result that does not validate is dropped with a warning, and the copies run
/// as saved.
pub fn refresh_prefab_copies(scene: &mut Scene, assets: &AssetLibrary) {
    if !scene.entities.iter().any(|entity| entity.prefab.is_some()) {
        return;
    }
    let mut load = |path: &str| {
        let store_path = path
            .strip_prefix("assets/")
            .filter(|_| valid_prefab_path(path))?;
        let bytes = assets.read_path(store_path).ok()??;
        if bytes.len() > MAX_PREFAB_BYTES {
            return None;
        }
        Prefab::from_ron_str(std::str::from_utf8(&bytes).ok()?).ok()
    };
    let mut refreshed = scene.clone();
    if !engine_scene::refresh_copies(&mut refreshed, &mut load) {
        return;
    }
    match refreshed.validate() {
        Ok(()) => *scene = refreshed,
        Err(error) => tracing::warn!(%error, "prefab copies could not be refreshed"),
    }
}
