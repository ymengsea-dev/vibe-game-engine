//! Turns an [`ImportedAnimation`]'s keyframes plus a point in time into a
//! pose ([`sample_pose`]), then a pose plus an [`ImportedSkeleton`] into
//! per-joint skinning matrices ([`compute_skinning_matrices`]) — the two
//! steps a renderer needs, in order, to actually deform a skinned mesh.
//! Wiring the result into a GPU skinning shader is future work; this
//! crate only computes the matrices.

use engine_asset::{ImportedAnimation, ImportedInterpolation, ImportedKeyframes, ImportedSkeleton};
use engine_utils::Transform;
use glam::{Mat4, Quat, Vec3};

/// Samples `animation` at `time_seconds` against `skeleton`, returning
/// one local-space [`Transform`] per joint, in the same order as
/// `skeleton.joints`.
///
/// `time_seconds` is clamped to `[0, animation.duration]` — this
/// function doesn't loop or extrapolate; a caller wanting a looping clip
/// should pass `time_seconds % animation.duration` (guarding
/// `duration > 0.0`) itself.
///
/// A joint untouched by `animation` (no channel keyed by its
/// [`ImportedJoint::node_index`](engine_asset::ImportedJoint::node_index),
/// or one missing a specific TRS channel) keeps that joint's bind-pose
/// value for the missing part.
pub fn sample_pose(
    skeleton: &ImportedSkeleton,
    animation: &ImportedAnimation,
    time_seconds: f32,
) -> Vec<Transform> {
    let time = time_seconds.clamp(0.0, animation.duration.max(0.0));

    skeleton
        .joints
        .iter()
        .map(|joint| {
            let mut transform = joint.local_bind_transform;
            let Some(channels) = animation.channels.get(&joint.node_index) else {
                return transform;
            };
            if let Some(keyframes) = &channels.translation {
                transform.translation = sample_vec3(keyframes, time);
            }
            if let Some(keyframes) = &channels.rotation {
                transform.rotation = sample_quat(keyframes, time);
            }
            if let Some(keyframes) = &channels.scale {
                transform.scale = sample_vec3(keyframes, time);
            }
            transform
        })
        .collect()
}

/// Computes each joint's skinning matrix: its pose-composed global
/// transform (parent's global transform, recursively, times its own
/// local one — the same composition `engine_ecs::propagate_transforms`
/// uses for scene entities) times its inverse bind matrix.
///
/// `local_poses` should be `skeleton.joints.len()` long, in the same
/// order (e.g. from [`sample_pose`] against the same `skeleton`) — a
/// shorter slice just falls back to each uncovered joint's bind pose,
/// rather than panicking.
///
/// Never panics, even against a malformed `skeleton` with a parent
/// cycle (only possible from untrusted/hand-edited glTF input, since
/// nothing in [`engine_asset::import_gltf_slice`]'s own construction can
/// produce one) — a joint whose parent chain cycles back to itself is
/// treated as having no parent instead of recursing forever.
pub fn compute_skinning_matrices(
    skeleton: &ImportedSkeleton,
    local_poses: &[Transform],
) -> Vec<Mat4> {
    let joint_count = skeleton.joints.len();
    let mut global: Vec<Option<Transform>> = vec![None; joint_count];
    let mut visiting = vec![false; joint_count];
    let mut matrices = Vec::with_capacity(joint_count);

    for index in 0..joint_count {
        let global_transform =
            resolve_global(skeleton, local_poses, index, &mut global, &mut visiting);
        matrices.push(global_transform.to_matrix() * skeleton.joints[index].inverse_bind_matrix);
    }

    matrices
}

fn resolve_global(
    skeleton: &ImportedSkeleton,
    local_poses: &[Transform],
    index: usize,
    global: &mut [Option<Transform>],
    visiting: &mut [bool],
) -> Transform {
    if let Some(transform) = global[index] {
        return transform;
    }

    let local = local_poses
        .get(index)
        .copied()
        .unwrap_or(skeleton.joints[index].local_bind_transform);

    visiting[index] = true;
    let parent = skeleton.joints[index].parent;
    let transform = match parent {
        // `!visiting[parent_index]` is the cycle guard: if the parent is
        // already being resolved further up this same call stack,
        // following it would recurse forever, so it's treated as "no
        // parent" instead.
        Some(parent_index) if parent_index < skeleton.joints.len() && !visiting[parent_index] => {
            resolve_global(skeleton, local_poses, parent_index, global, visiting)
                .mul_transform(&local)
        }
        _ => local,
    };
    visiting[index] = false;

    global[index] = Some(transform);
    transform
}

/// Returns `(index_a, index_b, t)`: the two keyframe indices `time`
/// falls between (equal, with `t = 0.0`, if `time` is at or before the
/// first keyframe or at or after the last) and the `[0, 1]`
/// interpolation factor between them.
///
/// `times` must be non-empty (both [`sample_vec3`]/[`sample_quat`]'s
/// only callers pass an [`ImportedKeyframes`]' `times`, which
/// `import_gltf_slice` never produces empty).
fn segment(times: &[f32], time: f32) -> (usize, usize, f32) {
    if times.len() <= 1 || time <= times[0] {
        return (0, 0, 0.0);
    }
    let last = times.len() - 1;
    if time >= times[last] {
        return (last, last, 0.0);
    }

    // `times` is strictly ascending (glTF's own requirement on sampler
    // input accessors), so this is a valid binary search.
    let b = times.partition_point(|&t| t <= time).max(1).min(last);
    let a = b - 1;
    let span = times[b] - times[a];
    let t = if span > f32::EPSILON {
        (time - times[a]) / span
    } else {
        0.0
    };
    (a, b, t)
}

fn sample_vec3(keyframes: &ImportedKeyframes<Vec3>, time: f32) -> Vec3 {
    let (a, b, t) = segment(&keyframes.times, time);
    match keyframes.interpolation {
        ImportedInterpolation::Step => keyframes.values[a],
        ImportedInterpolation::Linear => keyframes.values[a].lerp(keyframes.values[b], t),
    }
}

fn sample_quat(keyframes: &ImportedKeyframes<Quat>, time: f32) -> Quat {
    let (a, b, t) = segment(&keyframes.times, time);
    match keyframes.interpolation {
        ImportedInterpolation::Step => keyframes.values[a],
        // Rotations interpolate spherically, not linearly — glTF's own
        // requirement (see `ImportedInterpolation::Linear`'s docs).
        ImportedInterpolation::Linear => keyframes.values[a].slerp(keyframes.values[b], t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_asset::{ImportedAnimationChannels, ImportedJoint};
    use std::collections::HashMap;

    fn keyframes_vec3(
        interpolation: ImportedInterpolation,
        times: &[f32],
        values: &[Vec3],
    ) -> ImportedKeyframes<Vec3> {
        ImportedKeyframes {
            interpolation,
            times: times.to_vec(),
            values: values.to_vec(),
        }
    }

    fn keyframes_quat(
        interpolation: ImportedInterpolation,
        times: &[f32],
        values: &[Quat],
    ) -> ImportedKeyframes<Quat> {
        ImportedKeyframes {
            interpolation,
            times: times.to_vec(),
            values: values.to_vec(),
        }
    }

    fn joint(
        name: &str,
        node_index: usize,
        parent: Option<usize>,
        translation: Vec3,
    ) -> ImportedJoint {
        ImportedJoint {
            name: Some(name.to_string()),
            node_index,
            parent,
            local_bind_transform: Transform::from_translation(translation),
            inverse_bind_matrix: Mat4::IDENTITY,
        }
    }

    // --- `segment` ---

    #[test]
    fn segment_clamps_before_the_first_keyframe() {
        assert_eq!(segment(&[1.0, 2.0, 3.0], 0.0), (0, 0, 0.0));
    }

    #[test]
    fn segment_clamps_after_the_last_keyframe() {
        assert_eq!(segment(&[1.0, 2.0, 3.0], 10.0), (2, 2, 0.0));
    }

    #[test]
    fn segment_finds_the_midpoint_between_two_keyframes() {
        assert_eq!(segment(&[0.0, 2.0], 1.0), (0, 1, 0.5));
    }

    #[test]
    fn segment_lands_exactly_on_a_middle_keyframe() {
        // `t == 0.0` is what matters here, not whether `a == b` — either
        // representation selects keyframe 1's value exactly (interpolating
        // it with its *own* neighbor at `t = 0.0` is a no-op).
        let (a, _b, t) = segment(&[0.0, 1.0, 2.0], 1.0);
        assert_eq!(a, 1);
        assert_eq!(t, 0.0);
    }

    #[test]
    fn segment_handles_a_single_keyframe() {
        assert_eq!(segment(&[5.0], 100.0), (0, 0, 0.0));
    }

    // --- `sample_pose` ---

    #[test]
    fn sample_pose_uses_bind_pose_for_an_untouched_joint() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let animation = ImportedAnimation {
            name: None,
            duration: 1.0,
            channels: HashMap::new(),
            events: Vec::new(),
        };

        let pose = sample_pose(&skeleton, &animation, 0.5);
        assert_eq!(pose[0], skeleton.joints[0].local_bind_transform);
    }

    #[test]
    fn sample_pose_linearly_interpolates_translation() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: Some(keyframes_vec3(
                    ImportedInterpolation::Linear,
                    &[0.0, 2.0],
                    &[Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0)],
                )),
                rotation: None,
                scale: None,
            },
        );
        let animation = ImportedAnimation {
            name: None,
            duration: 2.0,
            channels,
            events: Vec::new(),
        };

        let pose = sample_pose(&skeleton, &animation, 1.0);
        assert_eq!(pose[0].translation, Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn sample_pose_holds_step_interpolated_values() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: Some(keyframes_vec3(
                    ImportedInterpolation::Step,
                    &[0.0, 2.0],
                    &[Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0)],
                )),
                rotation: None,
                scale: None,
            },
        );
        let animation = ImportedAnimation {
            name: None,
            duration: 2.0,
            channels,
            events: Vec::new(),
        };

        // Still mid-way between keyframes — STEP holds the first one.
        let pose = sample_pose(&skeleton, &animation, 1.0);
        assert_eq!(pose[0].translation, Vec3::ZERO);
    }

    #[test]
    fn sample_pose_slerps_rotation() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let start = Quat::IDENTITY;
        let end = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: None,
                rotation: Some(keyframes_quat(
                    ImportedInterpolation::Linear,
                    &[0.0, 1.0],
                    &[start, end],
                )),
                scale: None,
            },
        );
        let animation = ImportedAnimation {
            name: None,
            duration: 1.0,
            channels,
            events: Vec::new(),
        };

        let pose = sample_pose(&skeleton, &animation, 0.5);
        let expected = start.slerp(end, 0.5);
        assert!((pose[0].rotation.dot(expected)).abs() > 0.9999);
    }

    #[test]
    fn sample_pose_clamps_time_outside_the_duration() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: Some(keyframes_vec3(
                    ImportedInterpolation::Linear,
                    &[0.0, 1.0],
                    &[Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)],
                )),
                rotation: None,
                scale: None,
            },
        );
        let animation = ImportedAnimation {
            name: None,
            duration: 1.0,
            channels,
            events: Vec::new(),
        };

        let past_the_end = sample_pose(&skeleton, &animation, 100.0);
        assert_eq!(past_the_end[0].translation, Vec3::new(1.0, 0.0, 0.0));

        let before_the_start = sample_pose(&skeleton, &animation, -5.0);
        assert_eq!(before_the_start[0].translation, Vec3::ZERO);
    }

    // --- `compute_skinning_matrices` ---

    #[test]
    fn root_joint_skinning_matrix_is_its_own_pose_when_bind_matches() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint("Root", 0, None, Vec3::ZERO)],
        };
        let poses = vec![Transform::from_translation(Vec3::new(1.0, 0.0, 0.0))];

        let matrices = compute_skinning_matrices(&skeleton, &poses);
        assert_eq!(matrices[0], poses[0].to_matrix());
    }

    #[test]
    fn child_skinning_matrix_composes_through_its_parent() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![
                joint("Root", 0, None, Vec3::ZERO),
                joint("Child", 1, Some(0), Vec3::new(0.0, 1.0, 0.0)),
            ],
        };
        let poses = vec![
            Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
            Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        ];

        let matrices = compute_skinning_matrices(&skeleton, &poses);
        // Child's global position: parent's (5, 0, 0) plus its own
        // local (0, 1, 0) = (5, 1, 0).
        let child_global = matrices[1].transform_point3(Vec3::ZERO);
        assert_eq!(child_global, Vec3::new(5.0, 1.0, 0.0));
    }

    #[test]
    fn inverse_bind_matrix_cancels_out_the_bind_pose() {
        // A joint whose current pose *is* its bind pose should skin
        // back to identity — that's the entire point of an inverse bind
        // matrix.
        let bind_translation = Vec3::new(3.0, 4.0, 5.0);
        let joint = ImportedJoint {
            name: None,
            node_index: 0,
            parent: None,
            local_bind_transform: Transform::from_translation(bind_translation),
            inverse_bind_matrix: Mat4::from_translation(-bind_translation),
        };
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![joint],
        };
        let poses = vec![Transform::from_translation(bind_translation)];

        let matrices = compute_skinning_matrices(&skeleton, &poses);
        assert!(matrices[0].abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }

    #[test]
    fn a_parent_cycle_does_not_recurse_forever() {
        // Malformed on purpose: joint 0's parent is joint 1, and joint
        // 1's parent is joint 0 — impossible from `import_gltf_slice`
        // itself, but this must still terminate rather than blowing the
        // stack against hand-edited/adversarial input.
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![
                joint("A", 0, Some(1), Vec3::ZERO),
                joint("B", 1, Some(0), Vec3::ZERO),
            ],
        };
        let poses = vec![Transform::IDENTITY, Transform::IDENTITY];

        // No panic/hang is the assertion; this is just a sanity check
        // that it still produced two matrices.
        let matrices = compute_skinning_matrices(&skeleton, &poses);
        assert_eq!(matrices.len(), 2);
    }

    #[test]
    fn shorter_pose_slice_falls_back_to_bind_pose() {
        let skeleton = ImportedSkeleton {
            name: None,
            joints: vec![
                joint("Root", 0, None, Vec3::ZERO),
                joint("Child", 1, Some(0), Vec3::new(0.0, 1.0, 0.0)),
            ],
        };

        // Only one pose given for a two-joint skeleton.
        let poses = vec![Transform::IDENTITY];

        let matrices = compute_skinning_matrices(&skeleton, &poses);
        assert_eq!(matrices.len(), 2);
    }
}
