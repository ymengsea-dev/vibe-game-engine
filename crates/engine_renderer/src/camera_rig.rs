//! [`CameraRig`] — a third-person follow/orbit camera.
//!
//! Holds a look-at `target` (the character), an orbit `yaw`/`pitch`, a
//! `distance` and `height`, and a `follow_speed`. Each frame the game
//! points `target` at the character, optionally calls [`CameraRig::orbit`]
//! from input, and calls [`CameraRig::apply`] to drive a [`Camera`]: the
//! eye smoothly chases the rig's ideal position and the look point tracks
//! the target. Pure math — no `wgpu`, no per-frame allocation.

use glam::Vec3;

use crate::camera::Camera;

/// How far, in radians, [`CameraRig::pitch`] may tilt above or below the
/// horizon. Keeps the camera from flipping over the top or through the
/// floor.
const PITCH_LIMIT: f32 = 1.4;

/// A third-person camera rig: an orbit around a moving `target`, with
/// smoothed follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraRig {
    /// World point the camera looks at the base of (usually the
    /// character's feet). The actual look point is this plus `height` on
    /// `+Y` — see [`CameraRig::look_point`].
    pub target: Vec3,
    /// Distance from the look point to the eye.
    pub distance: f32,
    /// How far above `target` the look point sits.
    pub height: f32,
    /// Orbit angle around the target, radians. `0.0` places the eye
    /// directly behind the target on `+Z`.
    pub yaw: f32,
    /// Downward tilt, radians. Positive raises the eye and looks down;
    /// clamped by [`CameraRig::orbit`] to `[-1.4, 1.4]`.
    pub pitch: f32,
    /// Exponential follow rate, per second. Higher snaps the eye to its
    /// ideal position faster; `0.0` freezes the eye in place.
    pub follow_speed: f32,
}

impl CameraRig {
    /// A rig looking at `target` with sensible third-person defaults: 6
    /// units back, 1.5 up, a slight downward tilt, and a brisk follow.
    pub fn new(target: Vec3) -> Self {
        Self {
            target,
            distance: 6.0,
            height: 1.5,
            yaw: 0.0,
            pitch: 0.35,
            follow_speed: 8.0,
        }
    }

    /// Adds `delta_yaw` / `delta_pitch` to the orbit angles, wrapping yaw
    /// into `[-PI, PI)` and clamping pitch to `[-1.4, 1.4]`.
    pub fn orbit(&mut self, delta_yaw: f32, delta_pitch: f32) {
        let tau = std::f32::consts::TAU;
        self.yaw =
            (self.yaw + delta_yaw + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
        self.pitch = (self.pitch + delta_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// The world point the camera aims at: `target` lifted by `height`.
    pub fn look_point(&self) -> Vec3 {
        self.target + Vec3::Y * self.height
    }

    /// Where the eye wants to be this frame — `distance` from
    /// [`CameraRig::look_point`] along the current yaw/pitch orbit
    /// direction (behind and, for positive pitch, above the target).
    pub fn desired_eye(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let offset = Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch);
        self.look_point() + offset * self.distance
    }

    /// Points `camera` at this rig: the eye exponentially chases
    /// [`CameraRig::desired_eye`] over `dt` seconds (frame-rate
    /// independent), and the look target snaps to
    /// [`CameraRig::look_point`].
    ///
    /// `dt <= 0` (or non-finite) leaves the eye where it is; a very large
    /// `dt` or `follow_speed` snaps it exactly onto the ideal position.
    pub fn apply(&self, camera: &mut Camera, dt: f32) {
        let rate = self.follow_speed.max(0.0) * dt.max(0.0);
        let blend = if rate.is_finite() {
            (1.0 - (-rate).exp()).clamp(0.0, 1.0)
        } else {
            1.0
        };
        camera.eye = camera.eye.lerp(self.desired_eye(), blend);
        camera.target = self.look_point();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_camera() -> Camera {
        Camera::new(Vec3::ZERO, Vec3::ZERO, 1.0)
    }

    #[test]
    fn new_has_sane_defaults() {
        let rig = CameraRig::new(Vec3::new(1.0, 0.0, 2.0));
        assert!(rig.distance > 0.0);
        assert!(rig.follow_speed > 0.0);
        assert!(rig.pitch.abs() <= PITCH_LIMIT);
    }

    #[test]
    fn look_point_lifts_the_target_by_height() {
        let mut rig = CameraRig::new(Vec3::new(3.0, 0.0, -1.0));
        rig.height = 2.0;
        assert_eq!(rig.look_point(), Vec3::new(3.0, 2.0, -1.0));
    }

    #[test]
    fn desired_eye_sits_directly_behind_at_zero_orbit() {
        let mut rig = CameraRig::new(Vec3::ZERO);
        rig.yaw = 0.0;
        rig.pitch = 0.0;
        rig.height = 0.0;
        rig.distance = 5.0;
        assert!((rig.desired_eye() - Vec3::new(0.0, 0.0, 5.0)).length() < 1e-5);
    }

    #[test]
    fn pitch_raises_the_eye_and_shortens_the_ground_distance() {
        let mut rig = CameraRig::new(Vec3::ZERO);
        rig.height = 0.0;
        rig.distance = 4.0;

        rig.pitch = 0.0;
        let flat = rig.desired_eye();
        rig.pitch = 0.6;
        let tilted = rig.desired_eye();

        assert!(tilted.y > flat.y);
        let ground = |p: Vec3| (p.x * p.x + p.z * p.z).sqrt();
        assert!(ground(tilted) < ground(flat));
    }

    #[test]
    fn distance_scales_the_offset_linearly() {
        let mut rig = CameraRig::new(Vec3::ZERO);
        rig.height = 0.0;
        rig.distance = 2.0;
        let near = rig.desired_eye();
        rig.distance = 6.0;
        let far = rig.desired_eye();
        assert!((far.length() - 3.0 * near.length()).abs() < 1e-4);
    }

    #[test]
    fn orbit_wraps_yaw_and_clamps_pitch() {
        let mut rig = CameraRig::new(Vec3::ZERO);
        rig.yaw = 3.0;
        rig.orbit(1.0, 0.0); // 4.0 -> wrapped below PI
        assert!(rig.yaw >= -std::f32::consts::PI && rig.yaw < std::f32::consts::PI);

        rig.pitch = 0.0;
        rig.orbit(0.0, 10.0);
        assert_eq!(rig.pitch, PITCH_LIMIT);
        rig.orbit(0.0, -100.0);
        assert_eq!(rig.pitch, -PITCH_LIMIT);
    }

    #[test]
    fn a_full_yaw_turn_returns_to_the_same_eye() {
        let mut rig = CameraRig::new(Vec3::new(2.0, 1.0, -3.0));
        let start = rig.desired_eye();
        rig.orbit(std::f32::consts::TAU, 0.0);
        assert!((rig.desired_eye() - start).length() < 1e-4);
    }

    #[test]
    fn apply_with_zero_dt_does_not_move_the_eye() {
        let rig = CameraRig::new(Vec3::ZERO);
        let mut camera = stub_camera();
        camera.eye = Vec3::new(100.0, 100.0, 100.0);
        rig.apply(&mut camera, 0.0);
        assert_eq!(camera.eye, Vec3::new(100.0, 100.0, 100.0));
        // Look target still tracks.
        assert_eq!(camera.target, rig.look_point());
    }

    #[test]
    fn apply_with_a_huge_step_snaps_to_the_ideal_eye() {
        let rig = CameraRig::new(Vec3::new(1.0, 0.0, 1.0));
        let mut camera = stub_camera();
        camera.eye = Vec3::new(-50.0, -50.0, -50.0);
        rig.apply(&mut camera, 1_000.0);
        assert!((camera.eye - rig.desired_eye()).length() < 1e-3);
    }

    #[test]
    fn apply_converges_over_repeated_steps() {
        let rig = CameraRig::new(Vec3::new(4.0, 0.0, -2.0));
        let mut camera = stub_camera();
        camera.eye = Vec3::ZERO;
        for _ in 0..600 {
            rig.apply(&mut camera, 1.0 / 60.0);
        }
        assert!((camera.eye - rig.desired_eye()).length() < 1e-3);
    }
}
