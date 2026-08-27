//! # engine_utils
//!
//! Shared utilities: file helpers, math helpers, UUID, hashing, and serialization helpers.
//!
//! ## Status
//!
//! Milestone 2 in progress. Implemented so far: [`Transform`], the
//! engine-wide position/rotation/scale representation.

mod transform;

pub use transform::Transform;
