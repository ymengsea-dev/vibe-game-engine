//! Bringing files from the user's machine into a project.
//!
//! The import *pipeline* — scanning, decoding, `.meta` ids, previews, the
//! change watcher — has worked for a while. What was missing was the
//! front door: `Assets ▸ Import…` was a disabled button, nothing handled
//! a file dragged onto the window, and the only way to add an asset was
//! to copy it into `<project>/assets/` in Finder. This module is that
//! door.
//!
//! ## Copy, don't reference
//!
//! Imported files are **copied** into the project rather than referenced
//! where they sit. A project has to stay self-contained and exportable; a
//! reference into `~/Downloads` breaks the moment someone tidies up.
//!
//! ## Never overwrite
//!
//! Importing a name that already exists picks a free one
//! (`barrel.gltf` → `barrel_1.gltf`) rather than replacing the existing
//! asset. Silently destroying someone's work to save them a rename is not
//! a trade worth making.

use std::path::{Path, PathBuf};

use crate::assets::AssetKind;

/// What happened to one file an import was asked to bring in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Copied in, under this project-relative name. The name differs from
    /// the source when one was already taken.
    Copied {
        /// Where it landed, relative to the assets directory.
        destination: PathBuf,
        /// Whether it had to be renamed to avoid clobbering.
        renamed: bool,
    },
    /// The engine has no importer for this file type.
    Unsupported {
        /// The rejected file.
        source: PathBuf,
    },
    /// The copy failed.
    Failed {
        /// The file that could not be copied.
        source: PathBuf,
        /// Why.
        reason: String,
    },
}

impl ImportOutcome {
    /// A one-line description, for the Console.
    pub fn summary(&self) -> String {
        match self {
            ImportOutcome::Copied {
                destination,
                renamed: false,
            } => format!("imported {}", destination.display()),
            ImportOutcome::Copied {
                destination,
                renamed: true,
            } => format!(
                "imported as {} (that name was taken)",
                destination.display()
            ),
            ImportOutcome::Unsupported { source } => format!(
                "skipped {}: no importer for this file type",
                source.display()
            ),
            ImportOutcome::Failed { source, reason } => {
                format!("could not import {}: {reason}", source.display())
            }
        }
    }

    /// Whether this outcome added a file to the project.
    pub fn succeeded(&self) -> bool {
        matches!(self, ImportOutcome::Copied { .. })
    }
}

/// The file extensions [`AssetKind`] recognises, for the dialog's filter.
pub const IMPORTABLE_EXTENSIONS: [&str; 6] = ["gltf", "glb", "png", "jpg", "jpeg", "wav"];

/// Whether `path` names a file kind the engine can import.
///
/// Scenes and prefabs are excluded deliberately: they are *produced* by
/// the editor, and importing one from elsewhere would carry entity
/// references that mean nothing in this project.
pub fn is_importable(path: &Path) -> bool {
    matches!(
        AssetKind::from_path(path),
        AssetKind::Mesh | AssetKind::Texture | AssetKind::Audio
    )
}

/// Picks a free path in `directory` for a file named `file_name`.
///
/// Returns the name unchanged when nothing occupies it, otherwise appends
/// `_1`, `_2`, … before the extension until one is free.
///
/// Bounded: after a large number of attempts it gives up and returns the
/// last candidate rather than looping forever on a pathological
/// directory.
pub fn unique_destination(directory: &Path, file_name: &Path) -> Option<PathBuf> {
    unique_destination_with_limit(directory, file_name, 9_999)
}

fn unique_destination_with_limit(
    directory: &Path,
    file_name: &Path,
    limit: u32,
) -> Option<PathBuf> {
    let candidate = directory.join(file_name);
    if !candidate.exists() {
        return Some(candidate);
    }

    let stem = file_name
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "asset".to_string());
    let extension = file_name
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());

    for attempt in 1..=limit {
        let name = match &extension {
            Some(extension) => format!("{stem}_{attempt}.{extension}"),
            None => format!("{stem}_{attempt}"),
        };
        let candidate = directory.join(&name);
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Copies `sources` into `assets_dir`, creating it if absent.
///
/// One [`ImportOutcome`] per input, in the same order. Never returns an
/// error: a file that cannot be copied is reported and the rest still
/// import, because failing an eight-file drop over one bad file would be
/// worse than importing seven.
///
/// Imported assets receive a new local `.meta` sidecar during the next
/// import pass. Carrying a source project's sidecar could duplicate an
/// id that already exists here and make references resolve unpredictably.
pub fn import_files(assets_dir: &Path, sources: &[PathBuf]) -> Vec<ImportOutcome> {
    let mut outcomes = Vec::with_capacity(sources.len());

    for source in sources {
        if !is_importable(source) {
            outcomes.push(ImportOutcome::Unsupported {
                source: source.clone(),
            });
            continue;
        }

        if let Err(err) = std::fs::create_dir_all(assets_dir) {
            outcomes.push(ImportOutcome::Failed {
                source: source.clone(),
                reason: format!("could not create the assets directory: {err}"),
            });
            continue;
        }

        let Some(file_name) = source.file_name().map(Path::new) else {
            outcomes.push(ImportOutcome::Failed {
                source: source.clone(),
                reason: "path has no file name".to_string(),
            });
            continue;
        };

        let Some(destination) = unique_destination(assets_dir, file_name) else {
            outcomes.push(ImportOutcome::Failed {
                source: source.clone(),
                reason: "could not find a free asset name".to_string(),
            });
            continue;
        };
        let renamed = destination.file_name() != source.file_name();

        match std::fs::copy(source, &destination) {
            Ok(_) => {
                let relative = destination
                    .strip_prefix(assets_dir)
                    .unwrap_or(&destination)
                    .to_path_buf();
                outcomes.push(ImportOutcome::Copied {
                    destination: relative,
                    renamed,
                });
            }
            Err(err) => outcomes.push(ImportOutcome::Failed {
                source: source.clone(),
                reason: err.to_string(),
            }),
        }
    }

    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-asset-import-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn recognises_importable_kinds() {
        assert!(is_importable(Path::new("model.gltf")));
        assert!(is_importable(Path::new("model.GLB")));
        assert!(is_importable(Path::new("albedo.png")));
        assert!(is_importable(Path::new("step.wav")));
    }

    #[test]
    fn rejects_kinds_the_editor_produces_or_cannot_read() {
        // Scenes and prefabs are outputs, not inputs.
        assert!(!is_importable(Path::new("level.ron")));
        assert!(!is_importable(Path::new("barrel.prefab")));
        assert!(!is_importable(Path::new("notes.txt")));
        assert!(!is_importable(Path::new("noextension")));
    }

    #[test]
    fn unique_destination_keeps_a_free_name() {
        let dir = temp_dir("free");
        let chosen = unique_destination(&dir, Path::new("barrel.gltf"));
        assert_eq!(chosen, Some(dir.join("barrel.gltf")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_destination_avoids_clobbering() {
        let dir = temp_dir("taken");
        write(&dir.join("barrel.gltf"), "existing");

        let chosen = unique_destination(&dir, Path::new("barrel.gltf"));
        assert_eq!(chosen, Some(dir.join("barrel_1.gltf")));

        write(&dir.join("barrel_1.gltf"), "also existing");
        assert_eq!(
            unique_destination(&dir, Path::new("barrel.gltf")),
            Some(dir.join("barrel_2.gltf")),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_destination_reports_an_exhausted_name_space() {
        let dir = temp_dir("exhausted");
        write(&dir.join("barrel.gltf"), "original");
        write(&dir.join("barrel_1.gltf"), "original");

        assert!(unique_destination_with_limit(&dir, Path::new("barrel.gltf"), 1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn importing_never_overwrites_an_existing_asset() {
        let dir = temp_dir("nooverwrite");
        let assets = dir.join("assets");
        write(&assets.join("barrel.gltf"), "ORIGINAL");

        let source = dir.join("barrel.gltf");
        write(&source, "INCOMING");

        let outcomes = import_files(&assets, &[source]);
        assert!(outcomes[0].succeeded());
        assert!(
            matches!(&outcomes[0], ImportOutcome::Copied { renamed: true, .. }),
            "a name clash must rename, not replace",
        );
        assert_eq!(
            std::fs::read_to_string(assets.join("barrel.gltf")).expect("read"),
            "ORIGINAL",
            "the existing asset must be untouched",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_creates_the_assets_dir() {
        let dir = temp_dir("createdir");
        let assets = dir.join("assets");
        assert!(!assets.exists());

        let source = dir.join("thing.png");
        write(&source, "data");

        let outcomes = import_files(&assets, &[source]);
        assert!(outcomes[0].succeeded(), "{:?}", outcomes[0]);
        assert!(assets.join("thing.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_extension_is_reported_not_ignored() {
        let dir = temp_dir("unsupported");
        let source = dir.join("scene.blend");
        write(&source, "data");

        let outcomes = import_files(&dir.join("assets"), &[source]);
        assert!(matches!(outcomes[0], ImportOutcome::Unsupported { .. }));
        assert!(outcomes[0].summary().contains("no importer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_source_fails_without_stopping_the_others() {
        let dir = temp_dir("partial");
        let assets = dir.join("assets");
        let good = dir.join("good.png");
        write(&good, "data");
        let missing = dir.join("gone.png");

        let outcomes = import_files(&assets, &[missing, good]);
        assert!(matches!(outcomes[0], ImportOutcome::Failed { .. }));
        assert!(
            outcomes[1].succeeded(),
            "one bad file must not lose the rest of the batch",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_meta_sidecar_is_not_imported() {
        let dir = temp_dir("sidecar");
        let assets = dir.join("assets");
        let source = dir.join("barrel.gltf");
        write(&source, "model");
        write(
            &dir.join("barrel.gltf.meta"),
            "(id: \"b8d5d33b-086e-4494-b2bd-0cb556828369\", source_hash: 1,)",
        );

        import_files(&assets, &[source]);

        assert!(
            !assets.join("barrel.gltf.meta").exists(),
            "the project importer must mint a local id instead"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_asset_without_a_sidecar_imports_fine() {
        let dir = temp_dir("nosidecar");
        let assets = dir.join("assets");
        let source = dir.join("plain.wav");
        write(&source, "audio");

        let outcomes = import_files(&assets, &[source]);
        assert!(outcomes[0].succeeded());
        assert!(!assets.join("plain.wav.meta").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outcomes_are_returned_in_input_order() {
        let dir = temp_dir("order");
        let assets = dir.join("assets");
        let a = dir.join("a.png");
        let b = dir.join("b.txt");
        write(&a, "1");
        write(&b, "2");

        let outcomes = import_files(&assets, &[a, b]);
        assert!(outcomes[0].succeeded());
        assert!(matches!(outcomes[1], ImportOutcome::Unsupported { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
