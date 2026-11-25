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

/// A camera pose exchanged with a paused game: the game's own camera when
/// it pauses, or the free camera Studio looks through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewPose {
    /// World-space eye position.
    pub eye: glam::Vec3,
    /// World-space point looked at.
    pub target: glam::Vec3,
    /// World-space up direction.
    pub up: glam::Vec3,
    /// Vertical field of view in radians; `None` for an orthographic view.
    pub fov_y: Option<f32>,
    /// Visible world height of an orthographic view.
    pub ortho_height: f32,
    /// Near clip plane.
    pub near: f32,
    /// Far clip plane.
    pub far: f32,
}

/// The camera Studio looks through while the game is paused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeCamera {
    /// Moved with the Scene View's navigation.
    pub camera: crate::scene_camera::SceneCamera,
    /// An orthographic (2D) game: pans and zooms, never turns.
    pub two_d: bool,
    /// The game camera's field of view, kept so the view doesn't jump.
    fov_y: Option<f32>,
}

impl FreeCamera {
    /// Starts where `pose` looks from.
    pub fn from_pose(pose: &ViewPose) -> Option<Self> {
        let two_d = pose.fov_y.is_none();
        let camera = if two_d {
            crate::scene_camera::SceneCamera::flat(pose.target, pose.ortho_height)?
        } else {
            crate::scene_camera::SceneCamera::looking(pose.eye, pose.target)?
        };
        Some(Self {
            camera,
            two_d,
            fov_y: pose.fov_y,
        })
    }

    /// The pose to send to the game.
    pub fn pose(&self) -> ViewPose {
        let camera = self.camera.to_camera(self.two_d, 1.0);
        ViewPose {
            eye: camera.eye,
            target: camera.target,
            up: camera.up,
            fov_y: if self.two_d {
                None
            } else {
                Some(self.fov_y.unwrap_or(std::f32::consts::FRAC_PI_4))
            },
            ortho_height: self.camera.ortho_height,
            near: camera.near,
            far: camera.far,
        }
    }
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
    /// The object whose behaviour tree Studio last asked the game to report.
    ai_watch_sent: Option<String>,
    /// The latest report: which object, and what its tree's nodes answered.
    ai_trace: Option<(String, engine_scene::BtTrace)>,
    /// The object whose gameplay graph Studio last asked the game to report.
    graph_watch_sent: Option<String>,
    /// The latest report: which object, and the graph nodes it ran lately.
    graph_trace: Option<(String, Vec<u32>)>,
    /// The game's camera, as it reported when it paused.
    game_camera: Option<ViewPose>,
    /// The free camera, once the user moved the view of a paused game.
    free_camera: Option<FreeCamera>,
    /// A free camera change (`Some(None)`: back to the game's camera) not
    /// yet sent.
    free_camera_update: Option<Option<ViewPose>>,
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
            ai_watch_sent: None,
            ai_trace: None,
            graph_watch_sent: None,
            graph_trace: None,
            game_camera: None,
            free_camera: None,
            free_camera_update: None,
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
        self.ai_watch_sent = None;
        self.ai_trace = None;
        self.graph_watch_sent = None;
        self.graph_trace = None;
        self.game_camera = None;
        self.free_camera = None;
        self.free_camera_update = None;
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

    /// Records the game's acknowledgement of a pause or resume. Resuming
    /// drops the free camera (the game drops it too).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused && self.mode == EditorMode::Play;
        if !self.paused {
            self.game_camera = None;
            self.free_camera = None;
            self.free_camera_update = None;
        }
    }

    /// Keeps the camera the game reported when it paused.
    pub fn set_game_camera(&mut self, pose: ViewPose) {
        if self.paused {
            self.game_camera = Some(pose);
        }
    }

    /// The free camera of a paused game in the Scene View, started from the
    /// game's camera the first time it is asked for.
    pub fn free_camera_mut(&mut self) -> Option<&mut FreeCamera> {
        if !(self.paused && self.is_previewing()) {
            return None;
        }
        if self.free_camera.is_none() {
            self.free_camera = self.game_camera.as_ref().and_then(FreeCamera::from_pose);
        }
        self.free_camera.as_mut()
    }

    /// Whether the view of a paused game is the free camera's.
    pub fn is_free_camera(&self) -> bool {
        self.free_camera.is_some() && self.paused
    }

    /// Sends the free camera's pose to the game on the next frame.
    pub fn free_camera_moved(&mut self) {
        if let Some(free) = &self.free_camera {
            self.free_camera_update = Some(Some(free.pose()));
        }
    }

    /// Goes back to looking through the game's own camera.
    pub fn back_to_game_camera(&mut self) {
        if self.free_camera.take().is_some() {
            self.free_camera_update = Some(None);
        }
    }

    /// The free camera change waiting to be sent: `Some(None)` means the
    /// game's own camera again.
    pub fn take_free_camera_update(&mut self) -> Option<Option<ViewPose>> {
        self.free_camera_update.take()
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

    /// Records which object's behaviour tree Studio wants reported. Returns
    /// the request to send when it differs from the last one sent.
    pub fn watch_ai(&mut self, object_id: Option<String>) -> Option<Option<String>> {
        if self.ai_watch_sent == object_id {
            return None;
        }
        self.ai_watch_sent.clone_from(&object_id);
        if self.ai_trace.as_ref().map(|(id, _)| id) != object_id.as_ref() {
            self.ai_trace = None;
        }
        Some(object_id)
    }

    /// Keeps the game's report of the watched object's behaviour tree.
    pub fn set_ai_trace(&mut self, object_id: String, nodes: engine_scene::BtTrace) {
        if self.ai_watch_sent.as_ref() == Some(&object_id) && self.mode == EditorMode::Play {
            self.ai_trace = Some((object_id, nodes));
        }
    }

    /// Records which object's gameplay graph Studio wants reported. Returns
    /// the request to send when it differs from the last one sent.
    pub fn watch_graph(&mut self, object_id: Option<String>) -> Option<Option<String>> {
        if self.graph_watch_sent == object_id {
            return None;
        }
        self.graph_watch_sent.clone_from(&object_id);
        if self.graph_trace.as_ref().map(|(id, _)| id) != object_id.as_ref() {
            self.graph_trace = None;
        }
        Some(object_id)
    }

    /// Keeps the game's report of the watched object's gameplay graph.
    pub fn set_graph_trace(&mut self, object_id: String, nodes: Vec<u32>) {
        if self.graph_watch_sent.as_ref() == Some(&object_id) && self.mode == EditorMode::Play {
            self.graph_trace = Some((object_id, nodes));
        }
    }

    /// The watched object's gameplay graph nodes that ran lately, while
    /// playing in the Scene View.
    pub fn graph_trace(&self) -> Option<(&str, &[u32])> {
        self.graph_trace
            .as_ref()
            .map(|(id, nodes)| (id.as_str(), nodes.as_slice()))
    }

    /// The watched object's behaviour tree as it ran last, while playing in
    /// the Scene View.
    pub fn ai_trace(&self) -> Option<(&str, &[(u16, engine_scene::BtNodeRun)])> {
        self.ai_trace
            .as_ref()
            .map(|(id, nodes)| (id.as_str(), nodes.as_slice()))
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
        self.ai_watch_sent = None;
        self.ai_trace = None;
        self.graph_watch_sent = None;
        self.graph_trace = None;
        self.game_camera = None;
        self.free_camera = None;
        self.free_camera_update = None;
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

    #[test]
    fn the_watched_tree_is_asked_for_once_and_its_report_kept_while_playing() {
        use engine_scene::BtNodeRun;
        let mut world = World::new();
        let mut play = PlayState::new();
        play.start_in_viewport(&mut world);
        play.mark_running();
        assert_eq!(
            play.watch_ai(Some("guard".into())),
            Some(Some("guard".into()))
        );
        assert_eq!(play.watch_ai(Some("guard".into())), None, "asked once");
        play.set_ai_trace("other".into(), vec![(0, BtNodeRun::Running)]);
        assert!(
            play.ai_trace().is_none(),
            "reports for other objects are dropped"
        );
        play.set_ai_trace("guard".into(), vec![(0, BtNodeRun::Success)]);
        assert_eq!(
            play.ai_trace(),
            Some(("guard", &[(0, BtNodeRun::Success)][..]))
        );
        assert_eq!(play.watch_ai(None), Some(None));
        assert!(
            play.ai_trace().is_none(),
            "a new watch forgets the old report"
        );
        play.stop(&mut world);
        assert_eq!(play.watch_ai(None), None);
    }

    #[test]
    fn a_paused_preview_gets_a_free_camera_from_the_game_camera() {
        let mut world = World::new();
        let mut play = PlayState::new();
        play.start_in_viewport(&mut world);
        play.mark_running();
        let game = ViewPose {
            eye: glam::Vec3::new(0.0, 5.0, 10.0),
            target: glam::Vec3::ZERO,
            up: glam::Vec3::Y,
            fov_y: Some(1.0),
            ortho_height: 10.0,
            near: 0.1,
            far: 500.0,
        };
        play.set_game_camera(game);
        assert!(play.free_camera_mut().is_none(), "only while paused");
        play.set_paused(true);
        play.set_game_camera(game);
        let free = play.free_camera_mut().expect("free camera");
        assert!(!free.two_d);
        let pose = free.pose();
        assert!(
            pose.eye.distance(game.eye) < 1e-3,
            "starts at the game's eye"
        );
        assert_eq!(pose.fov_y, Some(1.0), "keeps the game's lens");
        assert_eq!(
            play.take_free_camera_update(),
            None,
            "nothing to send until it moves"
        );

        if let Some(free) = play.free_camera_mut() {
            free.camera.orbit(glam::Vec2::new(40.0, 0.0));
        }
        play.free_camera_moved();
        assert!(play.is_free_camera());
        assert!(matches!(play.take_free_camera_update(), Some(Some(_))));
        play.back_to_game_camera();
        assert_eq!(play.take_free_camera_update(), Some(None));
        assert!(!play.is_free_camera());

        play.free_camera_mut();
        play.set_paused(false);
        assert!(!play.is_free_camera(), "resuming drops it");
        assert!(play.free_camera_mut().is_none());
    }
}
