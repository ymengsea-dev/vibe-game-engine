//! Read-only reverse asset dependency reporting for the editor.

use std::collections::BTreeMap;
use std::path::Path;

use engine_ecs::components::{AssetSource, MeshSource, SpriteSource};
use engine_ecs::prelude::{Entity, World};
use engine_scene::{ColliderShape, SceneAudioEmitter, SceneCollider, SceneFoliage, SceneTilemap};

use crate::assets::{AssetIndex, parse_asset_id};

/// One component reference to an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyUse {
    /// Entity carrying the reference.
    pub entity: Entity,
    /// Human-readable component/field name.
    pub field: &'static str,
}

/// All scene users of one stable asset id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetDependency {
    /// Canonical UUID text.
    pub id: String,
    /// Current indexed path, if the asset still exists.
    pub path: Option<std::path::PathBuf>,
    /// Components that reference it.
    pub users: Vec<DependencyUse>,
}

impl AssetDependency {
    /// True when the stable id cannot currently be resolved to a file.
    pub fn is_missing(&self) -> bool {
        self.path.is_none()
    }
}

/// Builds a bounded reverse dependency report from authoring components.
pub fn scan(world: &mut World, index: &AssetIndex) -> Vec<AssetDependency> {
    let mut report: BTreeMap<String, AssetDependency> = BTreeMap::new();
    let mut add = |entity: Entity, field: &'static str, text: &str| {
        let Some(id) = parse_asset_id(text) else {
            return;
        };
        let key = id.to_string();
        let entry = report
            .entry(key.clone())
            .or_insert_with(|| AssetDependency {
                id: key,
                path: index.path_for(id).map(Path::to_path_buf),
                users: Vec::new(),
            });
        entry.users.push(DependencyUse { entity, field });
    };
    for (entity, source) in world.query::<(Entity, &AssetSource)>().iter(world) {
        if let Some(id) = source.id.as_deref() {
            add(entity, "AssetSource.path", id);
        }
    }
    for (entity, source) in world.query::<(Entity, &MeshSource)>().iter(world) {
        add(entity, "MeshSource.mesh", &source.mesh);
        add(entity, "MeshSource.material", &source.material);
    }
    for (entity, source) in world.query::<(Entity, &SceneFoliage)>().iter(world) {
        add(entity, "Foliage.mesh", &source.0.mesh.id);
        add(entity, "Foliage.material", &source.0.material.id);
    }
    for (entity, source) in world.query::<(Entity, &SpriteSource)>().iter(world) {
        add(entity, "SpriteSource.atlas", &source.atlas);
    }
    for (entity, source) in world.query::<(Entity, &SceneTilemap)>().iter(world) {
        add(entity, "Tilemap.atlas", &source.0.atlas.id);
    }
    for (entity, emitter) in world.query::<(Entity, &SceneAudioEmitter)>().iter(world) {
        add(entity, "AudioEmitter.sound", &emitter.0.sound.id);
    }
    for (entity, collider) in world.query::<(Entity, &SceneCollider)>().iter(world) {
        if let ColliderShape::TriMesh { mesh } = &collider.0.shape {
            add(entity, "Collider.mesh", &mesh.id);
        }
    }
    report.into_values().collect()
}

/// Returns only dependencies that currently have no indexed file.
pub fn missing(world: &mut World, index: &AssetIndex) -> Vec<AssetDependency> {
    scan(world, index)
        .into_iter()
        .filter(AssetDependency::is_missing)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::{MeshSource, Name};
    use engine_scene::{AssetRef, AudioEmitterData};

    #[test]
    fn foliage_model_is_reported_as_a_missing_asset_dependency() {
        let id = engine_asset::AssetId::new().to_string();
        let mut world = World::new();
        world.spawn(engine_scene::SceneFoliage(engine_scene::FoliageData {
            mesh: AssetRef { id: id.clone() },
            material: AssetRef { id: id.clone() },
            instances: Vec::new(),
        }));
        let report = missing(&mut world, &AssetIndex::new());
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].id, id);
        assert_eq!(report[0].users.len(), 2);
    }

    #[test]
    fn scan_finds_unique_component_users_and_ignores_invalid_ids() {
        let mut world = World::new();
        let id = uuid::Uuid::new_v4().to_string();
        let entity = world
            .spawn((Name::new("Tree"), MeshSource::new(&id, &id)))
            .id();
        world.spawn(SceneAudioEmitter(AudioEmitterData::new(AssetRef {
            id: "bad".into(),
        })));
        let report = scan(&mut world, &AssetIndex::new());
        assert_eq!(report.len(), 1);
        assert!(report[0].is_missing());
        assert_eq!(report[0].users.len(), 2);
        assert!(report[0].users.iter().all(|user| user.entity == entity));
    }
}
