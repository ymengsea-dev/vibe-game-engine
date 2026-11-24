//! World streaming: world areas ([`engine_scene::SceneStreamArea`]) load
//! their scene on top of the current one while the camera is near, and
//! unload it once the camera has gone.
//!
//! At most one area loads per frame, the nearest first, so a walk into a
//! crowded spot spreads its loading over several frames. Scenes and model
//! files are kept after an area unloads, so coming back doesn't read them
//! again.

use std::collections::{HashMap, HashSet};

use engine_ecs::components::{Disabled, GlobalTransform};
use engine_ecs::prelude::Without;
use engine_scene::{PlacedArea, Scene, SceneStreamArea, StreamAreaData, plan_streaming};
use glam::Vec3;

use super::{GameplayError, GameplayRuntime, SceneLoad};
use crate::app::GameContext;

/// Most imported model files kept between area loads. Past this the cache
/// starts over, so a world with many different models doesn't hold them all.
const MAX_CACHED_IMPORTS: usize = 32;

/// What streaming keeps between loads of the same areas.
#[derive(Default)]
pub(super) struct StreamCache {
    /// Area scenes as read from the scene source, by path.
    scenes: HashMap<String, Scene>,
    /// Model files imported by any scene loaded on top.
    pub(super) imports: engine_scene::ImportCache,
}

impl StreamCache {
    /// Drops the model files once there are too many.
    pub(super) fn trim(&mut self) {
        if self.imports.len() > MAX_CACHED_IMPORTS {
            self.imports.clear();
        }
    }
}

impl GameplayRuntime {
    /// The scenes streaming has loaded on top of the current one, in no
    /// particular order.
    pub fn streamed_areas(&self) -> impl Iterator<Item = &str> {
        self.streamed.iter().map(String::as_str)
    }

    /// Queues this frame's area loads and unloads, measured from the camera
    /// as it was last presented. [`Self::apply_scene_loads`] calls it.
    pub(super) fn queue_streaming(&mut self, ctx: &mut GameContext<'_>) {
        // A script or rule may have unloaded a streamed scene itself.
        let loaded: HashSet<&str> = self
            .loaded_scenes
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        self.streamed.retain(|path| loaded.contains(path.as_str()));
        let eye = ctx.camera_mut().eye;
        let world = ctx.world_mut();
        let areas: Vec<(StreamAreaData, Vec3)> = world
            .query_filtered::<(&GlobalTransform, &SceneStreamArea), Without<Disabled>>()
            .iter(world)
            .map(|(transform, area)| (area.0.clone(), transform.0.translation))
            .collect();
        if areas.is_empty() && self.streamed.is_empty() {
            return;
        }
        let placed: Vec<PlacedArea<'_>> = areas
            .iter()
            .map(|(data, centre)| PlacedArea {
                data,
                centre: *centre,
            })
            .collect();
        let plan = plan_streaming(&placed, eye, &self.streamed, |scene| {
            self.scene_path.as_deref() == Some(scene) || loaded.contains(scene)
        });
        for path in plan.unload {
            self.streamed.remove(&path);
            self.scene_loads.push_back(SceneLoad::Unload(path));
        }
        if let Some(path) = plan.load {
            self.streamed.insert(path.clone());
            self.scene_loads.push_back(SceneLoad::Load(path));
        }
    }

    /// The scene at `path`: from the cache when streaming read it before,
    /// else from the scene source (and cached when streaming loads it).
    pub(super) fn scene_to_load(&mut self, path: &str) -> Result<Scene, GameplayError> {
        if !self.streamed.contains(path) {
            return self.load_scene(path);
        }
        if let Some(scene) = self.stream_cache.scenes.get(path) {
            return Ok(scene.clone());
        }
        let scene = self.load_scene(path)?;
        self.stream_cache
            .scenes
            .insert(path.to_owned(), scene.clone());
        Ok(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_cached_models_start_over() {
        let mut cache = StreamCache::default();
        cache.trim();
        assert!(cache.imports.is_empty());
        assert!(cache.scenes.is_empty());
    }

    #[test]
    fn a_runtime_starts_with_nothing_streamed() {
        let runtime = GameplayRuntime::new(Scene::default(), None);
        assert_eq!(runtime.streamed_areas().count(), 0);
    }
}
