//! Regenerates the island's assets.
//!
//! ```sh
//! cargo run -p island --bin bake            # bake into the crate's own folder
//! cargo run -p island --bin bake -- <dir>   # or somewhere else
//! ```
//!
//! Writes textures, models, audio, a `project.ron` and a scene — see
//! [`island::bake`] for the full list. Existing `.meta` sidecars are
//! kept, so asset ids survive a re-bake and scenes referencing them
//! keep working.
//!
//! Run this after changing anything in `island::world`; the checked-in
//! assets are the *output* of those generators, not a second source of
//! truth.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let _ = engine::core::logging::init_default();

    // Default to the crate's own directory, so a bare `cargo run --bin
    // bake` refreshes this project rather than writing into whatever
    // directory the shell happened to be in.
    let root = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);

    match island::bake::bake(&root) {
        Ok(report) => {
            tracing::info!(
                root = %root.display(),
                textures = report.textures,
                models = report.models,
                audio = report.audio,
                entities = report.scene_entities,
                "baked the island project"
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(error = %err, root = %root.display(), "bake failed");
            ExitCode::FAILURE
        }
    }
}
