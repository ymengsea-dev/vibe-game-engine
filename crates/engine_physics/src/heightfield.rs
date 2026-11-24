//! Terrain collision: a rapier heightfield built from a row-major grid of
//! heights.
//!
//! The grid is laid out as the engine's terrain is: row `i` along Z,
//! column `j` along X, `heights[i * cols + j]`, centred on the collider's
//! origin. Every cell is split on its top-left to bottom-right diagonal,
//! the same split the drawn terrain uses, so the ground you stand on is
//! the ground you see. Cells marked absent (gaps between terrain tiles)
//! have no triangles.

use glam::{Vec2, Vec3};
use rapier3d::parry::shape::{HeightField, HeightFieldCellStatus, HeightFieldFlags, SharedShape};
use rapier3d::parry::utils::Array2;
use rapier3d::prelude::ColliderBuilder;

use crate::error::PhysicsError;

/// A heightfield collider for a `rows` x `cols` grid of `heights` spaced
/// `cell` apart (X, Z), centred on the collider's origin. `cell_present`
/// holds `(rows - 1) * (cols - 1)` row-major flags; `false` cells are open.
///
/// # Errors
///
/// [`PhysicsError::InvalidHeightfield`] for fewer than 2 rows or columns,
/// mismatched lengths, a non-finite height or a bad cell size.
pub fn heightfield_collider(
    heights: &[f32],
    rows: usize,
    cols: usize,
    cell: Vec2,
    cell_present: &[bool],
) -> Result<ColliderBuilder, PhysicsError> {
    let invalid = |reason: &'static str| Err(PhysicsError::InvalidHeightfield(reason));
    if rows < 2 || cols < 2 {
        return invalid("a heightfield needs at least 2 rows and columns");
    }
    if heights.len() != rows * cols || cell_present.len() != (rows - 1) * (cols - 1) {
        return invalid("heightfield data doesn't match its size");
    }
    if !heights.iter().all(|h| h.is_finite()) {
        return invalid("heightfield heights must be finite");
    }
    if !(cell.is_finite() && cell.min_element() > 0.0) {
        return invalid("heightfield cell size must be above zero");
    }
    // Parry stores the grid column-major.
    let data: Vec<f32> = (0..cols)
        .flat_map(|j| (0..rows).map(move |i| heights[i * cols + j]))
        .collect();
    let scale = Vec3::new((cols - 1) as f32 * cell.x, 1.0, (rows - 1) as f32 * cell.y);
    let mut field = HeightField::with_flags(
        Array2::new(rows, cols, data),
        scale,
        HeightFieldFlags::FIX_INTERNAL_EDGES,
    );
    for i in 0..rows - 1 {
        for j in 0..cols - 1 {
            let mut status = HeightFieldCellStatus::ZIGZAG_SUBDIVISION;
            if !cell_present[i * (cols - 1) + j] {
                status |= HeightFieldCellStatus::CELL_REMOVED;
            }
            field.set_cell_status(i, j, status);
        }
    }
    Ok(ColliderBuilder::new(SharedShape::new(field)))
}

#[cfg(test)]
mod tests {
    use rapier3d::prelude::RigidBodyBuilder;

    use super::*;
    use crate::{CharacterController, PhysicsWorld, cast_ray, move_character};

    fn ground(
        physics: &mut PhysicsWorld,
        heights: &[f32],
        rows: usize,
        cols: usize,
        present: &[bool],
    ) {
        let collider = heightfield_collider(heights, rows, cols, Vec2::ONE, present).unwrap();
        physics.rapier.insert(RigidBodyBuilder::fixed(), collider);
        physics.step(0.0).unwrap();
    }

    fn height_below(physics: &PhysicsWorld, x: f32, z: f32) -> Option<f32> {
        height_below_except(physics, x, z, None)
    }

    fn height_below_except(
        physics: &PhysicsWorld,
        x: f32,
        z: f32,
        except: Option<crate::ColliderHandle>,
    ) -> Option<f32> {
        cast_ray(physics, Vec3::new(x, 50.0, z), Vec3::NEG_Y, 100.0, except)
            .unwrap()
            .map(|hit| hit.point.y)
    }

    #[test]
    fn a_ramp_along_x_reads_back_along_x_so_the_grid_is_not_transposed() {
        // 3 rows (Z) by 5 columns (X); height = column index. Centred, so
        // column 0 sits at x = -2.
        let (rows, cols) = (3, 5);
        let heights: Vec<f32> = (0..rows * cols).map(|n| (n % cols) as f32).collect();
        let mut physics = PhysicsWorld::default();
        ground(&mut physics, &heights, rows, cols, &[true; 8]);
        for x in [-1.5f32, 0.0, 1.25] {
            let expected = x + 2.0;
            let got = height_below(&physics, x, 0.3).unwrap();
            assert!((got - expected).abs() < 1e-3, "x {x}: {got} vs {expected}");
        }
    }

    #[test]
    fn cells_split_on_the_same_diagonal_as_the_drawn_terrain() {
        // One cell, only the bottom-right node (row 1, col 1) raised: on
        // the top-left to bottom-right diagonal the centre is at half
        // height, and the upper-right triangle's plane is h = tz.
        let mut physics = PhysicsWorld::default();
        ground(&mut physics, &[0.0, 0.0, 0.0, 1.0], 2, 2, &[true]);
        assert!((height_below(&physics, 0.0, 0.0).unwrap() - 0.5).abs() < 1e-3);
        // tx = 0.8, tz = 0.2 (x right of centre, z near the top row).
        assert!((height_below(&physics, 0.3, -0.3).unwrap() - 0.2).abs() < 1e-3);
    }

    #[test]
    fn a_missing_cell_lets_rays_through() {
        let mut physics = PhysicsWorld::default();
        ground(&mut physics, &[0.0; 9], 3, 3, &[true, false, true, true]);
        assert!(height_below(&physics, -0.5, -0.5).is_some());
        assert!(height_below(&physics, 0.5, -0.5).is_none());
    }

    #[test]
    fn a_ball_comes_to_rest_on_a_slope() {
        let (rows, cols) = (9, 9);
        let heights: Vec<f32> = (0..rows * cols).map(|n| (n / cols) as f32 * 0.25).collect();
        let mut physics = PhysicsWorld::default();
        ground(&mut physics, &heights, rows, cols, &[true; 64]);
        let (ball, ball_collider) = physics.rapier.insert(
            // Locked rotation so it slides to a stop instead of rolling.
            RigidBodyBuilder::dynamic()
                .translation(Vec3::new(0.0, 5.0, 0.0))
                .lock_rotations(),
            ColliderBuilder::ball(0.5).friction(5.0),
        );
        for _ in 0..240 {
            physics.step(1.0 / 60.0).unwrap();
        }
        let position = physics.rapier.bodies[ball].translation();
        let surface =
            height_below_except(&physics, position.x, position.z, Some(ball_collider)).unwrap();
        // On a slope of 0.25 the ball's centre sits r / cos(angle) above
        // the surface straight below it.
        let lift = 0.5 * (1.0 + 0.25f32 * 0.25).sqrt();
        assert!(
            (position.y - surface - lift).abs() < 0.05,
            "ball at {position:?}, ground {surface}, expected lift {lift}"
        );
    }

    #[test]
    fn a_character_walks_across_and_stays_grounded() {
        let (rows, cols) = (11, 11);
        let heights: Vec<f32> = (0..rows * cols)
            .map(|n| ((n % cols) as f32 * 0.6).sin() * 0.3)
            .collect();
        let mut physics = PhysicsWorld::default();
        ground(&mut physics, &heights, rows, cols, &[true; 100]);
        let start = height_below(&physics, -4.0, 0.0).unwrap();
        let (body, collider) = physics.rapier.insert(
            RigidBodyBuilder::kinematic_position_based().translation(Vec3::new(
                -4.0,
                start + 0.81,
                0.0,
            )),
            ColliderBuilder::capsule_y(0.5, 0.3),
        );
        physics.step(0.0).unwrap();
        let controller = CharacterController {
            snap_to_ground: Some(crate::CharacterLength::Absolute(0.3)),
            ..CharacterController::default()
        };
        let mut grounded = 0;
        for _ in 0..120 {
            let movement = move_character(
                &mut physics,
                &controller,
                body,
                collider,
                Vec3::new(0.06, -0.05, 0.0),
                1.0 / 60.0,
            )
            .unwrap();
            grounded += usize::from(movement.grounded);
            physics.step(1.0 / 60.0).unwrap();
        }
        let end = physics.rapier.bodies[body].translation();
        assert!(end.x > 2.0, "walked to {end:?}");
        let floor = height_below_except(&physics, end.x, end.z, Some(collider)).unwrap();
        assert!(
            end.y - floor > 0.6 && end.y - floor < 1.0,
            "{end:?} over {floor}"
        );
        assert!(grounded > 100, "grounded on {grounded} of 120 frames");
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(heightfield_collider(&[0.0; 2], 1, 2, Vec2::ONE, &[]).is_err());
        assert!(heightfield_collider(&[0.0; 4], 2, 2, Vec2::ONE, &[]).is_err());
        assert!(
            heightfield_collider(&[f32::NAN, 0.0, 0.0, 0.0], 2, 2, Vec2::ONE, &[true]).is_err()
        );
        assert!(heightfield_collider(&[0.0; 4], 2, 2, Vec2::ZERO, &[true]).is_err());
    }
}
