//! Error types produced by `engine_scene`.

use thiserror::Error;

/// Errors that can occur while (de)serializing a [`crate::Scene`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SceneError {
    /// Failed to serialize a [`crate::Scene`] to RON text.
    #[error("failed to serialize scene: {0}")]
    Serialize(String),

    /// Failed to parse RON text into a [`crate::Scene`].
    ///
    /// Covers malformed RON syntax/shape only — see
    /// [`SceneError::Validation`] for semantic checks.
    #[error("failed to parse scene: {0}")]
    Deserialize(String),

    /// A parsed [`crate::Scene`] failed semantic validation: syntactically
    /// valid RON that still describes nonsense (NaN/infinite floats, a
    /// non-normalized rotation, a degenerate camera frustum, ...).
    ///
    /// Scene files are untrusted input (hand-edited, from an unknown
    /// source, or corrupted on disk) — this is the check that catches
    /// values that would otherwise silently propagate into NaN vertex
    /// positions or a broken projection matrix.
    #[error("invalid scene data: {0}")]
    Validation(String),

    /// A filesystem operation ([`crate::Scene::save_to_file`],
    /// [`crate::Scene::load_from_file`]) failed.
    #[error("scene file I/O error: {0}")]
    Io(String),

    /// A [`crate::SceneResolver`] could not turn an asset reference into
    /// a live renderable component — an unknown id, a reference pointing
    /// at the wrong kind of asset, or a failed GPU upload.
    ///
    /// Never fatal to a scene load: [`crate::Scene::instantiate_with_resolver`]
    /// logs one of these, counts it in
    /// [`crate::InstantiateReport::unresolved`], and spawns the entity
    /// without its geometry.
    #[error("failed to resolve asset reference: {0}")]
    Resolve(String),

    /// A parsed scene's format `version` is newer than this engine
    /// understands. Upgrading the engine, not the scene, is what this
    /// needs — see [`crate::CURRENT_SCENE_VERSION`].
    #[error("scene format version {found} is newer than this engine supports (max {max})")]
    UnsupportedVersion {
        /// The version found in the parsed scene.
        found: u32,
        /// The newest version this engine understands.
        max: u32,
    },
}
