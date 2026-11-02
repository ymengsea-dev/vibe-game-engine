//! Runtime instances of packaged single-object prefabs.

use std::path::{Component, Path};

use engine_ecs::prelude::{Entity, World};
use engine_scene::{AssetLibrary, Prefab, Scene, SceneResolver, TransformData};

use super::GameplayError;
use super::commands::valid_new_object;

/// Prefab files larger than this are refused before parsing.
const MAX_PREFAB_BYTES: usize = 1024 * 1024;

/// Whether `path` is a project-relative `assets/prefabs/**/*.prefab` path.
pub fn valid_prefab_path(path: &str) -> bool {
    let relative = Path::new(path);
    !relative.is_absolute()
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && relative.starts_with("assets/prefabs")
        && relative.extension().and_then(|ext| ext.to_str()) == Some("prefab")
}

/// Instantiates the prefab at `path` as a runtime object with `object_id` at `position`.
///
/// Returns the spawned entity and the one-entity scene it came from, which the
/// caller uses to add colliders. A prefab may carry a script path (validated
/// like any scene script); attaching it is the game host's job, because only
/// the host knows its gameplay language.
pub fn instantiate_prefab(
    world: &mut World,
    assets: &AssetLibrary,
    resolver: &mut impl SceneResolver,
    path: &str,
    object_id: &str,
    position: [f32; 3],
) -> Result<(Entity, Scene), GameplayError> {
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
    let mut data = prefab.entity().clone();
    data.object_id = Some(object_id.to_owned());
    let mut transform = data.transform.unwrap_or(TransformData {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    });
    transform.translation = position;
    data.transform = Some(transform);
    let mut scene = Scene::default();
    scene.entities.push(data);
    scene.validate().map_err(|err| fail(&err.to_string()))?;
    let report = scene.instantiate_with_resolver(world, resolver);
    let entity = report
        .spawned
        .first()
        .copied()
        .ok_or_else(|| fail("no entity was spawned"))?;
    if report.unresolved > 0 {
        tracing::warn!(
            path,
            unresolved = report.unresolved,
            "prefab has unresolved assets"
        );
    }
    Ok((entity, scene))
}
