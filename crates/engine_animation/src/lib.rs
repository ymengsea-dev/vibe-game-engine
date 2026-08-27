//! # engine_animation
//!
//! Animation: skeletons, blend trees, animation state machines, and animation events.
//!
//! ## Status
//!
//! Milestone 10 in progress: [`sample_pose`] turns an
//! `engine_asset::ImportedAnimation`'s keyframes plus a point in time
//! into a per-joint local-space pose against an
//! `engine_asset::ImportedSkeleton` (falling back to each joint's bind
//! pose for whatever the animation doesn't touch);
//! [`compute_skinning_matrices`] turns that pose into per-joint skinning
//! matrices, composing each joint's global transform through its parent
//! chain and applying its inverse bind matrix. [`blend_poses`] blends
//! two poses by a factor; [`sample_looping`] is [`sample_pose`] with
//! wraparound instead of clamping; [`sample_blend_tree_1d`] combines
//! both into a 1D blend tree (e.g. blending walk/run clips by speed).
//! [`StateMachine`] combines named [`AnimationState`]s (one clip each),
//! condition-gated [`Transition`]s, and time-based crossfading into a
//! complete (if — see its module docs — deliberately scoped-down) runtime
//! animator. Wiring any of this into a GPU skinning shader is future
//! work.

mod blend_tree;
mod sampling;
mod state_machine;

pub use blend_tree::{BlendMotion, blend_poses, sample_blend_tree_1d, sample_looping};
pub use sampling::{compute_skinning_matrices, sample_pose};
pub use state_machine::{
    AnimationState, StateMachine, StateMachineError, Transition, TransitionCondition,
};
