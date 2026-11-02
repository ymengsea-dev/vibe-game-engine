//! Where a running game reads its scenes from, and how the world is reset
//! between them.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use engine_ecs::components::{
    InstancedMeshRenderer, MeshRenderer, SkinnedMeshRenderer, VegetationRenderer,
};
use engine_ecs::prelude::World;
use engine_physics::PhysicsWorld;
use engine_project::Project;
use engine_renderer::RenderAssets;
use engine_scene::{Scene, SceneNavGrid};

use super::GameplayError;

pub use engine_project::build::SCENE_BUNDLE_FILE;

/// The origin of scenes requested with `change_scene`.
#[derive(Debug, Clone)]
pub enum SceneSource {
    /// A project on disk, as used by Studio Play.
    Project(Project),
    /// An exported game directory containing [`SCENE_BUNDLE_FILE`].
    Export(PathBuf),
}

/// Whether `path` is a portable, project-relative `scenes/**/*.ron` path.
pub fn valid_scene_path(path: &str) -> bool {
    let relative = Path::new(path);
    !relative.is_absolute()
        && !path.contains('\\')
        && !path.contains("//")
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && relative.starts_with("scenes")
        && relative.extension().and_then(|ext| ext.to_str()) == Some("ron")
}

impl SceneSource {
    /// Reads, validates and assigns object IDs to the scene at `path`.
    pub fn load(&self, path: &str) -> Result<Scene, GameplayError> {
        let fail = |reason: String| GameplayError::SceneRequest(path.to_owned(), reason);
        if !valid_scene_path(path) {
            return Err(fail("expected a project-relative scenes/*.ron path".into()));
        }
        let mut scene = match self {
            Self::Project(project) => {
                let absolute = project
                    .resolve(Path::new(path))
                    .map_err(|error| fail(error.to_string()))?;
                Scene::load_from_file(&absolute)?
            }
            Self::Export(root) => {
                let packed = engine_asset::Bundle::open(&root.join(SCENE_BUNDLE_FILE))
                    .map_err(|error| fail(error.to_string()))?;
                let key = path
                    .strip_prefix("scenes/")
                    .ok_or_else(|| fail("invalid scene path".into()))?;
                let bytes = packed.get(key).ok_or_else(|| fail("not packaged".into()))?;
                let text = std::str::from_utf8(bytes).map_err(|error| fail(error.to_string()))?;
                Scene::from_ron_str(text)?
            }
        };
        scene.validate()?;
        scene.assign_missing_object_ids();
        Ok(scene)
    }
}

/// Despawns every non-resource entity and releases its GPU meshes/materials.
///
/// Resources (such as gameplay variables) survive, so values carry across a
/// scene change; the navigation grid belongs to the old scene and is removed.
pub fn clear_world(world: &mut World, assets: &mut RenderAssets) {
    let resources: HashSet<_> = world
        .resource_entities()
        .iter()
        .map(|(_, entity)| entity)
        .collect();
    let entities: Vec<_> = world
        .iter_entities()
        .map(|entity| entity.id())
        .filter(|entity| !resources.contains(entity))
        .collect();
    for entity in &entities {
        if let Some(renderer) = world.get::<MeshRenderer>(*entity) {
            assets.meshes.release(renderer.mesh);
            assets.materials.release(renderer.material);
        }
        if let Some(renderer) = world.get::<SkinnedMeshRenderer>(*entity) {
            assets.skinned_meshes.release(renderer.mesh);
            assets.materials.release(renderer.material);
        }
        if let Some(renderer) = world.get::<InstancedMeshRenderer>(*entity) {
            assets.meshes.release(renderer.mesh);
            assets.materials.release(renderer.material);
        }
        if let Some(renderer) = world.get::<VegetationRenderer>(*entity) {
            assets.meshes.release(renderer.mesh);
            assets.materials.release(renderer.material);
        }
    }
    for entity in entities {
        world.despawn(entity);
    }
    world.remove_resource::<SceneNavGrid>();
}

/// Replaces the physics world with an empty one, keeping its gravity.
pub fn reset_physics(physics: &mut PhysicsWorld) {
    let gravity = physics.rapier.gravity;
    *physics = PhysicsWorld::new(gravity);
}
