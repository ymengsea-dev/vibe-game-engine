//! Asset browser panel: lists files under a project's assets directory.
//!
//! Directory-scanning logic ([`scan`]) is split out from the egui
//! drawing ([`show`]) for the same reason [`crate::hierarchy`]/
//! [`crate::inspector`] split theirs — testable without a live UI.
//!
//! Treats the assets directory as untrusted input (missing, unreadable,
//! or containing symlink cycles) rather than assuming it's well-formed —
//! see [`scan`]'s docs for exactly how each case is handled.
//!
//! Lists whatever's on disk — not yet wired to `engine_asset`'s
//! [`AssetDatabase`](engine_asset::AssetDatabase) (which tracks
//! *registered* assets by UUID, not files/paths at all) or to the
//! importers that would actually load a selected file. That wiring —
//! selecting a file here registers/imports it — is future work.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::EditorError;

/// How deep [`scan`] will recurse into subdirectories before stopping —
/// bounds both pathological directory nesting and symlink cycles (there's
/// no other cycle detection here, since [`std::fs::DirEntry::file_type`]
/// deliberately doesn't follow symlinks, so a symlink back to an ancestor
/// directory is treated as a fresh, ordinary — if never-terminating
/// without this cap — subdirectory).
const MAX_SCAN_DEPTH: u32 = 16;

/// A coarse content-type classification for an asset file, guessed from
/// its extension — good enough to group entries in the browser; not a
/// substitute for actually importing the file (see `engine_asset`'s
/// importers for that).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetKind {
    /// `.gltf`/`.glb` — see `engine_asset::import_gltf_slice`.
    Mesh,
    /// `.png`/`.jpg`/`.jpeg` — see `engine_asset::import_texture_bytes`.
    Texture,
    /// `.wav` — see `engine_asset::import_wav_bytes`.
    Audio,
    /// `.ron`, this engine's scene format — see `engine_scene`.
    Scene,
    /// Anything else.
    Other,
}

impl AssetKind {
    fn from_extension(extension: &str) -> Self {
        match extension.to_ascii_lowercase().as_str() {
            "gltf" | "glb" => Self::Mesh,
            "png" | "jpg" | "jpeg" => Self::Texture,
            "wav" => Self::Audio,
            "ron" => Self::Scene,
            _ => Self::Other,
        }
    }

    /// A short label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mesh => "Mesh",
            Self::Texture => "Texture",
            Self::Audio => "Audio",
            Self::Scene => "Scene",
            Self::Other => "Other",
        }
    }
}

/// One file found under an assets directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// Path relative to the scanned root — what the browser displays and
    /// what a selection refers to.
    pub relative_path: PathBuf,
    /// Guessed from the file's extension.
    pub kind: AssetKind,
}

impl AssetEntry {
    /// The file name (last path component) — what a single row's label
    /// shows, since `relative_path` can be nested.
    pub fn name(&self) -> String {
        self.relative_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.relative_path.to_string_lossy().into_owned())
    }
}

/// Recursively lists every file under `root`, sorted by relative path.
///
/// `root` not existing (or not being a directory) isn't an error — just
/// an empty result — since "no assets directory yet" is a normal project
/// state, not a failure.
///
/// # Errors
///
/// Returns [`EditorError::AssetScan`] if `root` exists but a directory
/// entry inside it can't be read (permissions, I/O error, ...).
pub fn scan(root: &Path) -> Result<Vec<AssetEntry>, EditorError> {
    let mut entries = Vec::new();
    if root.is_dir() {
        scan_into(root, root, 0, &mut entries)?;
    }
    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(entries)
}

fn scan_into(
    root: &Path,
    dir: &Path,
    depth: u32,
    entries: &mut Vec<AssetEntry>,
) -> Result<(), EditorError> {
    if depth >= MAX_SCAN_DEPTH {
        return Ok(());
    }
    let read_dir = fs::read_dir(dir).map_err(|source| EditorError::AssetScan {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|source| EditorError::AssetScan {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| EditorError::AssetScan {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            scan_into(root, &path, depth + 1, entries)?;
        } else if file_type.is_file() {
            let relative_path = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .to_path_buf();
            let kind = relative_path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(AssetKind::from_extension)
                .unwrap_or(AssetKind::Other);
            entries.push(AssetEntry {
                relative_path,
                kind,
            });
        }
        // Anything that's neither (symlinks — `file_type()` doesn't
        // follow them, unlike `Path::is_dir`/`is_file`) is skipped
        // rather than followed, keeping symlink-cycle risk at zero on
        // top of the depth cap above.
    }
    Ok(())
}

/// Draws the asset browser: `entries` grouped by [`AssetKind`], each a
/// collapsible section of selectable rows. Updates `*selected` when a
/// row is clicked.
pub fn show(ui: &mut egui::Ui, entries: &[AssetEntry], selected: &mut Option<PathBuf>) {
    if entries.is_empty() {
        ui.label("No assets found.");
        return;
    }

    let mut kinds: Vec<AssetKind> = entries.iter().map(|entry| entry.kind).collect();
    kinds.sort();
    kinds.dedup();

    for kind in kinds {
        egui::CollapsingHeader::new(kind.label())
            .default_open(true)
            .show(ui, |ui| {
                for entry in entries.iter().filter(|entry| entry.kind == kind) {
                    let is_selected = selected.as_deref() == Some(entry.relative_path.as_path());
                    if ui.selectable_label(is_selected, entry.name()).clicked() {
                        *selected = Some(entry.relative_path.clone());
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-engine_editor-assets-test-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_of_a_nonexistent_directory_returns_empty() {
        let missing = std::env::temp_dir().join("vge-this-path-does-not-exist-hopefully");
        assert_eq!(scan(&missing).unwrap(), Vec::new());
    }

    #[test]
    fn scan_of_an_empty_directory_returns_empty() {
        let dir = temp_dir("empty");
        assert_eq!(scan(&dir).unwrap(), Vec::new());
    }

    #[test]
    fn scan_finds_top_level_files_and_classifies_by_extension() {
        let dir = temp_dir("top-level");
        std::fs::write(dir.join("cube.gltf"), b"").unwrap();
        std::fs::write(dir.join("brick.png"), b"").unwrap();
        std::fs::write(dir.join("notes.txt"), b"").unwrap();

        let mut entries = scan(&dir).unwrap();
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        assert_eq!(
            entries,
            vec![
                AssetEntry {
                    relative_path: PathBuf::from("brick.png"),
                    kind: AssetKind::Texture,
                },
                AssetEntry {
                    relative_path: PathBuf::from("cube.gltf"),
                    kind: AssetKind::Mesh,
                },
                AssetEntry {
                    relative_path: PathBuf::from("notes.txt"),
                    kind: AssetKind::Other,
                },
            ]
        );
    }

    #[test]
    fn scan_recurses_into_subdirectories() {
        let dir = temp_dir("nested");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("hit.wav"), b"").unwrap();

        let entries = scan(&dir).unwrap();
        assert_eq!(
            entries,
            vec![AssetEntry {
                relative_path: PathBuf::from("audio").join("hit.wav"),
                kind: AssetKind::Audio,
            }]
        );
    }

    #[test]
    fn scan_results_are_sorted_by_relative_path() {
        let dir = temp_dir("sorted");
        std::fs::write(dir.join("b.png"), b"").unwrap();
        std::fs::write(dir.join("a.png"), b"").unwrap();

        let entries = scan(&dir).unwrap();
        let paths: Vec<_> = entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("a.png"), PathBuf::from("b.png")]);
    }

    #[test]
    fn asset_entry_name_is_the_final_path_component() {
        let entry = AssetEntry {
            relative_path: PathBuf::from("audio").join("hit.wav"),
            kind: AssetKind::Audio,
        };
        assert_eq!(entry.name(), "hit.wav");
    }

    #[test]
    fn asset_kind_label_is_non_empty_for_every_kind() {
        for kind in [
            AssetKind::Mesh,
            AssetKind::Texture,
            AssetKind::Audio,
            AssetKind::Scene,
            AssetKind::Other,
        ] {
            assert!(!kind.label().is_empty());
        }
    }
}
