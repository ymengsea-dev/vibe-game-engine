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
    /// The compiled game is running, in its own window or in the Scene
    /// View (see [`PlayKind`]).
    Play,
}

/// Where a running game is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayKind {
    /// Run: the game's own window, exactly as players get it.
    Window,
    /// Play: inside the Scene View, with Pause and Step.
    Viewport,
}

/// A request from the Scene View's Play controls to the running preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameControl {
    /// Freeze the game.
    Pause,
    /// Continue a paused game.
    Resume,
    /// Advance a paused game by one frame.
    Step,
}

/// Owns the editor mode and the pre-play scene snapshot.
///
/// The editor binary owns the child process. When that process exits or
/// fails, it calls [`PlayState::stop`] to restore this snapshot exactly.
pub struct PlayState {
    mode: EditorMode,
    kind: PlayKind,
    /// Whether the previewed game confirmed it is paused.
    paused: bool,
    /// Controls waiting for the editor binary to send them.
    controls: Vec<GameControl>,
    /// Whether the build is compiling code, not just checking that nothing
    /// changed (set by the editor binary from Cargo's output).
    compiling: bool,
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
            kind: PlayKind::Window,
            paused: false,
            controls: Vec::new(),
            compiling: false,
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

    /// Snapshots `world` and enters the building phase for Run (the
    /// game's own window).
    ///
    /// This is a no-op when Play is already active, preserving the first
    /// snapshot so repeated button or keyboard events cannot move the rewind
    /// point.
    pub fn start(&mut self, world: &mut World) {
        self.start_as(world, PlayKind::Window);
    }

    /// Like [`PlayState::start`], for a preview inside the Scene View.
    pub fn start_in_viewport(&mut self, world: &mut World) {
        self.start_as(world, PlayKind::Viewport);
    }

    fn start_as(&mut self, world: &mut World, kind: PlayKind) {
        if self.is_playing() {
            return;
        }
        self.snapshot = Some(Scene::from_world(world));
        self.mode = EditorMode::Building;
        self.kind = kind;
        self.paused = false;
        self.controls.clear();
        self.compiling = false;
    }

    /// Records whether the build is compiling code right now.
    pub fn set_compiling(&mut self, compiling: bool) {
        self.compiling = compiling && self.mode == EditorMode::Building;
    }

    /// Whether the game is being compiled, as opposed to checked and
    /// started. Only then is "Building" shown.
    pub fn is_compiling(&self) -> bool {
        self.compiling && self.mode == EditorMode::Building
    }

    /// Where the game is shown, while one is building or running.
    pub fn kind(&self) -> Option<PlayKind> {
        self.is_playing().then_some(self.kind)
    }

    /// Whether a game is building or running in the Scene View.
    pub fn is_previewing(&self) -> bool {
        self.kind() == Some(PlayKind::Viewport)
    }

    /// Whether the previewed game is paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Records the game's acknowledgement of a pause or resume.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused && self.mode == EditorMode::Play;
    }

    /// Queues `control` for a running preview. Ignored when it does not
    /// apply: nothing is previewing, Pause while paused, Resume or Step
    /// while running.
    pub fn request(&mut self, control: GameControl) {
        let applies = match control {
            GameControl::Pause => !self.paused,
            GameControl::Resume | GameControl::Step => self.paused,
        };
        if applies && self.is_previewing() && self.mode == EditorMode::Play {
            self.controls.push(control);
        }
    }

    /// The queued controls, oldest first, for the editor binary to send.
    pub fn take_controls(&mut self) -> Vec<GameControl> {
        std::mem::take(&mut self.controls)
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
        self.paused = false;
        self.controls.clear();
        self.compiling = false;
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

    #[test]
    fn preview_controls_queue_only_when_they_apply() {
        let mut world = World::new();
        let mut play = PlayState::new();
        play.request(GameControl::Pause);
        assert!(play.take_controls().is_empty(), "nothing is running");

        play.start_in_viewport(&mut world);
        assert_eq!(play.kind(), Some(PlayKind::Viewport));
        play.request(GameControl::Pause);
        assert!(play.take_controls().is_empty(), "still building");

        play.mark_running();
        play.request(GameControl::Step);
        play.request(GameControl::Pause);
        assert_eq!(
            play.take_controls(),
            [GameControl::Pause],
            "Step needs a pause"
        );
        play.set_paused(true);
        play.request(GameControl::Pause);
        play.request(GameControl::Step);
        play.request(GameControl::Resume);
        assert_eq!(
            play.take_controls(),
            [GameControl::Step, GameControl::Resume]
        );

        play.stop(&mut world);
        assert!(!play.is_paused());
        assert_eq!(play.kind(), None);
        play.start(&mut world);
        assert_eq!(play.kind(), Some(PlayKind::Window), "Run never previews");
    }

    #[test]
    fn building_is_shown_only_while_code_compiles() {
        let mut world = World::new();
        let mut play = PlayState::new();
        play.set_compiling(true);
        assert!(!play.is_compiling(), "nothing is building");
        play.start_in_viewport(&mut world);
        assert!(
            !play.is_compiling(),
            "checking, not compiling, until Cargo says so"
        );
        play.set_compiling(true);
        assert!(play.is_compiling());
        play.mark_running();
        assert!(!play.is_compiling(), "a running game is not building");
    }
}
