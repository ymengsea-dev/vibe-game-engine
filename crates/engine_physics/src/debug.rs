//! Physics debug visualization: line segments representing colliders,
//! rigid body axes, joints, and (optionally) contacts, extracted from
//! rapier's own `debug-render` feature.
//!
//! This crate has no rendering dependency of its own (see the crate-level
//! docs) — [`debug_render_lines`] hands back plain line segments with
//! linear RGBA colors; turning those into GPU-drawable geometry is a
//! renderer concern (see `engine_renderer::DebugLineVertex`).

use glam::Vec3;
use rapier3d::pipeline::{DebugRenderBackend, DebugRenderObject, DebugRenderPipeline};
pub use rapier3d::pipeline::{DebugRenderMode, DebugRenderStyle};

use crate::world::PhysicsWorld;

/// A single colored line segment for debug visualization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugLine {
    /// World-space start point.
    pub start: Vec3,
    /// World-space end point.
    pub end: Vec3,
    /// Linear RGBA, `[0,1]` per channel — converted from rapier's own
    /// HSLA debug-color convention (standard HSL-to-RGB, alpha passed
    /// through unchanged).
    pub color: [f32; 4],
}

/// Extracts every debug line rapier's own [`DebugRenderPipeline`] would
/// draw for `physics`'s current state, as plain line segments ready to
/// hand to a renderer.
///
/// `mode` selects what gets drawn — [`DebugRenderMode::default`] draws
/// collider shapes, rigid body axes, and joints (not contacts or AABBs,
/// which get visually noisy fast); [`DebugRenderMode::all`] draws
/// everything.
pub fn debug_render_lines(
    physics: &PhysicsWorld,
    style: DebugRenderStyle,
    mode: DebugRenderMode,
) -> Vec<DebugLine> {
    struct Collector {
        lines: Vec<DebugLine>,
    }

    impl DebugRenderBackend for Collector {
        fn draw_line(&mut self, _object: DebugRenderObject<'_>, a: Vec3, b: Vec3, color: [f32; 4]) {
            self.lines.push(DebugLine {
                start: a,
                end: b,
                color: hsla_to_linear_rgba(color),
            });
        }
    }

    let mut collector = Collector { lines: Vec::new() };
    let mut pipeline = DebugRenderPipeline::new(style, mode);
    pipeline.render(
        &mut collector,
        &physics.rapier.bodies,
        &physics.rapier.colliders,
        &physics.rapier.impulse_joints,
        &physics.rapier.multibody_joints,
        &physics.rapier.narrow_phase,
    );
    collector.lines
}

/// Converts rapier's HSLA debug-color convention (`[hue 0..=360,
/// saturation 0..=1, lightness 0..=1, alpha 0..=1]`) into linear RGBA —
/// the standard HSL-to-RGB algorithm, extended with a passthrough alpha
/// channel.
fn hsla_to_linear_rgba(hsla: [f32; 4]) -> [f32; 4] {
    let [h, s, l, a] = hsla;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    [r1 + m, g1 + m, b1 + m, a]
}

#[cfg(test)]
mod tests {
    use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder};

    use super::*;

    #[test]
    fn red_hue_converts_correctly() {
        let rgba = hsla_to_linear_rgba([0.0, 1.0, 0.5, 1.0]);
        assert!((rgba[0] - 1.0).abs() < 1e-5);
        assert!(rgba[1].abs() < 1e-5);
        assert!(rgba[2].abs() < 1e-5);
        assert!((rgba[3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn green_hue_converts_correctly() {
        let rgba = hsla_to_linear_rgba([120.0, 1.0, 0.5, 1.0]);
        assert!(rgba[0].abs() < 1e-5);
        assert!((rgba[1] - 1.0).abs() < 1e-5);
        assert!(rgba[2].abs() < 1e-5);
    }

    #[test]
    fn blue_hue_converts_correctly() {
        let rgba = hsla_to_linear_rgba([240.0, 1.0, 0.5, 1.0]);
        assert!(rgba[0].abs() < 1e-5);
        assert!(rgba[1].abs() < 1e-5);
        assert!((rgba[2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn zero_lightness_is_black_regardless_of_hue() {
        let rgba = hsla_to_linear_rgba([180.0, 1.0, 0.0, 1.0]);
        assert!(rgba[0].abs() < 1e-5);
        assert!(rgba[1].abs() < 1e-5);
        assert!(rgba[2].abs() < 1e-5);
    }

    #[test]
    fn full_lightness_is_white_regardless_of_hue_or_saturation() {
        let rgba = hsla_to_linear_rgba([50.0, 0.7, 1.0, 1.0]);
        assert!((rgba[0] - 1.0).abs() < 1e-5);
        assert!((rgba[1] - 1.0).abs() < 1e-5);
        assert!((rgba[2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn zero_saturation_is_gray_regardless_of_hue() {
        let rgba = hsla_to_linear_rgba([90.0, 0.0, 0.5, 1.0]);
        assert!((rgba[0] - 0.5).abs() < 1e-5);
        assert!((rgba[1] - 0.5).abs() < 1e-5);
        assert!((rgba[2] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn alpha_passes_through_unchanged() {
        let rgba = hsla_to_linear_rgba([0.0, 1.0, 0.5, 0.25]);
        assert!((rgba[3] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn out_of_range_hue_wraps_instead_of_panicking() {
        let rgba = hsla_to_linear_rgba([720.0, 1.0, 0.5, 1.0]);
        let expected = hsla_to_linear_rgba([0.0, 1.0, 0.5, 1.0]);
        assert!((rgba[0] - expected[0]).abs() < 1e-5);
        assert!((rgba[1] - expected[1]).abs() < 1e-5);
        assert!((rgba[2] - expected[2]).abs() < 1e-5);
    }

    #[test]
    fn negative_hue_wraps_instead_of_panicking() {
        let rgba = hsla_to_linear_rgba([-360.0, 1.0, 0.5, 1.0]);
        let expected = hsla_to_linear_rgba([0.0, 1.0, 0.5, 1.0]);
        assert!((rgba[0] - expected[0]).abs() < 1e-5);
        assert!((rgba[1] - expected[1]).abs() < 1e-5);
        assert!((rgba[2] - expected[2]).abs() < 1e-5);
    }

    #[test]
    fn empty_world_produces_no_debug_lines() {
        let physics = PhysicsWorld::default();
        let lines = debug_render_lines(
            &physics,
            DebugRenderStyle::default(),
            DebugRenderMode::default(),
        );
        assert!(lines.is_empty());
    }

    #[test]
    fn a_collider_produces_at_least_one_debug_line() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed(),
            ColliderBuilder::cuboid(1.0, 1.0, 1.0),
        );

        let lines = debug_render_lines(
            &physics,
            DebugRenderStyle::default(),
            DebugRenderMode::COLLIDER_SHAPES,
        );
        assert!(!lines.is_empty());
    }

    #[test]
    fn an_empty_mode_produces_no_lines_even_with_geometry_present() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed(),
            ColliderBuilder::cuboid(1.0, 1.0, 1.0),
        );

        let lines = debug_render_lines(
            &physics,
            DebugRenderStyle::default(),
            DebugRenderMode::empty(),
        );
        assert!(lines.is_empty());
    }
}
