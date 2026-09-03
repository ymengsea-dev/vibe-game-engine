//! Errors produced by `engine_editor`.

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur while setting up or driving the editor.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EditorError {
    /// [`crate::Viewport::new`] failed to create its render geometry.
    #[error("failed to create viewport geometry: {0}")]
    Renderer(#[from] engine_renderer::RendererError),

    /// [`crate::scan_assets`] failed to read a directory entry.
    #[error("failed to scan assets directory {path}: {source}")]
    AssetScan {
        /// The directory entry that couldn't be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// Writing the editor session file (`.studio/session.ron`) failed.
    #[error("failed to write editor session {path}: {message}")]
    Session {
        /// The session file path.
        path: PathBuf,
        /// What went wrong (serialization or I/O).
        message: String,
    },

    /// Saving or loading a `.prefab` file failed (see
    /// [`crate::prefab::write_prefab`] / [`crate::EditorState::spawn_prefab`]).
    #[error("prefab file error: {0}")]
    Prefab(#[from] engine_scene::SceneError),

    /// A "Make Prefab" request named an entity that had already been
    /// despawned by the time the host handled it.
    #[error("prefab: the selected entity no longer exists")]
    PrefabEntityMissing,
}
