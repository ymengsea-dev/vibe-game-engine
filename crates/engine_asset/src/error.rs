//! Error types produced by `engine_asset`.

use thiserror::Error;

use crate::id::AssetId;

/// Errors that can occur while working with the [`crate::AssetDatabase`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetError {
    /// An operation referenced an [`AssetId`] the database has never seen
    /// (never registered, or already fully released).
    #[error("unknown asset id: {0}")]
    UnknownAsset(AssetId),

    /// [`crate::AssetDatabase::release`] was called on an asset with a
    /// reference count already at zero.
    #[error("asset {0} has no remaining references to release")]
    NotReferenced(AssetId),

    /// [`crate::AssetDatabase::add_dependency`] would create a cycle in
    /// the dependency graph (`dependency` already transitively depends on
    /// `dependent`).
    #[error("adding dependency {dependency} to {dependent} would create a cycle")]
    CyclicDependency {
        /// The asset that would gain the new dependency.
        dependent: AssetId,
        /// The asset it would depend on.
        dependency: AssetId,
    },

    /// [`crate::AssetLoader::new`] failed to start its background tokio
    /// runtime.
    #[error("failed to start asset loading runtime: {0}")]
    RuntimeInit(String),

    /// A background load's task was dropped (panicked, or was cancelled)
    /// before it sent a result back through its [`crate::LoadHandle`].
    #[error("background load task was dropped before completing")]
    LoadTaskDropped,

    /// [`crate::import_gltf_slice`] failed: malformed glTF/GLB, or a
    /// feature of the file this importer doesn't support yet (e.g. an
    /// embedded image format other than 8-bit RGB/RGBA).
    #[error("failed to import glTF: {0}")]
    GltfImport(String),

    /// [`crate::import_texture_bytes`] failed to decode the image (see
    /// [`engine_renderer::decode_rgba8`]).
    #[error("failed to import texture: {0}")]
    TextureImport(String),

    /// [`crate::import_wav_bytes`] failed: malformed/truncated WAV, or an
    /// unsupported format tag/bit depth.
    #[error("failed to import audio: {0}")]
    AudioImport(String),

    /// [`crate::AssetWatcher::watch`] failed to start watching a path
    /// (e.g. the path doesn't exist).
    #[error("failed to start watching for asset changes: {0}")]
    WatchInit(String),
}
