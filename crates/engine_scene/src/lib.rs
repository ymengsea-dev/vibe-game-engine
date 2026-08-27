//! # engine_scene
//!
//! Scene system: loading, saving, prefabs, entity hierarchy, and scene switching.
//!
//! ## Status
//!
//! Stage 2 "Scene format v2" complete: the [`Scene`] RON format
//! ([`TransformData`], [`CameraData`], [`SceneEntity`]) — deliberately a
//! separate on-disk shape from the live ECS component types (see the
//! module-level docs on the `format` module in source for why) — is
//! versioned ([`CURRENT_SCENE_VERSION`], migrated on parse by
//! [`Scene::from_ron_str`]), and [`SceneEntity`] carries a name, a
//! parent-index for hierarchy, and a [`MeshRendererData`] asset reference,
//! on top of the original transform/camera. File I/O
//! ([`Scene::save_to_file`], [`Scene::load_from_file`]) and semantic
//! validation ([`Scene::validate`]) cover all of it, including hierarchy
//! bounds/acyclicity — untrusted scene data can't describe a parent cycle
//! and have it survive validation. Live parent/child *transform*
//! propagation is the `engine_ecs` side (`engine_ecs::propagate_transforms`,
//! `bevy_ecs::hierarchy::ChildOf`); turning a `Scene`'s index-based
//! hierarchy into that live form is Stage 4's "Scene save/load — on the
//! Stage 2 format, from the editor." [`Prefab`] wraps the same
//! [`SceneEntity`] shape as a reusable entity template, instantiable into
//! a live `World` any number of times.

mod error;
mod format;
mod io;
mod migration;
mod prefab;
mod validate;

pub use error::SceneError;
pub use format::{
    AssetRef, CameraData, MeshRendererData, ProjectionData, Scene, SceneEntity, SpriteData,
    TransformData,
};
pub use migration::CURRENT_SCENE_VERSION;
pub use prefab::Prefab;
