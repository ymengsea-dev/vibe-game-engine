//! Play-mode state: preserve the edited scene while the project's game runs.
//!
//! The standalone studio cannot link an arbitrary Rust project into its
//! already-compiled process. The editor executable therefore builds and runs
//! the selected project as a managed child process; this module owns only the
//! editor-side state transition and the pre-play snapshot. Keeping process
//! management out of this library also keeps OS handles out of serializable
//! editor state.

use bevy_ecs::prelude::World;
use engine_scene::Scene;

/// Whether the editor is editing, building, or running the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    /// Panels mutate the world directly; edits are undoable.
    #[default]
    Edit,
    /// The selected project configuration is compiling.
    Building,
    /// The compiled game is running in its own window.
    Play,
}

/// Owns the editor mode and the pre-play scene snapshot.
///
/// The editor binary owns the child process. When that process exits or
/// fails, it calls [`PlayState::stop`] to restore this snapshot exactly.
pub struct PlayState {
    mode: EditorMode,
    snapshot: Option<Scene>,
}

impl Default for PlayState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayState {
    /// Creates an editing play state with no retained snapshot.
    pub fn new() -> Self {
        Self {
            mode: EditorMode::Edit,
            snapshot: None,
        }
    }

    /// Returns the current editor mode.
    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    /// Whether a build or running game currently owns Play mode.
    pub fn is_playing(&self) -> bool {
        self.mode != EditorMode::Edit
    }

    /// Snapshots `world` and enters the building phase.
    ///
    /// This is a no-op when Play is already active, preserving the first
    /// snapshot so repeated button or keyboard events cannot move the rewind
    /// point.
    pub fn start(&mut self, world: &mut World) {
        if self.is_playing() {
            return;
        }
        self.snapshot = Some(Scene::from_world(world));
        self.mode = EditorMode::Building;
    }

    /// Marks a successfully launched project game as running.
    ///
    /// A late launch notification after Stop is ignored.
    pub fn mark_running(&mut self) {
        if self.mode == EditorMode::Building {
            self.mode = EditorMode::Play;
        }
    }

    /// Restores the pre-play snapshot and returns to Edit mode.
    ///
    /// This is a no-op if Play is not active.
    pub fn stop(&mut self, world: &mut World) {
        if self.mode == EditorMode::Edit {
            return;
        }
        if let Some(scene) = self.snapshot.take() {
            let mut fresh = World::new();
            scene.instantiate(&mut fresh);
            *world = fresh;
        }
        self.mode = EditorMode::Edit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::{Name, Transform};
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    #[test]
    fn start_then_stop_restores_the_pre_play_world_exactly() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Box"),
                Transform::from(MathTransform::from_translation(Vec3::ZERO)),
            ))
            .id();
        let mut play = PlayState::new();

        play.start(&mut world);
        assert_eq!(play.mode(), EditorMode::Building);

        world
            .get_mut::<Transform>(entity)
            .expect("transform")
            .0
            .translation
            .x = 42.0;
        world.spawn((
            Name::new("Runtime only"),
            Transform::from(MathTransform::IDENTITY),
        ));

        play.stop(&mut world);
        assert_eq!(play.mode(), EditorMode::Edit);
        assert_eq!(world.query::<&Name>().iter(&world).count(), 1);
        let transform = world
            .query::<&Transform>()
            .iter(&world)
            .next()
            .expect("restored transform");
        assert_eq!(transform.0.translation, Vec3::ZERO);
    }

    #[test]
    fn successful_launch_moves_building_to_play() {
        let mut world = World::new();
        let mut play = PlayState::new();
        play.start(&mut world);
        play.mark_running();
        assert_eq!(play.mode(), EditorMode::Play);
    }

    #[test]
    fn double_start_keeps_the_first_snapshot() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Box"),
                Transform::from(MathTransform::from_translation(Vec3::new(1.0, 0.0, 0.0))),
            ))
            .id();
        let mut play = PlayState::new();

        play.start(&mut world);
        world
            .get_mut::<Transform>(entity)
            .expect("transform")
            .0
            .translation
            .x = 9.0;
        play.start(&mut world);
        play.stop(&mut world);

        let transform = world
            .query::<&Transform>()
            .iter(&world)
            .next()
            .expect("restored transform");
        assert_eq!(transform.0.translation.x, 1.0);
    }

    #[test]
    fn late_launch_after_stop_is_ignored() {
        let mut world = World::new();
        let mut play = PlayState::new();
        play.start(&mut world);
        play.stop(&mut world);
        play.mark_running();
        assert_eq!(play.mode(), EditorMode::Edit);
    }
}
