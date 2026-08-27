//! Blending poses together: [`blend_poses`] (the primitive — two poses
//! and a factor), [`sample_looping`] ([`sample_pose`] plus wraparound,
//! for a clip meant to repeat), and [`sample_blend_tree_1d`] (a 1D blend
//! tree — e.g. blending "walk"/"run" clips by a speed parameter — built
//! from both).

use engine_asset::{ImportedAnimation, ImportedSkeleton};
use engine_utils::Transform;

use crate::sampling::sample_pose;

/// One motion in a 1D blend tree: an animation clip and the parameter
/// value (e.g. a speed) it's authored for.
///
/// A [`sample_blend_tree_1d`] call's `motions` slice should be sorted
/// ascending by `parameter` — see that function's docs for what happens
/// if it isn't.
#[derive(Debug, Clone, Copy)]
pub struct BlendMotion<'a> {
    /// Where this motion sits along the tree's one axis.
    pub parameter: f32,
    /// The clip to sample when at (or near) this motion's `parameter`.
    pub animation: &'a ImportedAnimation,
}

/// [`sample_pose`], but wrapping `time_seconds` into `animation`'s own
/// `[0, duration)` range instead of clamping — for a clip meant to
/// repeat (a walk cycle, an idle loop, ...) rather than hold its last
/// frame. A zero (or negative — never produced by
/// `engine_asset::import_gltf_slice`, but not assumed here) duration
/// always samples at `0.0`, since there's no span to wrap within.
pub fn sample_looping(
    skeleton: &ImportedSkeleton,
    animation: &ImportedAnimation,
    time_seconds: f32,
) -> Vec<Transform> {
    let duration = animation.duration;
    let time = if duration > f32::EPSILON {
        time_seconds.rem_euclid(duration)
    } else {
        0.0
    };
    sample_pose(skeleton, animation, time)
}

/// Blends `pose_a` and `pose_b` (e.g. two [`sample_looping`] results
/// against the same skeleton) by `t`, clamped to `[0, 1]`: `0.0` is
/// exactly `pose_a`, `1.0` is exactly `pose_b`, values between
/// interpolate each joint — linearly for translation/scale, spherically
/// for rotation (the same convention
/// `engine_asset::ImportedInterpolation::Linear` uses for TRS channels).
///
/// The two poses may have different lengths (e.g. sampled against
/// different skeletons by mistake) — a joint present in only one of them
/// passes through unblended rather than this function panicking.
pub fn blend_poses(pose_a: &[Transform], pose_b: &[Transform], t: f32) -> Vec<Transform> {
    let t = t.clamp(0.0, 1.0);
    let len = pose_a.len().max(pose_b.len());

    (0..len)
        .map(|i| match (pose_a.get(i), pose_b.get(i)) {
            (Some(a), Some(b)) => Transform {
                translation: a.translation.lerp(b.translation, t),
                rotation: a.rotation.slerp(b.rotation, t),
                scale: a.scale.lerp(b.scale, t),
            },
            (Some(a), None) => *a,
            (None, Some(b)) => *b,
            // Unreachable given `len` is derived from the two slices
            // above, but keeps this total rather than indexing unsafely.
            (None, None) => Transform::IDENTITY,
        })
        .collect()
}

/// Samples a 1D blend tree: `motions`, sorted ascending by `parameter`,
/// blended by where `parameter` falls between them, each sampled (via
/// [`sample_looping`]) at `time_seconds` — every motion shares the same
/// playhead, rather than each keeping its own; a common simplification
/// for a locomotion blend tree, where the clips are authored to be
/// roughly in phase with each other (e.g. walk/run cycles that both
/// start on a left-foot strike).
///
/// - Zero motions: `skeleton`'s bind pose.
/// - One motion, or `parameter` at or outside the tree's range: that
///   single nearest motion alone, unblended.
/// - Between two motions: linearly blended by how far `parameter` sits
///   between them.
///
/// `motions` not actually sorted ascending is a violated precondition,
/// not a checked error — the result degrades to "closest single motion"
/// rather than panicking, but isn't a meaningful blend.
pub fn sample_blend_tree_1d(
    skeleton: &ImportedSkeleton,
    motions: &[BlendMotion<'_>],
    parameter: f32,
    time_seconds: f32,
) -> Vec<Transform> {
    match motions {
        [] => skeleton
            .joints
            .iter()
            .map(|joint| joint.local_bind_transform)
            .collect(),
        [only] => sample_looping(skeleton, only.animation, time_seconds),
        _ => {
            let first = &motions[0];
            let last = &motions[motions.len() - 1];

            if parameter <= first.parameter {
                return sample_looping(skeleton, first.animation, time_seconds);
            }
            if parameter >= last.parameter {
                return sample_looping(skeleton, last.animation, time_seconds);
            }

            for i in 0..motions.len() - 1 {
                let a = &motions[i];
                let b = &motions[i + 1];
                if parameter >= a.parameter && parameter <= b.parameter {
                    let span = b.parameter - a.parameter;
                    let t = if span > f32::EPSILON {
                        (parameter - a.parameter) / span
                    } else {
                        0.0
                    };
                    let pose_a = sample_looping(skeleton, a.animation, time_seconds);
                    let pose_b = sample_looping(skeleton, b.animation, time_seconds);
                    return blend_poses(&pose_a, &pose_b, t);
                }
            }

            // `motions` wasn't actually sorted ascending — no bracketing
            // pair was found even though `parameter` is within the
            // tree's overall (first, last) range. Fall back to the
            // single closest motion by parameter distance.
            let closest = motions
                .iter()
                .min_by(|m1, m2| {
                    (m1.parameter - parameter)
                        .abs()
                        .total_cmp(&(m2.parameter - parameter).abs())
                })
                .unwrap_or(first);
            sample_looping(skeleton, closest.animation, time_seconds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_asset::{
        ImportedAnimationChannels, ImportedInterpolation, ImportedJoint, ImportedKeyframes,
    };
    use glam::{Quat, Vec3};
    use std::collections::HashMap;

    fn single_joint_skeleton() -> ImportedSkeleton {
        ImportedSkeleton {
            name: None,
            joints: vec![ImportedJoint {
                name: None,
                node_index: 0,
                parent: None,
                local_bind_transform: Transform::IDENTITY,
                inverse_bind_matrix: glam::Mat4::IDENTITY,
            }],
        }
    }

    /// A 1-second, looping-friendly animation holding node 0's
    /// translation at a constant `x`.
    fn constant_translation_clip(x: f32) -> ImportedAnimation {
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: Some(ImportedKeyframes {
                    interpolation: ImportedInterpolation::Step,
                    times: vec![0.0],
                    values: vec![Vec3::new(x, 0.0, 0.0)],
                }),
                rotation: None,
                scale: None,
            },
        );
        ImportedAnimation {
            name: None,
            duration: 1.0,
            channels,
        }
    }

    // --- `sample_looping` ---

    #[test]
    fn sample_looping_wraps_time_past_the_duration() {
        let skeleton = single_joint_skeleton();
        let animation = constant_translation_clip(1.0);
        // 2.5s into a 1s clip should behave exactly like 0.5s.
        let wrapped = sample_looping(&skeleton, &animation, 2.5);
        let direct = sample_pose(&skeleton, &animation, 0.5);
        assert_eq!(wrapped, direct);
    }

    #[test]
    fn sample_looping_wraps_negative_time_forward() {
        let skeleton = single_joint_skeleton();
        let animation = constant_translation_clip(1.0);
        let wrapped = sample_looping(&skeleton, &animation, -0.25);
        let direct = sample_pose(&skeleton, &animation, 0.75);
        assert_eq!(wrapped, direct);
    }

    #[test]
    fn sample_looping_zero_duration_always_samples_at_zero() {
        let skeleton = single_joint_skeleton();
        let mut animation = constant_translation_clip(1.0);
        animation.duration = 0.0;
        // Must not divide by zero / panic.
        let pose = sample_looping(&skeleton, &animation, 42.0);
        assert_eq!(pose, sample_pose(&skeleton, &animation, 0.0));
    }

    // --- `blend_poses` ---

    #[test]
    fn blend_poses_at_zero_is_exactly_pose_a() {
        let a = vec![Transform::from_translation(Vec3::new(1.0, 0.0, 0.0))];
        let b = vec![Transform::from_translation(Vec3::new(5.0, 0.0, 0.0))];
        assert_eq!(blend_poses(&a, &b, 0.0), a);
    }

    #[test]
    fn blend_poses_at_one_is_exactly_pose_b() {
        let a = vec![Transform::from_translation(Vec3::new(1.0, 0.0, 0.0))];
        let b = vec![Transform::from_translation(Vec3::new(5.0, 0.0, 0.0))];
        assert_eq!(blend_poses(&a, &b, 1.0), b);
    }

    #[test]
    fn blend_poses_at_half_averages_translation() {
        let a = vec![Transform::from_translation(Vec3::new(0.0, 0.0, 0.0))];
        let b = vec![Transform::from_translation(Vec3::new(10.0, 0.0, 0.0))];
        let blended = blend_poses(&a, &b, 0.5);
        assert_eq!(blended[0].translation, Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn blend_poses_clamps_t_outside_zero_one() {
        let a = vec![Transform::from_translation(Vec3::new(0.0, 0.0, 0.0))];
        let b = vec![Transform::from_translation(Vec3::new(10.0, 0.0, 0.0))];
        assert_eq!(blend_poses(&a, &b, -5.0), a);
        assert_eq!(blend_poses(&a, &b, 5.0), b);
    }

    #[test]
    fn blend_poses_passes_through_a_joint_only_one_side_has() {
        let a = vec![
            Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),
            Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)),
        ];
        let b = vec![Transform::from_translation(Vec3::new(9.0, 0.0, 0.0))];
        let blended = blend_poses(&a, &b, 0.5);
        assert_eq!(blended.len(), 2);
        assert_eq!(blended[1], a[1]);
    }

    #[test]
    fn blend_poses_slerps_rotation() {
        let start = Quat::IDENTITY;
        let end = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let a = vec![Transform::from_rotation(start)];
        let b = vec![Transform::from_rotation(end)];
        let blended = blend_poses(&a, &b, 0.5);
        let expected = start.slerp(end, 0.5);
        assert!(blended[0].rotation.dot(expected).abs() > 0.9999);
    }

    // --- `sample_blend_tree_1d` ---

    #[test]
    fn blend_tree_with_no_motions_is_the_bind_pose() {
        let skeleton = single_joint_skeleton();
        let pose = sample_blend_tree_1d(&skeleton, &[], 0.0, 0.0);
        assert_eq!(pose, vec![skeleton.joints[0].local_bind_transform]);
    }

    #[test]
    fn blend_tree_with_one_motion_is_just_that_motion() {
        let skeleton = single_joint_skeleton();
        let animation = constant_translation_clip(3.0);
        let motions = [BlendMotion {
            parameter: 0.0,
            animation: &animation,
        }];

        let pose = sample_blend_tree_1d(&skeleton, &motions, 999.0, 0.0);
        assert_eq!(pose[0].translation, Vec3::new(3.0, 0.0, 0.0));
    }

    #[test]
    fn blend_tree_clamps_below_the_lowest_motion() {
        let skeleton = single_joint_skeleton();
        let walk = constant_translation_clip(1.0);
        let run = constant_translation_clip(5.0);
        let motions = [
            BlendMotion {
                parameter: 2.0,
                animation: &walk,
            },
            BlendMotion {
                parameter: 6.0,
                animation: &run,
            },
        ];

        let pose = sample_blend_tree_1d(&skeleton, &motions, 0.0, 0.0);
        assert_eq!(pose[0].translation, Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn blend_tree_clamps_above_the_highest_motion() {
        let skeleton = single_joint_skeleton();
        let walk = constant_translation_clip(1.0);
        let run = constant_translation_clip(5.0);
        let motions = [
            BlendMotion {
                parameter: 2.0,
                animation: &walk,
            },
            BlendMotion {
                parameter: 6.0,
                animation: &run,
            },
        ];

        let pose = sample_blend_tree_1d(&skeleton, &motions, 100.0, 0.0);
        assert_eq!(pose[0].translation, Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn blend_tree_blends_between_two_bracketing_motions() {
        let skeleton = single_joint_skeleton();
        let walk = constant_translation_clip(0.0);
        let run = constant_translation_clip(10.0);
        let motions = [
            BlendMotion {
                parameter: 2.0,
                animation: &walk,
            },
            BlendMotion {
                parameter: 6.0,
                animation: &run,
            },
        ];

        // Halfway between 2.0 and 6.0 is 4.0 — halfway between the two
        // clips' translations too.
        let pose = sample_blend_tree_1d(&skeleton, &motions, 4.0, 0.0);
        assert_eq!(pose[0].translation, Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn blend_tree_picks_the_right_pair_among_three_motions() {
        let skeleton = single_joint_skeleton();
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(10.0);
        let run = constant_translation_clip(20.0);
        let motions = [
            BlendMotion {
                parameter: 0.0,
                animation: &idle,
            },
            BlendMotion {
                parameter: 2.0,
                animation: &walk,
            },
            BlendMotion {
                parameter: 6.0,
                animation: &run,
            },
        ];

        // Between idle and walk, not walk and run.
        let pose = sample_blend_tree_1d(&skeleton, &motions, 1.0, 0.0);
        assert_eq!(pose[0].translation, Vec3::new(5.0, 0.0, 0.0));
    }
}
