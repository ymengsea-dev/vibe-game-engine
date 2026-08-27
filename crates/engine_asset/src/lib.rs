//! # engine_asset
//!
//! Asset pipeline: import, asset database, UUIDs, background loading, and hot reload.
//!
//! ## Status
//!
//! Milestone 5 complete: [`AssetId`] (UUID identity), [`AssetDatabase`]
//! (reference counting, dependency tracking, cycle rejection),
//! [`AssetLoader`] (background loading via tokio — spawn a future, poll a
//! [`LoadHandle`] each frame without blocking), [`import_gltf_slice`]
//! (glTF 2.0 meshes/materials/cameras), [`import_texture_bytes`] (decode
//! plus mip chain generation), [`import_wav_bytes`] (WAV PCM/float
//! decode), and [`AssetWatcher`] (poll-based hot reload — watch a path,
//! drain changed files each frame without blocking, same shape as
//! [`LoadHandle`]).
//!
//! Milestone 10 in progress: [`import_gltf_slice`] also imports skins as
//! [`ImportedSkeleton`] (joint hierarchy, inverse bind matrices) plus
//! per-vertex [`ImportedSkinWeights`], and animations as
//! [`ImportedAnimation`] (per-node [`ImportedKeyframes`], keyed by node
//! index to match [`ImportedJoint::node_index`]) — data only; sampling a
//! pose from these lives in `engine_animation`, not here.

mod audio_import;
mod database;
mod error;
mod gltf_import;
mod hot_reload;
mod id;
mod loader;
mod texture_import;

pub use audio_import::{ImportedAudio, import_wav_bytes};
pub use database::AssetDatabase;
pub use error::AssetError;
pub use gltf_import::{
    ImportedAnimation, ImportedAnimationChannels, ImportedCamera, ImportedGltf, ImportedImage,
    ImportedInterpolation, ImportedJoint, ImportedKeyframes, ImportedMaterial, ImportedMesh,
    ImportedSkeleton, ImportedSkinWeights, import_gltf_slice,
};
pub use hot_reload::AssetWatcher;
pub use id::AssetId;
pub use loader::{AssetLoader, LoadHandle, LoadStatus};
pub use texture_import::{ImportedTexture, generate_mip_chain, import_texture_bytes};
