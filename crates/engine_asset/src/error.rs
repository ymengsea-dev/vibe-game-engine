//! Error types produced by `engine_asset`.

use thiserror::Error;

use engine_utils::AssetId;

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

    /// [`crate::export_gltf`] refused to write a file: geometry that no
    /// importer would accept (empty, a partial triangle, an index past
    /// the vertex list, a material index that does not exist), or a
    /// serialization failure.
    #[error("failed to export glTF: {0}")]
    GltfExport(String),

    /// [`crate::import_texture_bytes`] failed to decode the image (see
    /// [`engine_renderer::decode_rgba8`]).
    #[error("failed to import texture: {0}")]
    TextureImport(String),

    /// [`crate::encode_png_rgba8`] was handed a pixel buffer that does
    /// not match the given dimensions, or PNG encoding failed.
    #[error("failed to export texture: {0}")]
    TextureExport(String),

    /// [`crate::import_wav_bytes`] failed: malformed/truncated WAV, or an
    /// unsupported format tag/bit depth.
    #[error("failed to import audio: {0}")]
    AudioImport(String),

    /// [`crate::encode_wav_mono16`] was handed no samples or a zero
    /// sample rate.
    #[error("failed to export audio: {0}")]
    AudioExport(String),

    /// [`crate::AssetWatcher::watch`] failed to start watching a path
    /// (e.g. the path doesn't exist).
    #[error("failed to start watching for asset changes: {0}")]
    WatchInit(String),

    /// Reading or writing an [`crate::AssetMeta`] sidecar (or the source
    /// file it describes) failed.
    #[error("asset metadata I/O failed for {path}: {message}")]
    MetaIo {
        /// The path the I/O was attempted on.
        path: String,
        /// The underlying OS error message.
        message: String,
    },

    /// An [`crate::AssetMeta`] sidecar file exists but isn't valid RON /
    /// doesn't match the expected shape.
    #[error("asset metadata at {path} is malformed: {reason}")]
    MetaParse {
        /// The `.meta` file path.
        path: String,
        /// What was wrong with it.
        reason: String,
    },

    /// Reading, writing, or parsing an asset [`crate::Bundle`] (`.pak`)
    /// failed — bad magic, a truncated index, an out-of-range entry, or
    /// an I/O error.
    #[error("asset bundle error: {0}")]
    Bundle(String),
}
