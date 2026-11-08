//! Click-to-select in the Scene View: which object is under the pointer.
//!
//! A ray is cast from the camera through the clicked pixel. Each visible
//! mesh is tested in its own local space — first against its bounding box
//! (cheap rejection), then against its actual triangles — so a tree standing
//! inside the terrain's big bounding box is still picked over the terrain.
//! Sprites are tested as their quad. The nearest hit wins. Disabled and
//! locked objects cannot be clicked, so a locked terrain is never grabbed by
//! accident (select it in the Hierarchy instead).

use engine_ecs::components::{Disabled, Lock, MeshSource, SpriteSource};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::Vertex;
use glam::{Mat4, Vec2, Vec3};

use crate::hierarchy::world_transform;

/// A ray in world space; `direction` need not be normalized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    /// Start point (on the near plane).
    pub origin: Vec3,
    /// Direction toward the far plane.
    pub direction: Vec3,
}

impl Ray {
    fn transformed(self, matrix: Mat4) -> Self {
        Self {
            origin: matrix.transform_point3(self.origin),
            direction: matrix.transform_vector3(self.direction),
        }
    }
}

/// The ray through `ndc` (−1..1, +Y up) for a wgpu view-projection
/// (depth 0..1). Works for perspective and orthographic cameras.
pub fn ray_from_screen(view_proj: Mat4, ndc: Vec2) -> Option<Ray> {
    let inverse = view_proj.inverse();
    if !inverse.is_finite() {
        return None;
    }
    let near = inverse.project_point3(ndc.extend(0.0));
    let far = inverse.project_point3(ndc.extend(1.0));
    let direction = far - near;
    (near.is_finite() && direction.is_finite() && direction.length_squared() > 0.0).then_some(Ray {
        origin: near,
        direction,
    })
}

/// Ray parameter where the ray enters the box `[min, max]` (0 if it starts
/// inside), or `None` if it misses. Per-axis slab test; a ray parallel to
/// an axis must start between that axis's planes.
fn hit_box(ray: Ray, min: Vec3, max: Vec3) -> Option<f32> {
    let mut near = 0.0f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        let origin = ray.origin[axis];
        let direction = ray.direction[axis];
        if direction.abs() < 1e-12 {
            if origin < min[axis] || origin > max[axis] {
                return None;
            }
            continue;
        }
        let a = (min[axis] - origin) / direction;
        let b = (max[axis] - origin) / direction;
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return None;
        }
    }
    Some(near)
}

/// Ray parameter of a hit on triangle `a b c` (both faces), Möller–Trumbore.
fn hit_triangle(ray: Ray, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let edge1 = b - a;
    let edge2 = c - a;
    let p = ray.direction.cross(edge2);
    let determinant = edge1.dot(p);
    if determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let s = ray.origin - a;
    let u = s.dot(p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = ray.direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = edge2.dot(q) * inverse;
    (t >= 0.0).then_some(t)
}

/// Nearest triangle hit of `ray` (in the mesh's local space).
fn hit_mesh(ray: Ray, vertices: &[Vertex], indices: &[u32]) -> Option<f32> {
    let position = |index: &u32| {
        vertices
            .get(*index as usize)
            .map(|vertex| Vec3::from_array(vertex.position))
    };
    let first = Vec3::from_array(vertices.first()?.position);
    let (min, max) = vertices.iter().fold((first, first), |(min, max), vertex| {
        let point = Vec3::from_array(vertex.position);
        (min.min(point), max.max(point))
    });
    hit_box(ray, min, max)?;
    indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|triangle| {
            hit_triangle(
                ray,
                position(&triangle[0])?,
                position(&triangle[1])?,
                position(&triangle[2])?,
            )
        })
        .min_by(f32::total_cmp)
}

/// The selectable object nearest along `ray`. See [`pick_hit`].
pub fn pick<'a>(
    world: &mut World,
    ray: Ray,
    mesh_geometry: impl Fn(&MeshSource) -> Option<(&'a [Vertex], &'a [u32])>,
) -> Option<Entity> {
    pick_hit(world, ray, mesh_geometry, LockedObjects::Skip).map(|hit| hit.entity)
}

/// Whether locked objects take part in a pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedObjects {
    /// Selecting: locked objects are passed through, so they are never
    /// grabbed by accident.
    Skip,
    /// Focusing the camera on a point: a locked terrain is still a surface.
    Include,
}

/// Where a pick ray struck an object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickHit {
    /// The object hit.
    pub entity: Entity,
    /// The world-space point hit.
    pub point: Vec3,
}

/// How far past the camera's orbit distance the ground still catches a drop,
/// as a multiple of it. A ray near the horizon meets the ground very far away.
const GROUND_RANGE: f32 = 50.0;

/// Where something dropped along `ray` lands: on the object hit, else on the
/// ground (`y = 0`), else `fallback_distance` in front of the camera.
pub fn placement_point(hit: Option<Vec3>, ray: Ray, fallback_distance: f32) -> Vec3 {
    if let Some(point) = hit {
        return point;
    }
    let to_ground = -ray.origin.y / ray.direction.y;
    if to_ground.is_finite() && to_ground > 0.0 && to_ground <= fallback_distance * GROUND_RANGE {
        return ray.origin + ray.direction * to_ground;
    }
    ray.origin + ray.direction * fallback_distance
}

/// The object nearest along `ray` and the point where it was hit.
/// `mesh_geometry` returns a mesh's imported vertices and indices; objects
/// without loaded geometry (other than sprites) are not hit, nor are
/// disabled ones, nor locked ones unless `locked` says to include them.
pub fn pick_hit<'a>(
    world: &mut World,
    ray: Ray,
    mesh_geometry: impl Fn(&MeshSource) -> Option<(&'a [Vertex], &'a [u32])>,
    locked: LockedObjects,
) -> Option<PickHit> {
    let mut query = world.query::<(
        Entity,
        Option<&MeshSource>,
        Option<&SpriteSource>,
        Option<&Disabled>,
        Option<&Lock>,
        Option<&engine_ecs::components::Hidden>,
    )>();
    let candidates: Vec<(Entity, Option<MeshSource>, Option<[f32; 2]>)> = query
        .iter(world)
        .filter(|(.., disabled, lock, hidden)| {
            disabled.is_none()
                && hidden.is_none()
                && (lock.is_none() || locked == LockedObjects::Include)
        })
        .filter(|(_, mesh, sprite, ..)| mesh.is_some() || sprite.is_some())
        .map(|(entity, mesh, sprite, ..)| (entity, mesh.cloned(), sprite.map(|s| s.size)))
        .collect();
    let mut best: Option<(Entity, f32)> = None;
    for (entity, mesh, sprite) in candidates {
        let model = world_transform(world, entity).to_matrix();
        let inverse = model.inverse();
        if !inverse.is_finite() {
            continue;
        }
        let local = ray.transformed(inverse);
        let hit = if let Some((vertices, indices)) = mesh.as_ref().and_then(&mesh_geometry) {
            hit_mesh(local, vertices, indices)
        } else if let Some([width, height]) = sprite {
            let (x, y) = (width * 0.5, height * 0.5);
            let corners = [
                Vec3::new(-x, -y, 0.0),
                Vec3::new(x, -y, 0.0),
                Vec3::new(x, y, 0.0),
                Vec3::new(-x, y, 0.0),
            ];
            hit_triangle(local, corners[0], corners[1], corners[2])
                .or_else(|| hit_triangle(local, corners[0], corners[2], corners[3]))
        } else {
            None
        };
        // Local and world rays share the parameter, so hits compare directly.
        if let Some(t) = hit
            && best.is_none_or(|(_, nearest)| t < nearest)
        {
            best = Some((entity, t));
        }
    }
    best.map(|(entity, t)| PickHit {
        entity,
        point: ray.origin + ray.direction * t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_lands_on_the_object_then_the_ground_then_in_front_of_the_camera() {
        let down = Ray {
            origin: Vec3::new(1.0, 10.0, 2.0),
            direction: Vec3::NEG_Y,
        };
        let on_object = Vec3::new(1.0, 4.0, 2.0);
        assert_eq!(placement_point(Some(on_object), down, 5.0), on_object);
        assert_eq!(placement_point(None, down, 5.0), Vec3::new(1.0, 0.0, 2.0));

        let up = Ray {
            origin: Vec3::new(0.0, 1.0, 0.0),
            direction: Vec3::Y,
        };
        assert_eq!(placement_point(None, up, 5.0), Vec3::new(0.0, 6.0, 0.0));

        let level = Ray {
            origin: Vec3::new(0.0, 1.0, 0.0),
            direction: Vec3::X,
        };
        assert_eq!(placement_point(None, level, 5.0), Vec3::new(5.0, 1.0, 0.0));

        // Nearly level: the ground is too far away to be what was meant.
        let horizon = Ray {
            origin: Vec3::new(0.0, 100.0, 0.0),
            direction: Vec3::new(1.0, -0.001, 0.0).normalize(),
        };
        let landed = placement_point(None, horizon, 5.0);
        assert!(landed.x < 6.0, "stayed near the camera, got {landed}");
    }
    use engine_ecs::components::Transform;
    use engine_utils::Transform as MathTransform;

    fn cube() -> (Vec<Vertex>, Vec<u32>) {
        let (vertices, indices) = engine_renderer::cube();
        (vertices, indices)
    }

    fn mesh_at(world: &mut World, position: Vec3, scale: f32, id: &str) -> Entity {
        let mut transform = MathTransform::from_translation(position);
        transform.scale = Vec3::splat(scale);
        world
            .spawn((
                Transform::from(transform),
                MeshSource {
                    mesh: id.into(),
                    material: id.into(),
                },
            ))
            .id()
    }

    fn down_from(x: f32, z: f32) -> Ray {
        Ray {
            origin: Vec3::new(x, 50.0, z),
            direction: Vec3::NEG_Y,
        }
    }

    #[test]
    fn the_nearest_mesh_along_the_ray_is_picked() {
        let (vertices, indices) = cube();
        let mut world = World::new();
        let low = mesh_at(&mut world, Vec3::ZERO, 1.0, "low");
        let high = mesh_at(&mut world, Vec3::new(0.0, 5.0, 0.0), 1.0, "high");
        let geometry = |_: &MeshSource| Some((vertices.as_slice(), indices.as_slice()));
        assert_eq!(pick(&mut world, down_from(0.0, 0.0), geometry), Some(high));
        world.entity_mut(high).insert(Disabled);
        assert_eq!(pick(&mut world, down_from(0.0, 0.0), geometry), Some(low));
        assert_eq!(pick(&mut world, down_from(9.0, 9.0), geometry), None);
    }

    #[test]
    fn triangles_not_bounding_boxes_decide_the_hit() {
        // A thin slanted "terrain" triangle whose bounding box encloses a small
        // cube; the ray passes through the box but misses the triangle.
        let mut world = World::new();
        let terrain = mesh_at(&mut world, Vec3::ZERO, 1.0, "terrain");
        let tree = mesh_at(&mut world, Vec3::new(4.0, 2.0, 4.0), 0.5, "tree");
        let (cube_vertices, cube_indices) = cube();
        let vertex = |x: f32, y: f32, z: f32| Vertex {
            position: [x, y, z],
            ..cube_vertices[0]
        };
        let terrain_vertices = vec![
            vertex(-10.0, 0.0, -10.0),
            vertex(10.0, 0.0, -10.0),
            vertex(-10.0, 10.0, 10.0),
        ];
        let terrain_indices = vec![0, 1, 2];
        let geometry = |source: &MeshSource| {
            Some(if source.mesh == "terrain" {
                (terrain_vertices.as_slice(), terrain_indices.as_slice())
            } else {
                (cube_vertices.as_slice(), cube_indices.as_slice())
            })
        };
        assert_eq!(pick(&mut world, down_from(4.0, 4.0), geometry), Some(tree));
        assert_eq!(
            pick(&mut world, down_from(-6.0, -6.0), geometry),
            Some(terrain)
        );
    }

    #[test]
    fn locked_objects_cannot_be_selected_but_still_count_as_surfaces() {
        let (vertices, indices) = cube();
        let mut world = World::new();
        let ground = mesh_at(&mut world, Vec3::ZERO, 1.0, "ground");
        world.entity_mut(ground).insert(Lock);
        let geometry = |_: &MeshSource| Some((vertices.as_slice(), indices.as_slice()));
        assert_eq!(pick(&mut world, down_from(0.0, 0.0), geometry), None);
        let hit = pick_hit(
            &mut world,
            down_from(0.0, 0.0),
            geometry,
            LockedObjects::Include,
        )
        .expect("focus still finds the locked surface");
        assert_eq!(hit.entity, ground);
        assert!(
            hit.point.distance(Vec3::new(0.0, 0.5, 0.0)) < 1e-4,
            "{}",
            hit.point
        );
    }

    #[test]
    fn sprites_are_picked_by_their_quad() {
        let mut world = World::new();
        let sprite = world
            .spawn((
                Transform::from(MathTransform::IDENTITY),
                SpriteSource {
                    size: [2.0, 2.0],
                    ..SpriteSource::default()
                },
            ))
            .id();
        let toward = |x: f32| Ray {
            origin: Vec3::new(x, 0.0, 10.0),
            direction: Vec3::NEG_Z,
        };
        let geometry = |_: &MeshSource| None;
        assert_eq!(pick(&mut world, toward(0.5), geometry), Some(sprite));
        assert_eq!(pick(&mut world, toward(1.5), geometry), None);
    }

    #[test]
    fn the_screen_centre_ray_points_where_the_camera_looks() {
        let camera = engine_renderer::Camera::new(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 1.0);
        let ray = ray_from_screen(camera.view_projection_matrix(), Vec2::ZERO).expect("ray");
        assert!(ray.direction.normalize().distance(Vec3::NEG_Z) < 1e-4);
        assert!(ray.origin.x.abs() < 1e-4 && ray.origin.y.abs() < 1e-4);
        let flat = engine_renderer::Camera::new_orthographic(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            1.0,
            10.0,
        );
        let corner = ray_from_screen(flat.view_projection_matrix(), Vec2::ONE).expect("ray");
        assert!((corner.origin.x - 5.0).abs() < 1e-3 && (corner.origin.y - 5.0).abs() < 1e-3);
    }
}
