//! # engine_asset
//!
//! Asset pipeline: import, UUIDs, metadata, bundles, and hot reload.
//!
//! ## Status
//!
//! The pipeline provides [`AssetId`] UUID identity, [`AssetMeta`] sidecars,
//! import caching, glTF/texture/WAV import, asset bundles, and polling hot
//! reload. Reference counting lives with concrete asset stores; loading is
//! currently synchronous at its editor call sites.
//!
//! Milestone 10 in progress: [`import_gltf_slice`] also imports skins as
//! [`ImportedSkeleton`] (joint hierarchy, inverse bind matrices) plus
//! per-vertex [`ImportedSkinWeights`], and animations as
//! [`ImportedAnimation`] (per-node [`ImportedKeyframes`], keyed by node
//! index to match [`ImportedJoint::node_index`]) — data only; sampling a
//! pose from these lives in `engine_animation`, not here.

mod audio_import;
mod bundle;
mod error;
mod gltf_export;
mod gltf_import;
mod hot_reload;
mod meta;
mod texture_import;

pub use audio_import::{ImportedAudio, import_wav_bytes};
pub use bundle::{BUNDLE_MAGIC, Bundle, BundleStats, pack_dir, pack_dir_with_metadata};
/// Re-exported from `engine_utils`, where it lives alongside
/// [`engine_utils::AssetHandle`]/[`engine_utils::AssetStore`] so that
/// crate (which sits below both this one and `engine_renderer`) can be the
/// one place asset identity is defined, without either of those two
/// crates depending on each other. `engine_asset::AssetId` stays a valid
/// path so existing callers are unaffected.
pub use engine_utils::AssetId;
pub use error::AssetError;
pub use gltf_export::{
    ExportAlphaMode, ExportAnimation, ExportAnimationEvent, ExportChannel, ExportChannelValues,
    ExportJoint, ExportMaterial, ExportMesh, ExportSkin, ExportSkinnedMesh, ExportTexture,
    GltfScene, encode_png_rgba8, encode_wav_mono16, export_gltf, export_gltf_scene,
};
pub use gltf_import::{
    ImportedAnimation, ImportedAnimationChannels, ImportedAnimationEvent, ImportedCamera,
    ImportedGltf, ImportedImage, ImportedInterpolation, ImportedJoint, ImportedKeyframes,
    ImportedMaterial, ImportedMesh, ImportedSkeleton, ImportedSkinWeights, import_gltf_file,
    import_gltf_slice, import_gltf_slice_with,
};
pub use hot_reload::AssetWatcher;
pub use meta::{AssetMeta, AssetMetadataStore, ImportCache, hash_bytes};
pub use texture_import::{ImportedTexture, generate_mip_chain, import_texture_bytes};
