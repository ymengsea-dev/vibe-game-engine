//! Scene View navigation: orbit, pan, zoom, fly and frame-selected.
//!
//! [`SceneCamera`] is plain data owned by the editor state, separate from the
//! GPU camera. Input handling changes it; the viewport turns it into a
//! [`Camera`] every frame. Keeping the maths here, away from egui and wgpu,
//! makes every navigation rule unit-testable.

use engine_renderer::Camera;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Vertical field of view of the 3D Scene View (matches [`Camera::new`]).
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
/// Keeps the view from flipping over the poles.
const MAX_PITCH: f32 = 89.0 * std::f32::consts::PI / 180.0;
/// Radians turned per pixel dragged.
const ROTATE_PER_PIXEL: f32 = 0.006;
/// Fraction of the distance zoomed per scroll step (one wheel notch ≈ 1 step).
const ZOOM_PER_STEP: f32 = 0.12;
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 20_000.0;
const MIN_ORTHO_HEIGHT: f32 = 0.1;
/// Below this distance from the focus, zooming in moves the camera forward
/// instead of only closing in, so zoom never stalls at the orbit point.
const DOLLY_DISTANCE: f32 = 1.0;
/// Furthest the camera stays from a point you double-click to focus on.
const FOCUS_DISTANCE: f32 = 12.0;
/// How quickly a glide closes the gap (per second); ~0.3 s to arrive.
const GLIDE_RATE: f32 = 14.0;
const MAX_ORTHO_HEIGHT: f32 = 20_000.0;

/// Where the Scene View looks from. The camera orbits `focus` at `distance`;
/// 2D mode looks straight down −Z at `focus` showing `ortho_height` world units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneCamera {
    /// Orbit pivot and the point the camera looks at.
    pub focus: Vec3,
    /// Rotation around world +Y, radians; 0 looks from +Z toward −Z.
    pub yaw: f32,
    /// Elevation above the horizon, radians.
    pub pitch: f32,
    /// Eye distance from `focus` in 3D.
    pub distance: f32,
    /// Visible world height in 2D.
    pub ortho_height: f32,
}

impl Default for SceneCamera {
    /// The editor's original view: from (4, 3, 6) toward the origin.
    fn default() -> Self {
        let eye = Vec3::new(4.0, 3.0, 6.0);
        let distance = eye.length();
        Self {
            focus: Vec3::ZERO,
            yaw: eye.x.atan2(eye.z),
            pitch: (eye.y / distance).asin(),
            distance,
            ortho_height: 10.0,
        }
    }
}

impl SceneCamera {
    /// Unit vector from `focus` toward the eye.
    fn backward(&self) -> Vec3 {
        Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        )
    }

    /// The 3D eye position.
    pub fn eye(&self) -> Vec3 {
        self.focus + self.backward() * self.distance
    }

    /// Screen-right and screen-up directions in world space for this view.
    fn screen_axes(&self, two_d: bool) -> (Vec3, Vec3) {
        if two_d {
            return (Vec3::X, Vec3::Y);
        }
        let forward = -self.backward();
        let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
        (right, right.cross(forward))
    }

    /// World units covered by one pixel at the focus, for a viewport `height_px` tall.
    fn units_per_pixel(&self, two_d: bool, height_px: f32) -> f32 {
        let visible = if two_d {
            self.ortho_height
        } else {
            2.0 * self.distance * (FOV_Y * 0.5).tan()
        };
        visible / height_px.max(1.0)
    }

    /// The world point under a screen position, on the plane through `focus`
    /// facing the camera. `ndc` is −1..1 across the viewport, +Y up.
    pub fn point_under(&self, ndc: Vec2, two_d: bool, aspect_ratio: f32) -> Vec3 {
        let (right, up) = self.screen_axes(two_d);
        let half_height = if two_d {
            self.ortho_height * 0.5
        } else {
            self.distance * (FOV_Y * 0.5).tan()
        };
        self.focus + right * ndc.x * half_height * aspect_ratio + up * ndc.y * half_height
    }

    /// The GPU camera for this view. Clip planes scale with distance so both
    /// close-up detail and large worlds stay visible.
    pub fn to_camera(&self, two_d: bool, aspect_ratio: f32) -> Camera {
        let mut camera = if two_d {
            Camera::new_orthographic(
                self.focus + Vec3::Z * 10.0,
                self.focus,
                aspect_ratio,
                self.ortho_height,
            )
        } else {
            Camera::new(self.eye(), self.focus, aspect_ratio)
        };
        if !two_d {
            camera.near = (self.distance * 0.001).clamp(0.01, 1.0);
            camera.far = (self.distance * 50.0).max(200.0);
        } else {
            camera.far = 1_000.0;
        }
        camera
    }

    /// Rotates around `focus` by a pointer drag in pixels (3D only).
    pub fn orbit(&mut self, drag: Vec2) {
        self.yaw -= drag.x * ROTATE_PER_PIXEL;
        self.pitch = (self.pitch + drag.y * ROTATE_PER_PIXEL).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Turns the view in place, keeping the eye still (fly-mode mouse look).
    pub fn look(&mut self, drag: Vec2) {
        let eye = self.eye();
        self.orbit(drag);
        self.focus = eye - self.backward() * self.distance;
    }

    /// Slides the view so the scene follows the pointer by `drag` pixels.
    pub fn pan(&mut self, drag: Vec2, two_d: bool, viewport_height_px: f32) {
        let (right, up) = self.screen_axes(two_d);
        let scale = self.units_per_pixel(two_d, viewport_height_px);
        self.focus += (-right * drag.x + up * drag.y) * scale;
    }

    /// Zooms by `steps` (positive = closer), toward `toward` when given —
    /// the world point under the cursor — so what you point at stays put.
    pub fn zoom(&mut self, steps: f32, two_d: bool, toward: Option<Vec3>) {
        if !steps.is_finite() || steps == 0.0 {
            return;
        }
        let factor = (1.0 - ZOOM_PER_STEP).powf(steps);
        let (before, after) = if two_d {
            let before = self.ortho_height;
            self.ortho_height = (before * factor).clamp(MIN_ORTHO_HEIGHT, MAX_ORTHO_HEIGHT);
            (before, self.ortho_height)
        } else {
            let before = self.distance;
            let wanted = before * factor;
            if steps > 0.0 && wanted < DOLLY_DISTANCE {
                // Close in: keep a working distance and carry the whole view
                // forward by the rest, at no less than the speed zoom has at
                // the dolly distance, so the camera passes the orbit point.
                let kept = before.clamp(MIN_DISTANCE, DOLLY_DISTANCE);
                let travel = (before - wanted).max(DOLLY_DISTANCE * (1.0 - factor));
                let forward = travel - (before - kept);
                self.distance = kept;
                self.focus -= self.backward() * forward.max(0.0);
                return;
            }
            self.distance = wanted.clamp(MIN_DISTANCE, MAX_DISTANCE);
            (before, self.distance)
        };
        if let Some(point) = toward.filter(|point| point.is_finite()) {
            // Move the focus along with the zoom so `point` keeps its place on screen.
            let ratio = after / before;
            self.focus = point + (self.focus - point) * ratio;
        }
    }

    /// Moves the camera through the scene: `direction` is (right, up, forward)
    /// in view space, scaled by `seconds`. Speed grows with distance so large
    /// and small scenes both feel responsive; `fast` (Shift) is 4×.
    pub fn fly(&mut self, direction: Vec3, seconds: f32, fast: bool) {
        if direction == Vec3::ZERO || !seconds.is_finite() {
            return;
        }
        let speed = self.distance.max(1.0) * if fast { 4.0 } else { 1.0 };
        let forward = -self.backward();
        let (right, _) = self.screen_axes(false);
        let step = right * direction.x + Vec3::Y * direction.y + forward * direction.z;
        self.focus += step.normalize_or_zero() * speed * seconds.clamp(0.0, 0.1);
    }

    /// Looks at `point` from the current angle, moving closer if far away.
    pub fn focus_on(&mut self, point: Vec3) {
        if !point.is_finite() {
            return;
        }
        self.focus = point;
        self.distance = self.distance.min(FOCUS_DISTANCE);
    }

    /// Moves part of the way toward `target`'s focus and zoom (the angle is
    /// kept). Returns `true` once it has arrived.
    fn approach(&mut self, target: &SceneCamera, fraction: f32) -> bool {
        self.focus = self.focus.lerp(target.focus, fraction);
        self.distance += (target.distance - self.distance) * fraction;
        self.ortho_height += (target.ortho_height - self.ortho_height) * fraction;
        let arrived = self.focus.distance(target.focus) < 1e-3 * target.distance.max(1.0)
            && (self.distance - target.distance).abs() < 1e-3
            && (self.ortho_height - target.ortho_height).abs() < 1e-3;
        if arrived {
            self.focus = target.focus;
            self.distance = target.distance;
            self.ortho_height = target.ortho_height;
        }
        arrived
    }

    /// Centres the view on a sphere and fits it on screen, keeping the angle.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        if !center.is_finite() {
            return;
        }
        let radius = if radius.is_finite() {
            radius.max(0.25)
        } else {
            1.0
        };
        self.focus = center;
        self.distance = (radius / (FOV_Y * 0.5).sin() * 1.15).clamp(MIN_DISTANCE, MAX_DISTANCE);
        self.ortho_height = (radius * 2.4).clamp(MIN_ORTHO_HEIGHT, MAX_ORTHO_HEIGHT);
    }
}

/// The 3D and 2D views, kept separately so switching modes returns you to
/// where you were in each, plus any glide in progress for each.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SceneCameras {
    /// The perspective 3D view.
    pub three_d: SceneCamera,
    /// The orthographic 2D view.
    pub two_d: SceneCamera,
    glide_3d: Option<SceneCamera>,
    glide_2d: Option<SceneCamera>,
}

impl SceneCameras {
    /// The camera for the current mode.
    pub fn active(&self, two_d: bool) -> &SceneCamera {
        if two_d { &self.two_d } else { &self.three_d }
    }

    /// Starts a smooth move of the current mode's camera to `target`.
    pub fn glide_to(&mut self, two_d: bool, target: SceneCamera) {
        *self.glide_mut(two_d) = Some(target);
    }

    /// Stops any glide, as when the user takes control of the camera.
    pub fn stop_glide(&mut self, two_d: bool) {
        *self.glide_mut(two_d) = None;
    }

    /// Advances the current mode's glide by `seconds`. Returns `true` while
    /// the camera is still moving, so the caller keeps redrawing.
    pub fn advance_glide(&mut self, two_d: bool, seconds: f32) -> bool {
        let Some(target) = *self.glide_mut(two_d) else {
            return false;
        };
        let fraction = 1.0 - (-GLIDE_RATE * seconds.clamp(0.0, 0.1)).exp();
        let arrived = self.active_mut(two_d).approach(&target, fraction);
        if arrived {
            *self.glide_mut(two_d) = None;
        }
        !arrived
    }

    fn glide_mut(&mut self, two_d: bool) -> &mut Option<SceneCamera> {
        if two_d {
            &mut self.glide_2d
        } else {
            &mut self.glide_3d
        }
    }

    /// The camera for the current mode, mutably.
    pub fn active_mut(&mut self, two_d: bool) -> &mut SceneCamera {
        if two_d {
            &mut self.two_d
        } else {
            &mut self.three_d
        }
    }
}

/// Where the Scene View was looking, as kept in the editor session so it
/// is the same when the project reopens. Editor-only: the scene file and
/// its unsaved-changes state never see it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedSceneView {
    /// The perspective 3D view.
    pub three_d: SavedSceneCamera,
    /// The orthographic 2D view.
    pub two_d: SavedSceneCamera,
    /// Whether the 2D view was the one showing.
    pub mode_2d: bool,
}

/// One [`SceneCamera`] in [`SavedSceneView`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedSceneCamera {
    /// See [`SceneCamera::focus`].
    pub focus: [f32; 3],
    /// See [`SceneCamera::yaw`].
    pub yaw: f32,
    /// See [`SceneCamera::pitch`].
    pub pitch: f32,
    /// See [`SceneCamera::distance`].
    pub distance: f32,
    /// See [`SceneCamera::ortho_height`].
    pub ortho_height: f32,
}

impl From<SceneCamera> for SavedSceneCamera {
    fn from(camera: SceneCamera) -> Self {
        Self {
            focus: camera.focus.to_array(),
            yaw: camera.yaw,
            pitch: camera.pitch,
            distance: camera.distance,
            ortho_height: camera.ortho_height,
        }
    }
}

impl SavedSceneCamera {
    /// The camera this describes, kept within the navigation limits.
    /// `None` for a value no navigation could produce (not finite) — the
    /// session file is hand-editable, so it is not trusted.
    pub fn restore(self) -> Option<SceneCamera> {
        let finite = self.focus.iter().all(|value| value.is_finite())
            && [self.yaw, self.pitch, self.distance, self.ortho_height]
                .iter()
                .all(|value| value.is_finite());
        finite.then(|| SceneCamera {
            focus: Vec3::from_array(self.focus),
            yaw: self.yaw,
            pitch: self.pitch.clamp(-MAX_PITCH, MAX_PITCH),
            distance: self.distance.clamp(MIN_DISTANCE, MAX_DISTANCE),
            ortho_height: self.ortho_height.clamp(MIN_ORTHO_HEIGHT, MAX_ORTHO_HEIGHT),
        })
    }
}

impl SceneCameras {
    /// Both views for the session; a glide in progress is saved where it
    /// was heading.
    pub fn saved(&self, mode_2d: bool) -> SavedSceneView {
        SavedSceneView {
            three_d: self.glide_3d.unwrap_or(self.three_d).into(),
            two_d: self.glide_2d.unwrap_or(self.two_d).into(),
            mode_2d,
        }
    }

    /// Cameras from a session, with the default view for any that can't
    /// be restored.
    pub fn from_saved(view: &SavedSceneView) -> Self {
        Self {
            three_d: view.three_d.restore().unwrap_or_default(),
            two_d: view.two_d.restore().unwrap_or_default(),
            glide_3d: None,
            glide_2d: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_view_restores_the_same_cameras() {
        let mut cameras = SceneCameras::default();
        cameras.three_d.zoom(3.0, false, None);
        cameras.three_d.orbit(Vec2::new(40.0, 10.0));
        cameras.two_d.pan(Vec2::new(30.0, -12.0), true, 600.0);

        let saved = cameras.saved(true);
        assert!(saved.mode_2d);
        assert_eq!(SceneCameras::from_saved(&saved), cameras);
    }

    #[test]
    fn a_broken_saved_camera_falls_back_to_the_default_view() {
        let mut saved = SceneCameras::default().saved(false);
        saved.three_d.distance = f32::NAN;
        saved.two_d.ortho_height = -5.0;

        let restored = SceneCameras::from_saved(&saved);
        assert_eq!(restored.three_d, SceneCamera::default());
        assert_eq!(restored.two_d.ortho_height, MIN_ORTHO_HEIGHT);
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn default_view_matches_the_original_editor_camera() {
        assert!(close(
            SceneCamera::default().eye(),
            Vec3::new(4.0, 3.0, 6.0)
        ));
    }

    #[test]
    fn orbit_keeps_the_focus_and_distance_and_clamps_pitch() {
        let mut camera = SceneCamera::default();
        let distance = camera.eye().distance(camera.focus);
        camera.orbit(Vec2::new(120.0, 40.0));
        assert_eq!(camera.focus, Vec3::ZERO);
        assert!((camera.eye().distance(camera.focus) - distance).abs() < 1e-3);
        camera.orbit(Vec2::new(0.0, 100_000.0));
        assert!(camera.pitch <= MAX_PITCH);
        assert!(
            camera.to_camera(false, 1.5).is_valid(),
            "no degenerate view at the pole"
        );
    }

    #[test]
    fn look_turns_in_place() {
        let mut camera = SceneCamera::default();
        let eye = camera.eye();
        camera.look(Vec2::new(80.0, -30.0));
        assert!(close(camera.eye(), eye));
        assert_ne!(camera.focus, Vec3::ZERO);
    }

    #[test]
    fn pan_moves_focus_across_the_screen_by_the_dragged_distance() {
        let mut camera = SceneCamera::default();
        // 2D: dragging right by half the view height moves the scene right,
        // so the focus moves left by half the visible height.
        camera.pan(Vec2::new(50.0, 0.0), true, 100.0);
        assert!(close(camera.focus, Vec3::new(-5.0, 0.0, 0.0)));
        camera.pan(Vec2::new(0.0, 50.0), true, 100.0);
        assert!(close(camera.focus, Vec3::new(-5.0, 5.0, 0.0)));

        let mut camera = SceneCamera::default();
        let eye_to_focus = camera.focus - camera.eye();
        camera.pan(Vec2::new(30.0, -10.0), false, 600.0);
        assert!(
            close(camera.focus - camera.eye(), eye_to_focus),
            "angle unchanged"
        );
        assert_ne!(camera.focus, Vec3::ZERO);
    }

    #[test]
    fn zoom_is_bounded_and_keeps_the_pointed_spot_in_place() {
        let mut camera = SceneCamera::default();
        let before = camera.distance;
        camera.zoom(1.0, false, None);
        assert!(camera.distance < before);
        camera.zoom(-1.0, false, None);
        assert!(
            (camera.distance - before).abs() < 1e-3,
            "zoom in then out returns"
        );
        camera.zoom(-10_000.0, false, None);
        assert_eq!(camera.distance, MAX_DISTANCE);
        camera.zoom(f32::NAN, false, None);
        assert_eq!(camera.distance, MAX_DISTANCE);

        let mut flat = SceneCamera::default();
        let spot = Vec3::new(4.0, 2.0, 0.0);
        flat.zoom(3.0, true, Some(spot));
        // The spot's offset from the view centre shrank by the same ratio as the view.
        let ratio = flat.ortho_height / 10.0;
        assert!(close(spot - flat.focus, spot * ratio));
    }

    #[test]
    fn zooming_in_close_keeps_moving_forward_instead_of_stalling() {
        let mut camera = SceneCamera::default();
        let forward = (camera.focus - camera.eye()).normalize();
        let start_eye = camera.eye();
        let mut previous = start_eye;
        for _ in 0..200 {
            camera.zoom(1.0, false, None);
            let eye = camera.eye();
            let step = (eye - previous).dot(forward);
            assert!(
                step >= DOLLY_DISTANCE * ZOOM_PER_STEP * 0.99,
                "never stalls: {step}"
            );
            previous = eye;
        }
        assert_eq!(camera.distance, DOLLY_DISTANCE, "keeps a working distance");
        assert!(
            (camera.eye() - start_eye).dot(forward) > start_eye.length(),
            "flew past the original orbit point"
        );
        let close = camera.distance;
        camera.zoom(-5.0, false, None);
        assert!(camera.distance > close, "zooming out still backs away");
    }

    #[test]
    fn fly_moves_forward_faster_with_shift_and_ignores_idle_input() {
        let mut camera = SceneCamera::default();
        let forward = (camera.focus - camera.eye()).normalize();
        camera.fly(Vec3::Z, 0.05, false);
        let slow = camera.focus;
        assert!(slow.dot(forward) > 0.0);
        let mut fast = SceneCamera::default();
        fast.fly(Vec3::Z, 0.05, true);
        assert!(fast.focus.length() > slow.length() * 3.9);
        let mut idle = SceneCamera::default();
        idle.fly(Vec3::ZERO, 0.05, true);
        assert_eq!(idle, SceneCamera::default());
    }

    #[test]
    fn the_point_under_the_screen_centre_is_the_focus() {
        let camera = SceneCamera::default();
        assert!(close(
            camera.point_under(Vec2::ZERO, false, 1.5),
            camera.focus
        ));
        let flat = SceneCamera::default();
        let corner = flat.point_under(Vec2::new(1.0, 1.0), true, 2.0);
        assert!(close(corner, Vec3::new(10.0, 5.0, 0.0)), "{corner}");
    }

    #[test]
    fn framing_centres_and_fits_the_object() {
        let mut camera = SceneCamera::default();
        camera.frame(Vec3::new(10.0, 1.0, -3.0), 2.0);
        assert_eq!(camera.focus, Vec3::new(10.0, 1.0, -3.0));
        let half_view = camera.distance * (FOV_Y * 0.5).sin();
        assert!(half_view > 2.0, "the whole radius is in view");
        let yaw = camera.yaw;
        camera.frame(Vec3::ZERO, f32::NAN);
        assert_eq!(camera.yaw, yaw, "framing keeps the viewing angle");
        let focus = camera.focus;
        camera.frame(Vec3::splat(f32::INFINITY), 1.0);
        assert_eq!(camera.focus, focus, "invalid centre ignored");
    }

    #[test]
    fn glides_arrive_and_stop_when_the_user_takes_over() {
        let mut cameras = SceneCameras::default();
        let mut target = SceneCamera::default();
        target.focus_on(Vec3::new(30.0, 0.0, 0.0));
        cameras.glide_to(false, target);
        assert!(
            cameras.advance_glide(false, 0.016),
            "still moving after one frame"
        );
        assert!(cameras.three_d.focus.x > 0.0 && cameras.three_d.focus.x < 30.0);
        let mut frames = 0;
        while cameras.advance_glide(false, 0.016) {
            frames += 1;
            assert!(frames < 200, "a glide must finish");
        }
        assert_eq!(cameras.three_d.focus, target.focus);

        cameras.glide_to(false, SceneCamera::default());
        cameras.stop_glide(false);
        assert!(!cameras.advance_glide(false, 0.016));
        assert_eq!(
            cameras.three_d.focus, target.focus,
            "a stopped glide does not move"
        );
    }

    #[test]
    fn focusing_on_a_far_point_moves_closer_but_keeps_the_angle() {
        let mut camera = SceneCamera {
            distance: 80.0,
            ..SceneCamera::default()
        };
        let (yaw, pitch) = (camera.yaw, camera.pitch);
        camera.focus_on(Vec3::new(5.0, 2.0, 1.0));
        assert_eq!(camera.focus, Vec3::new(5.0, 2.0, 1.0));
        assert_eq!(camera.distance, FOCUS_DISTANCE);
        assert_eq!((camera.yaw, camera.pitch), (yaw, pitch));
        camera.focus_on(Vec3::NAN);
        assert_eq!(camera.focus, Vec3::new(5.0, 2.0, 1.0));
    }

    #[test]
    fn modes_keep_their_own_views() {
        let mut cameras = SceneCameras::default();
        cameras
            .active_mut(true)
            .pan(Vec2::new(40.0, 0.0), true, 100.0);
        assert_eq!(cameras.active(false), &SceneCamera::default());
        assert_ne!(cameras.active(true).focus, Vec3::ZERO);
    }
}
