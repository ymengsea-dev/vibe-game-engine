//! A procedurally-generated walking character.
//!
//! Everything here — the skinned mesh, the skeleton, and the walk cycle —
//! is built in code, matching the slice's no-asset-files decision. It is
//! deliberately simple: a body box and two leg boxes bound to a
//! three-joint skeleton, with a clip that swings the legs and bobs the
//! body.
//!
//! Enough to prove the animation system end to end — sampling, skinning,
//! upload, and footstep events landing on the frames the feet plant.
//! It is not a character model, and it will look like what it is.

use std::collections::HashMap;
use std::sync::Arc;

use engine::asset::{
    ImportedAnimation, ImportedAnimationChannels, ImportedInterpolation, ImportedJoint,
    ImportedKeyframes, ImportedSkeleton,
};
use engine::prelude::{SkinnedVertex, Transform};
use glam::{Quat, Vec3};

/// Joint indices. The body is the root; each leg hangs off it.
const JOINT_BODY: usize = 0;
const JOINT_LEG_LEFT: usize = 1;
const JOINT_LEG_RIGHT: usize = 2;

/// How long one full walk cycle takes, in seconds. Both feet plant once.
pub const WALK_DURATION: f32 = 0.9;

/// Overall height of the character, in world units.
pub const CHARACTER_HEIGHT: f32 = 1.7;

/// Where the legs attach, measured up from the feet.
const HIP_HEIGHT: f32 = 0.85;

/// The character's skeleton: a body root with two leg joints.
///
/// Joints sit at their bind position; the clip rotates them about it, so
/// a leg swings from the hip rather than sliding.
pub fn skeleton() -> Arc<ImportedSkeleton> {
    let joint = |name: &str, index: usize, parent: Option<usize>, at: Vec3| ImportedJoint {
        name: Some(name.to_string()),
        node_index: index,
        parent,
        local_bind_transform: Transform::from_translation(at),
        // The inverse bind matrix undoes the joint's rest pose, so
        // skinning transforms vertices *relative* to where the joint
        // rests rather than stacking the rest pose on twice.
        inverse_bind_matrix: glam::Mat4::from_translation(-at),
    };

    Arc::new(ImportedSkeleton {
        name: Some("character".into()),
        joints: vec![
            joint("body", JOINT_BODY, None, Vec3::new(0.0, HIP_HEIGHT, 0.0)),
            joint(
                "leg_left",
                JOINT_LEG_LEFT,
                Some(JOINT_BODY),
                Vec3::new(-0.16, 0.0, 0.0),
            ),
            joint(
                "leg_right",
                JOINT_LEG_RIGHT,
                Some(JOINT_BODY),
                Vec3::new(0.16, 0.0, 0.0),
            ),
        ],
    })
}

/// Appends a box to a skinned mesh, every vertex bound fully to `joint`.
///
/// Rigid binding (one joint, weight 1) rather than smooth skinning: the
/// character is blocky by design, and a hard bind makes the joint
/// rotations obvious, which is the point of a test character.
fn push_box(
    vertices: &mut Vec<SkinnedVertex>,
    indices: &mut Vec<u32>,
    centre: Vec3,
    half: Vec3,
    joint: u32,
) {
    // Each face gets its own four vertices so normals stay flat.
    let faces: [(Vec3, [Vec3; 4]); 6] = [
        (
            Vec3::Z,
            [
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_Z,
            [
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::X,
            [
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_X,
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::Y,
            [
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::NEG_Y,
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, 1.0),
            ],
        ),
    ];

    for (normal, corners) in faces {
        let base = vertices.len() as u32;
        let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        for (corner, uv) in corners.iter().zip(uvs) {
            vertices.push(SkinnedVertex {
                position: (centre + *corner * half).to_array(),
                normal: normal.to_array(),
                uv,
                joints: [joint, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The character's skinned mesh: a body bound to the root joint and one
/// leg bound to each leg joint.
pub fn mesh() -> (Vec<SkinnedVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    // Body, from the hips up.
    push_box(
        &mut vertices,
        &mut indices,
        Vec3::new(0.0, HIP_HEIGHT + 0.42, 0.0),
        Vec3::new(0.24, 0.42, 0.16),
        JOINT_BODY as u32,
    );
    // Head.
    push_box(
        &mut vertices,
        &mut indices,
        Vec3::new(0.0, CHARACTER_HEIGHT - 0.16, 0.0),
        Vec3::new(0.17, 0.17, 0.17),
        JOINT_BODY as u32,
    );
    // Legs, hanging from the hips to the ground.
    for (joint, x) in [(JOINT_LEG_LEFT, -0.16), (JOINT_LEG_RIGHT, 0.16)] {
        push_box(
            &mut vertices,
            &mut indices,
            Vec3::new(x, HIP_HEIGHT * 0.5, 0.0),
            Vec3::new(0.10, HIP_HEIGHT * 0.5, 0.10),
            joint as u32,
        );
    }

    (vertices, indices)
}

/// Builds a rotation channel from `(time, angle)` pairs about `axis`.
fn swing_channel(axis: Vec3, keys: &[(f32, f32)]) -> ImportedAnimationChannels {
    ImportedAnimationChannels {
        translation: None,
        rotation: Some(ImportedKeyframes {
            interpolation: ImportedInterpolation::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: keys
                .iter()
                .map(|(_, angle)| Quat::from_axis_angle(axis, *angle))
                .collect(),
        }),
        scale: None,
    }
}

/// A walk cycle: legs swinging in opposition, body bobbing twice per
/// cycle (once per footfall).
pub fn walk_clip() -> Arc<ImportedAnimation> {
    let quarter = WALK_DURATION * 0.25;
    let half = WALK_DURATION * 0.5;
    let swing = 0.55_f32;

    let mut channels = HashMap::new();

    // Left leg forward first, right leg opposite.
    channels.insert(
        JOINT_LEG_LEFT,
        swing_channel(
            Vec3::X,
            &[(0.0, swing), (half, -swing), (WALK_DURATION, swing)],
        ),
    );
    channels.insert(
        JOINT_LEG_RIGHT,
        swing_channel(
            Vec3::X,
            &[(0.0, -swing), (half, swing), (WALK_DURATION, -swing)],
        ),
    );

    // Body bob: up between footfalls, down as each foot plants. Uses
    // translation rather than rotation so the whole character rises.
    channels.insert(
        JOINT_BODY,
        ImportedAnimationChannels {
            translation: Some(ImportedKeyframes {
                interpolation: ImportedInterpolation::Linear,
                times: vec![0.0, quarter, half, quarter * 3.0, WALK_DURATION],
                values: vec![
                    Vec3::new(0.0, HIP_HEIGHT, 0.0),
                    Vec3::new(0.0, HIP_HEIGHT + 0.06, 0.0),
                    Vec3::new(0.0, HIP_HEIGHT, 0.0),
                    Vec3::new(0.0, HIP_HEIGHT + 0.06, 0.0),
                    Vec3::new(0.0, HIP_HEIGHT, 0.0),
                ],
            }),
            rotation: None,
            scale: None,
        },
    );

    Arc::new(ImportedAnimation {
        name: Some("walk".into()),
        duration: WALK_DURATION,
        channels,
    })
}

/// The frames each foot plants, for footstep audio.
///
/// A foot is down when its leg passes through vertical, which is halfway
/// through each half-cycle — so at one quarter and three quarters.
pub fn footstep_times() -> [f32; 2] {
    [WALK_DURATION * 0.25, WALK_DURATION * 0.75]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_has_a_root_and_two_children() {
        let skeleton = skeleton();
        assert_eq!(skeleton.joints.len(), 3);
        assert!(skeleton.joints[JOINT_BODY].parent.is_none());
        assert_eq!(skeleton.joints[JOINT_LEG_LEFT].parent, Some(JOINT_BODY));
        assert_eq!(skeleton.joints[JOINT_LEG_RIGHT].parent, Some(JOINT_BODY));
    }

    #[test]
    fn mesh_is_well_formed_and_fully_weighted() {
        let (vertices, indices) = mesh();
        assert!(!vertices.is_empty());
        assert_eq!(indices.len() % 3, 0);
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
        for vertex in &vertices {
            let total: f32 = vertex.weights.iter().sum();
            assert!(
                (total - 1.0).abs() < 1e-5,
                "skin weights must sum to 1, got {total}",
            );
            assert!(
                (vertex.joints[0] as usize) < 3,
                "vertex bound to a joint that does not exist",
            );
        }
    }

    #[test]
    fn every_joint_is_bound_by_some_vertex() {
        let (vertices, _) = mesh();
        for joint in 0..3u32 {
            assert!(
                vertices.iter().any(|v| v.joints[0] == joint),
                "joint {joint} drives no geometry, so animating it would do nothing",
            );
        }
    }

    #[test]
    fn walk_clip_covers_every_joint_and_loops_cleanly() {
        let clip = walk_clip();
        assert!((clip.duration - WALK_DURATION).abs() < 1e-6);
        assert_eq!(clip.channels.len(), 3);

        // First and last keyframes must match or the loop pops.
        for (joint, channels) in &clip.channels {
            if let Some(rotation) = &channels.rotation {
                let first = rotation.values.first().expect("keyframes");
                let last = rotation.values.last().expect("keyframes");
                assert!(
                    first.abs_diff_eq(*last, 1e-5),
                    "joint {joint}'s rotation does not return to its start",
                );
            }
            if let Some(translation) = &channels.translation {
                let first = translation.values.first().expect("keyframes");
                let last = translation.values.last().expect("keyframes");
                assert!(
                    first.abs_diff_eq(*last, 1e-5),
                    "joint {joint}'s translation does not return to its start",
                );
            }
        }
    }

    #[test]
    fn keyframe_times_ascend_and_stay_in_the_clip() {
        let clip = walk_clip();
        for channels in clip.channels.values() {
            for times in [
                channels.rotation.as_ref().map(|k| &k.times),
                channels.translation.as_ref().map(|k| &k.times),
            ]
            .into_iter()
            .flatten()
            {
                assert!(
                    times.windows(2).all(|w| w[0] < w[1]),
                    "keyframe times must strictly ascend: {times:?}",
                );
                assert!(times.iter().all(|&t| (0.0..=WALK_DURATION).contains(&t)));
            }
        }
    }

    #[test]
    fn legs_swing_in_opposition() {
        let clip = walk_clip();
        let left = clip.channels[&JOINT_LEG_LEFT]
            .rotation
            .as_ref()
            .expect("left leg rotates");
        let right = clip.channels[&JOINT_LEG_RIGHT]
            .rotation
            .as_ref()
            .expect("right leg rotates");
        assert!(
            !left.values[0].abs_diff_eq(right.values[0], 1e-3),
            "both legs starting in the same place is a march, not a walk",
        );
    }

    #[test]
    fn footsteps_fall_inside_the_clip() {
        for time in footstep_times() {
            assert!((0.0..=WALK_DURATION).contains(&time));
        }
    }
}
