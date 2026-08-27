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
}
