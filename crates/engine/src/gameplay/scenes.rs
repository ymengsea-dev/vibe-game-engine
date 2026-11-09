//! Where a running game reads its scenes from, and how the world is reset
//! between them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine_ecs::components::{
    InstancedMeshRenderer, MeshRenderer, Script, SkinnedMeshRenderer, VegetationRenderer,
};
use engine_ecs::prelude::{ChildOf, Entity, World};
use engine_physics::PhysicsWorld;
use engine_project::Project;
use engine_renderer::RenderAssets;
use engine_scene::{KeepBetweenScenes, Scene, SceneNavGrid, SceneObjectId};

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

pub use engine_scene::valid_scene_path;

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

impl SceneSource {
    /// The scene the game starts on, and every scene in the project's order.
    pub fn scene_order(&self) -> engine_project::build::ExportedSceneOrder {
        match self {
            Self::Project(project) => engine_project::build::ExportedSceneOrder {
                start: project
                    .manifest()
                    .main_scene
                    .to_string_lossy()
                    .replace('\\', "/"),
                order: project.ordered_scenes(),
            },
            Self::Export(root) => {
                std::fs::read_to_string(root.join(engine_project::build::SCENE_ORDER_FILE))
                    .ok()
                    .and_then(|text| ron::from_str(&text).ok())
                    .unwrap_or_default()
            }
        }
    }
}

/// The scene after `current` in `order`, leaving out the loading screen. `None`
/// after the last one, or when `current` isn't in the order.
pub(crate) fn scene_after(order: &[String], current: &str) -> Option<String> {
    let order: Vec<&String> = order
        .iter()
        .filter(|scene| scene.as_str() != engine_project::LOADING_SCENE)
        .collect();
    let at = order.iter().position(|scene| scene.as_str() == current)?;
    order.get(at + 1).map(|scene| (*scene).clone())
}

/// Releases the GPU meshes and materials one entity holds, so a mesh shared
/// with other entities survives and an unshared one is freed. Does not despawn.
pub(crate) fn release_entity_assets(world: &World, entity: Entity, assets: &mut RenderAssets) {
    if let Some(renderer) = world.get::<MeshRenderer>(entity) {
        assets.meshes.release(renderer.mesh);
        assets.materials.release(renderer.material);
    }
    if let Some(renderer) = world.get::<SkinnedMeshRenderer>(entity) {
        assets.skinned_meshes.release(renderer.mesh);
        assets.materials.release(renderer.material);
    }
    if let Some(renderer) = world.get::<InstancedMeshRenderer>(entity) {
        assets.meshes.release(renderer.mesh);
        assets.materials.release(renderer.material);
    }
    if let Some(renderer) = world.get::<VegetationRenderer>(entity) {
        assets.meshes.release(renderer.mesh);
        assets.materials.release(renderer.material);
    }
}

/// Every object that survives a scene change: each top-level object marked
/// Keep between scenes, and everything under it.
pub fn kept_objects(world: &World) -> HashSet<Entity> {
    let roots: Vec<Entity> = world
        .iter_entities()
        .filter(|entity| entity.contains::<KeepBetweenScenes>() && !entity.contains::<ChildOf>())
        .map(|entity| entity.id())
        .collect();
    roots
        .into_iter()
        .flat_map(|root| super::objects::subtree(world, root))
        .collect()
}

/// The stable IDs of `entities`.
fn object_ids(world: &World, entities: &HashSet<Entity>) -> HashSet<String> {
    entities
        .iter()
        .filter_map(|entity| world.get::<SceneObjectId>(*entity).map(|id| id.0.clone()))
        .collect()
}

/// The stable IDs of the objects that survive a scene change.
pub fn kept_object_ids(world: &World) -> HashSet<String> {
    object_ids(world, &kept_objects(world))
}

/// The stable IDs of the surviving objects that have a script. A host that
/// keeps script instances keeps these.
pub fn kept_script_objects(world: &World) -> HashSet<String> {
    let scripted: HashSet<Entity> = kept_objects(world)
        .into_iter()
        .filter(|entity| world.get::<Script>(*entity).is_some())
        .collect();
    object_ids(world, &scripted)
}

/// Despawns every non-resource entity and releases its GPU meshes/materials.
///
/// Resources (such as gameplay variables) survive, so values carry across a
/// scene change; the navigation grid belongs to the old scene and is removed.
pub fn clear_world(world: &mut World, assets: &mut RenderAssets) {
    clear_world_except(world, assets, &HashSet::new());
}

/// [`clear_world`], leaving `keep` alone.
pub(super) fn clear_world_except(
    world: &mut World,
    assets: &mut RenderAssets,
    keep: &HashSet<Entity>,
) {
    let resources: HashSet<_> = world
        .resource_entities()
        .iter()
        .map(|(_, entity)| entity)
        .collect();
    let entities: Vec<_> = world
        .iter_entities()
        .map(|entity| entity.id())
        .filter(|entity| !resources.contains(entity) && !keep.contains(entity))
        .collect();
    for entity in &entities {
        release_entity_assets(world, *entity, assets);
    }
    for entity in entities {
        // Despawning a parent already took its children with it.
        if world.get_entity(entity).is_ok() {
            world.despawn(entity);
        }
    }
    world.remove_resource::<SceneNavGrid>();
}

/// Empties the physics world of everything but `keep`'s bodies. With nothing to
/// keep it starts a new world, which also forgets per-world contact state.
pub(super) fn clear_physics_except(
    world: &World,
    physics: &mut PhysicsWorld,
    keep: &HashSet<Entity>,
) {
    if keep.is_empty() {
        reset_physics(physics);
        return;
    }
    let leaving: Vec<Entity> = world
        .iter_entities()
        .map(|entity| entity.id())
        .filter(|entity| !keep.contains(entity))
        .collect();
    for entity in leaving {
        super::objects::remove_physics(world, physics, entity);
    }
}

/// Replaces the physics world with an empty one, keeping its gravity.
pub fn reset_physics(physics: &mut PhysicsWorld) {
    let gravity = physics.rapier.gravity;
    *physics = PhysicsWorld::new(gravity);
}
