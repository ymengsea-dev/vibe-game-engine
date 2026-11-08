//! Camera pose helpers: a camera's view comes from its object's transform.
//!
//! Scenes written before the transform became the source of truth store the
//! view as world-space `eye`/`target`/`up`. [`adopt_legacy_camera_poses`]
//! folds that into the entity's local transform on load, and
//! [`camera_world_pose`] derives the three vectors back on save.

use glam::{Mat3, Mat4, Quat, Vec3};

use crate::format::{Scene, TransformData};

/// A rotation whose -Z axis is `forward` and whose +Y leans toward `up`.
///
/// When `up` is parallel to `forward` (a top-down camera looking along the
/// world up axis) the result falls back to −Z, then +X, as the up hint, so it
/// stays finite and deterministic. Returns `None` only when `forward` is zero
/// or non-finite.
#[must_use]
pub fn look_rotation(forward: Vec3, up: Vec3) -> Option<Quat> {
    let forward = forward.try_normalize()?;
    let right = [up, Vec3::NEG_Z, Vec3::X]
        .into_iter()
        .filter(|hint| hint.is_finite())
        .map(|hint| forward.cross(hint))
        .find(|cross| cross.length() > 1e-4)?
        .normalize();
    let true_up = right.cross(forward);
    let rotation = Quat::from_mat3(&Mat3::from_cols(right, true_up, -forward));
    rotation.is_finite().then_some(rotation)
}

/// Eye, target (`eye + forward`) and up of a camera whose object has
/// `world_matrix`. The camera looks along -Z with up = +Y; scale is ignored.
#[must_use]
pub fn camera_world_pose(world_matrix: Mat4) -> (Vec3, Vec3, Vec3) {
    let eye = world_matrix.w_axis.truncate();
    let forward = (-world_matrix.z_axis.truncate())
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let up = world_matrix
        .y_axis
        .truncate()
        .try_normalize()
        .unwrap_or(Vec3::Y);
    (eye, eye + forward, up)
}

fn local_matrix(data: Option<&TransformData>) -> Mat4 {
    data.map_or(Mat4::IDENTITY, |t| {
        Mat4::from_scale_rotation_translation(
            Vec3::from_array(t.scale),
            Quat::from_array(t.rotation),
            Vec3::from_array(t.translation),
        )
    })
}

/// World matrix of entity `index`'s parent chain (identity for a root).
fn parent_world(scene: &Scene, index: usize) -> Mat4 {
    let mut chain = Vec::new();
    let mut current = scene.entities.get(index).and_then(|e| e.parent);
    while let Some(parent) = current {
        // Bound the walk so a malformed (cyclic) hierarchy cannot hang.
        if chain.len() > scene.entities.len() {
            break;
        }
        let Some(entity) = scene.entities.get(parent) else {
            break;
        };
        chain.push(parent);
        current = entity.parent;
    }
    chain.iter().rev().fold(Mat4::IDENTITY, |world, &i| {
        world * local_matrix(scene.entities[i].transform.as_ref())
    })
}

fn depth(scene: &Scene, index: usize) -> usize {
    let mut depth = 0;
    let mut current = scene.entities.get(index).and_then(|e| e.parent);
    while let Some(parent) = current {
        depth += 1;
        if depth > scene.entities.len() {
            break;
        }
        current = scene.entities.get(parent).and_then(|e| e.parent);
    }
    depth
}

/// Moves every legacy camera's `eye`/`target`/`up` into its transform.
///
/// The camera's world view is unchanged, including under a moved or rotated
/// parent. Cameras already marked `pose_from_transform` are left alone, so
/// calling this twice is the same as calling it once.
pub fn adopt_legacy_camera_poses(scene: &mut Scene) {
    let mut pending: Vec<usize> = scene
        .entities
        .iter()
        .enumerate()
        .filter(|(_, e)| e.camera.is_some_and(|c| !c.pose_from_transform))
        .map(|(i, _)| i)
        .collect();
    // Parents first, so a camera parented to another legacy camera sees the
    // parent's adopted transform.
    pending.sort_by_key(|&i| depth(scene, i));

    for index in pending {
        let parent = parent_world(scene, index);
        let Some(camera) = scene.entities[index].camera else {
            continue;
        };
        let eye = Vec3::from_array(camera.eye);
        let forward = Vec3::from_array(camera.target) - eye;
        let up = Vec3::from_array(camera.up);
        let existing = scene.entities[index].transform.unwrap_or(TransformData {
            translation: [0.0; 3],
            rotation: Quat::IDENTITY.to_array(),
            scale: [1.0; 3],
        });
        let (world_rotation, degenerate) = match look_rotation(forward, up) {
            Some(rotation) => (rotation, false),
            None => (Quat::IDENTITY, true),
        };
        let desired = Mat4::from_rotation_translation(world_rotation, eye);
        let inverse = parent.inverse();
        let local = if inverse.is_finite() {
            inverse * desired
        } else {
            desired
        };
        let (_, local_rotation, local_translation) = local.to_scale_rotation_translation();
        let rotation = if degenerate {
            tracing::warn!("camera {index} has no usable view direction; keeping its rotation");
            Quat::from_array(existing.rotation)
        } else if local_rotation.is_finite() {
            local_rotation
        } else {
            Quat::from_array(existing.rotation)
        };
        let translation = if local_translation.is_finite() {
            local_translation
        } else {
            Vec3::from_array(existing.translation)
        };
        scene.entities[index].transform = Some(TransformData {
            translation: translation.to_array(),
            rotation: rotation.to_array(),
            scale: existing.scale,
        });
        if let Some(c) = scene.entities[index].camera.as_mut() {
            c.pose_from_transform = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{CameraData, ProjectionData, SceneEntity};

    fn camera(eye: [f32; 3], target: [f32; 3]) -> CameraData {
        CameraData {
            eye,
            target,
            up: [0.0, 1.0, 0.0],
            aspect_ratio: 16.0 / 9.0,
            projection: ProjectionData::Perspective { fov_y_radians: 1.0 },
            near: 0.1,
            far: 100.0,
            pose_from_transform: false,
            active: false,
        }
    }

    fn matrix(t: &TransformData) -> Mat4 {
        local_matrix(Some(t))
    }

    fn assert_near(a: Vec3, b: Vec3) {
        assert!((a - b).length() < 1e-4, "{a:?} != {b:?}");
    }

    #[test]
    fn legacy_camera_pose_is_adopted_into_the_transform() {
        let mut scene = Scene {
            entities: vec![SceneEntity {
                camera: Some(camera([0.0, 2.0, 6.0], [0.0; 3])),
                ..Default::default()
            }],
            ..Default::default()
        };
        adopt_legacy_camera_poses(&mut scene);
        let t = scene.entities[0].transform.unwrap();
        assert_near(Vec3::from_array(t.translation), Vec3::new(0.0, 2.0, 6.0));
        let forward = Quat::from_array(t.rotation) * Vec3::NEG_Z;
        assert_near(forward, Vec3::new(0.0, -2.0, -6.0).normalize());
        assert!(scene.entities[0].camera.unwrap().pose_from_transform);
    }

    #[test]
    fn legacy_camera_under_a_rotated_parent_keeps_its_world_view() {
        let parent = TransformData {
            translation: [10.0, 0.0, 0.0],
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2).to_array(),
            scale: [1.0; 3],
        };
        let mut scene = Scene {
            entities: vec![
                SceneEntity {
                    transform: Some(parent),
                    ..Default::default()
                },
                SceneEntity {
                    parent: Some(0),
                    camera: Some(camera([0.0, 2.0, 6.0], [0.0; 3])),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        adopt_legacy_camera_poses(&mut scene);
        let world = matrix(&parent) * matrix(&scene.entities[1].transform.unwrap());
        let (eye, target, up) = camera_world_pose(world);
        assert_near(eye, Vec3::new(0.0, 2.0, 6.0));
        assert_near(
            (target - eye).normalize(),
            Vec3::new(0.0, -2.0, -6.0).normalize(),
        );
        assert_near(up.normalize(), up);
    }

    #[test]
    fn straight_down_camera_with_up_y_still_gets_a_rotation() {
        let rotation = look_rotation(Vec3::NEG_Y, Vec3::Y).expect("fallback up axis");
        assert_near(rotation * Vec3::NEG_Z, Vec3::NEG_Y);
        assert!(look_rotation(Vec3::Y, Vec3::Y).is_some());
        assert!(look_rotation(Vec3::ZERO, Vec3::Y).is_none());
        assert!(look_rotation(Vec3::new(f32::NAN, 0.0, 0.0), Vec3::Y).is_none());
    }

    #[test]
    fn adoption_is_idempotent() {
        let mut scene = Scene {
            entities: vec![SceneEntity {
                camera: Some(camera([1.0, 2.0, 3.0], [0.0; 3])),
                ..Default::default()
            }],
            ..Default::default()
        };
        adopt_legacy_camera_poses(&mut scene);
        let once = scene.clone();
        adopt_legacy_camera_poses(&mut scene);
        assert_eq!(scene, once);
    }

    #[test]
    fn degenerate_legacy_camera_keeps_rotation() {
        let mut scene = Scene {
            entities: vec![SceneEntity {
                camera: Some(camera([1.0, 2.0, 3.0], [1.0, 2.0, 3.0])),
                ..Default::default()
            }],
            ..Default::default()
        };
        adopt_legacy_camera_poses(&mut scene);
        let t = scene.entities[0].transform.unwrap();
        assert_eq!(t.translation, [1.0, 2.0, 3.0]);
        assert_eq!(t.rotation, Quat::IDENTITY.to_array());
        assert!(t.rotation.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn old_ron_without_new_fields_loads_and_adopts() {
        let ron = "(version: 5, entities: [(camera: Some((eye: (0.0, 2.0, 6.0), \
                   target: (0.0, 0.0, 0.0), up: (0.0, 1.0, 0.0), aspect_ratio: 1.5, \
                   projection: Perspective(fov_y_radians: 1.0), near: 0.1, far: 50.0)))])";
        let scene = Scene::from_ron_str(ron).unwrap();
        assert_eq!(scene.version, crate::CURRENT_SCENE_VERSION);
        let c = scene.entities[0].camera.unwrap();
        assert!(c.pose_from_transform);
        assert!(!c.active);
        assert_eq!(
            scene.entities[0].transform.unwrap().translation,
            [0.0, 2.0, 6.0]
        );
    }
}
