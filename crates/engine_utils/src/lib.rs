//! # engine_utils
//!
//! Shared utilities: file helpers, math helpers, UUID, hashing, and serialization helpers.
//!
//! ## Status
//!
//! Milestone 2 in progress. Implemented so far: [`Transform`], the
//! engine-wide position/rotation/scale representation, and [`AssetId`]/
//! [`AssetHandle`]/[`AssetStore`] — asset identity and a generic,
//! ref-counted store for live asset data, shared by every crate that
//! either imports/registers assets (`engine_asset`, which sits above this
//! one) or holds live, GPU-resident asset data (`engine_renderer`, which
//! sits below it) — see the `asset` module docs for why this crate is
//! where the two meet.
//!
//! [`Rng`] (deterministic SplitMix64, used by particles and procedural
//! scattering) and [`JobSystem`] (a `rayon`-backed parallel task pool —
//! `join` / `scope` / `par_map` / `par_for_each`, `Arc`-backed and
//! `Clone` so subsystems share one pool) also live here, both being
//! primitives every layer above may need.

mod asset;
mod jobs;
mod rng;
mod transform;

pub use asset::{AssetHandle, AssetId, AssetStore};
pub use jobs::{JobError, JobSystem, Scope};
pub use rng::Rng;
pub use transform::Transform;
