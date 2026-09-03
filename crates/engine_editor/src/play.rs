//! Play-mode: run the scene instead of editing it, then rewind.
//!
//! [`PlayState::start`] snapshots the world as a [`Scene`], then each
//! frame [`PlayState::tick`] runs a `bevy_ecs` [`Schedule`] over it;
//! [`PlayState::stop`] restores the pre-play snapshot exactly. So play
//! testing never disturbs the scene being edited.
//!
//! The schedule ships with one demo system (`demo_spin`) so Play
//! visibly *does* something today — a game embedding the editor would
//! register its own gameplay systems into the schedule instead. Wiring
//! that registration through is future work (it needs the Stage 5
//! gameplay-API layer to have systems worth running).

use bevy_ecs::prelude::{Query, World};
use bevy_ecs::schedule::{Schedule, ScheduleLabel};
use engine_ecs::components::{Name, Transform};
use engine_scene::Scene;
use glam::Quat;

#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PlayUpdate;

/// Whether the editor is editing the scene or running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    /// Panels mutate the world directly; edits are undoable.
    #[default]
    Edit,
    /// The play schedule ticks the world each frame; the pre-play state
    /// is held for restore on Stop.
    Play,
}

/// Owns the play schedule and the pre-play scene snapshot.
pub struct PlayState {
    mode: EditorMode,
    /// While playing, whether the schedule is currently frozen. Always
    /// `false` in [`EditorMode::Edit`].
    paused: bool,
    schedule: Schedule,
    snapshot: Option<Scene>,
}

impl Default for PlayState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayState {
    /// A fresh play state in [`EditorMode::Edit`], with the demo system
    /// registered.
    pub fn new() -> Self {
        let mut schedule = Schedule::new(PlayUpdate);
        schedule.add_systems(demo_spin);
        Self {
            mode: EditorMode::Edit,
            paused: false,
            schedule,
            snapshot: None,
        }
    }

    /// The current mode.
    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    /// Whether the editor is currently in [`EditorMode::Play`].
    pub fn is_playing(&self) -> bool {
        self.mode == EditorMode::Play
    }

    /// Whether play is currently frozen by [`PlayState::toggle_pause`].
    /// Always `false` while editing.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Snapshots `world` and enters [`EditorMode::Play`]. No-op if already
    /// playing.
    pub fn start(&mut self, world: &mut World) {
        if self.mode == EditorMode::Play {
            return;
        }
        self.snapshot = Some(Scene::from_world(world));
        self.mode = EditorMode::Play;
        self.paused = false;
    }

    /// Restores `world` from the snapshot taken by [`PlayState::start`]
    /// and returns to [`EditorMode::Edit`]. No-op if not playing.
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
        self.paused = false;
    }

    /// Toggles the paused state while playing; a no-op in
    /// [`EditorMode::Edit`]. While paused, [`PlayState::tick`] does
    /// nothing, so the running scene freezes in place.
    pub fn toggle_pause(&mut self) {
        if self.is_playing() {
            self.paused = !self.paused;
        }
    }

    /// Runs one tick of the play schedule over `world` — only while
    /// playing and not paused.
    pub fn tick(&mut self, world: &mut World) {
        if self.mode == EditorMode::Play && !self.paused {
            self.schedule.run(world);
        }
    }
}

/// Demo gameplay system: spins every entity named `"Spinner"` about `+Y`
/// a little each tick, so Play mode has something visible to show before
/// real game systems are wired in.
fn demo_spin(mut query: Query<(&Name, &mut Transform)>) {
    for (name, mut transform) in &mut query {
        if name.0 == "Spinner" {
            transform.0.rotation = (transform.0.rotation * Quat::from_rotation_y(0.03)).normalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::World;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    #[test]
    fn start_then_stop_restores_the_pre_play_world() {
        let mut world = World::new();
        world.spawn((
            Name::new("Box"),
            Transform::from(MathTransform::from_translation(Vec3::ZERO)),
        ));
        let mut play = PlayState::new();

        play.start(&mut world);
        assert!(play.is_playing());

        // Mutate during play.
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 42.0;
        }

        play.stop(&mut world);
        assert!(!play.is_playing());
        let mut query = world.query::<&Transform>();
        assert_eq!(query.iter(&world).next().unwrap().0.translation.x, 0.0);
    }

    #[test]
    fn tick_does_nothing_in_edit_mode() {
        let mut world = World::new();
        let spinner = world
            .spawn((
                Name::new("Spinner"),
                Transform::from(MathTransform::IDENTITY),
            ))
            .id();
        let mut play = PlayState::new();

        play.tick(&mut world);
        assert_eq!(
            world.get::<Transform>(spinner).unwrap().0.rotation,
            MathTransform::IDENTITY.rotation
        );
    }

    #[test]
    fn tick_runs_the_demo_system_while_playing() {
        let mut world = World::new();
        let spinner = world
            .spawn((
                Name::new("Spinner"),
                Transform::from(MathTransform::IDENTITY),
            ))
            .id();
        let other = world
            .spawn((
                Name::new("Static"),
                Transform::from(MathTransform::IDENTITY),
            ))
            .id();
        let mut play = PlayState::new();

        play.start(&mut world);
        play.tick(&mut world);

        assert_ne!(
            world.get::<Transform>(spinner).unwrap().0.rotation,
            MathTransform::IDENTITY.rotation,
            "the Spinner rotates"
        );
        assert_eq!(
            world.get::<Transform>(other).unwrap().0.rotation,
            MathTransform::IDENTITY.rotation,
            "other entities are untouched"
        );
    }

    #[test]
    fn toggle_pause_freezes_and_resumes_the_schedule() {
        let mut world = World::new();
        let spinner = world
            .spawn((
                Name::new("Spinner"),
                Transform::from(MathTransform::IDENTITY),
            ))
            .id();
        let mut play = PlayState::new();
        play.start(&mut world);

        play.toggle_pause();
        assert!(play.is_paused());
        play.tick(&mut world);
        assert_eq!(
            world.get::<Transform>(spinner).unwrap().0.rotation,
            MathTransform::IDENTITY.rotation,
            "a paused schedule does not tick"
        );

        play.toggle_pause();
        assert!(!play.is_paused());
        play.tick(&mut world);
        assert_ne!(
            world.get::<Transform>(spinner).unwrap().0.rotation,
            MathTransform::IDENTITY.rotation,
            "resuming ticks again"
        );
    }

    #[test]
    fn toggle_pause_is_a_noop_while_editing() {
        let mut play = PlayState::new();
        play.toggle_pause();
        assert!(!play.is_paused());
    }

    #[test]
    fn stop_clears_the_paused_flag() {
        let mut world = World::new();
        world.spawn((Name::new("Box"), Transform::from(MathTransform::IDENTITY)));
        let mut play = PlayState::new();
        play.start(&mut world);
        play.toggle_pause();
        assert!(play.is_paused());

        play.stop(&mut world);
        assert!(!play.is_paused());
    }

    #[test]
    fn double_start_keeps_the_first_snapshot() {
        let mut world = World::new();
        world.spawn((
            Name::new("Box"),
            Transform::from(MathTransform::from_translation(Vec3::new(1.0, 0.0, 0.0))),
        ));
        let mut play = PlayState::new();

        play.start(&mut world);
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 9.0;
        }
        play.start(&mut world); // ignored — still playing, snapshot unchanged
        play.stop(&mut world);

        let mut query = world.query::<&Transform>();
        assert_eq!(query.iter(&world).next().unwrap().0.translation.x, 1.0);
    }
}
