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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use engine_asset::{AssetId, AssetMeta};

use crate::error::EditorError;
use crate::import::ImportStats;

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
    /// `.prefab`, a reusable single-entity template — see
    /// `engine_scene::Prefab`. Dropped on the Scene View to instantiate.
    Prefab,
    /// Anything else.
    Other,
}

impl AssetKind {
    /// The kind implied by `path`'s extension, or [`AssetKind::Other`]
    /// if it has none this editor recognizes.
    pub fn from_path(path: &std::path::Path) -> Self {
        path.extension()
            .and_then(|extension| extension.to_str())
            .map_or(Self::Other, Self::from_extension)
    }

    fn from_extension(extension: &str) -> Self {
        match extension.to_ascii_lowercase().as_str() {
            "gltf" | "glb" => Self::Mesh,
            "png" | "jpg" | "jpeg" => Self::Texture,
            "wav" => Self::Audio,
            "ron" => Self::Scene,
            "prefab" => Self::Prefab,
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
            Self::Prefab => "Prefab",
            Self::Other => "Other",
        }
    }

    /// Whether entities reference files of this kind (mesh / texture /
    /// audio) — the kinds [`resolve_ids`] gives a stable `.meta` id.
    /// Scenes and loose files are addressed by path only.
    pub fn is_referenceable(self) -> bool {
        matches!(self, Self::Mesh | Self::Texture | Self::Audio)
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
    /// Stable id from the file's `.meta` sidecar, once [`resolve_ids`]
    /// has run for it. `None` for kinds that aren't
    /// [`AssetKind::is_referenceable`], or if the sidecar couldn't be
    /// read/written.
    pub id: Option<AssetId>,
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

/// A two-way map between an asset's stable id and its current
/// project-relative path — built by [`resolve_ids`] from the `.meta`
/// sidecars on disk. The editor uses it to re-point a scene's
/// `AssetSource` references at a file that has been renamed or moved
/// (the `.meta` travels with the file, so its id is unchanged).
#[derive(Debug, Default, Clone)]
pub struct AssetIndex {
    by_id: HashMap<AssetId, PathBuf>,
    by_path: HashMap<PathBuf, AssetId>,
}

impl AssetIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one id ↔ relative-path pair (both directions).
    pub(crate) fn insert(&mut self, id: AssetId, relative_path: PathBuf) {
        self.by_id.insert(id, relative_path.clone());
        self.by_path.insert(relative_path, id);
    }

    /// The current relative path for `id`, if the index knows it.
    pub fn path_for(&self, id: AssetId) -> Option<&Path> {
        self.by_id.get(&id).map(PathBuf::as_path)
    }

    /// The id recorded for `relative_path`, if any.
    pub fn id_for(&self, relative_path: &Path) -> Option<AssetId> {
        self.by_path.get(relative_path).copied()
    }

    /// The current relative path for `id`, or `fallback` if the id is
    /// unknown (e.g. the source file was deleted).
    pub fn resolve<'a>(&'a self, id: AssetId, fallback: &'a Path) -> &'a Path {
        self.path_for(id).unwrap_or(fallback)
    }

    /// How many ids are indexed.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Parses canonical UUID text (as stored in
/// `engine_ecs::AssetSource::id` and `engine_scene`'s `asset_id`) into
/// an [`AssetId`]. `None` if the text isn't a valid UUID.
pub fn parse_asset_id(text: &str) -> Option<AssetId> {
    uuid::Uuid::parse_str(text).ok().map(AssetId::from_uuid)
}

/// Ensures every [`AssetKind::is_referenceable`] entry has a `.meta`
/// sidecar (creating one with a fresh id if absent), fills in each
/// entry's [`AssetEntry::id`], and returns the id↔path index.
///
/// `root` is the assets directory the `relative_path`s are relative to.
/// A sidecar that can't be read or written is logged and that entry is
/// left without an id — a partial index is better than none.
pub fn resolve_ids(root: &Path, entries: &mut [AssetEntry]) -> AssetIndex {
    let mut index = AssetIndex::new();
    for entry in entries.iter_mut() {
        if !entry.kind.is_referenceable() {
            continue;
        }
        let absolute = root.join(&entry.relative_path);
        match AssetMeta::load_or_create(&absolute) {
            Ok(meta) => {
                let id = AssetId::from_uuid(meta.id);
                entry.id = Some(id);
                index.insert(id, entry.relative_path.clone());
            }
            Err(err) => {
                tracing::warn!(
                    path = %absolute.display(), error = %err,
                    "could not read or create the asset .meta sidecar"
                );
            }
        }
    }
    index
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
                id: None,
            });
        }
        // Anything that's neither (symlinks — `file_type()` doesn't
        // follow them, unlike `Path::is_dir`/`is_file`) is skipped
        // rather than followed, keeping symlink-cycle risk at zero on
        // top of the depth cap above.
    }
    Ok(())
}

/// Whether `path`, by its extension, is a text file the integrated code
/// editor should open on double-click.
pub fn is_text_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("rs" | "toml" | "ron" | "wgsl" | "glsl" | "md" | "txt" | "json" | "cfg")
    )
}

/// Draws the asset browser: `entries` grouped by [`AssetKind`], each a
/// collapsible section of rows. A row click updates `*selected`;
/// double-clicking a text file (see [`is_text_file`]) puts its
/// [`AssetEntry::relative_path`] into `*open_request` for the host to
/// open in the code editor. A row can also be dragged onto the Scene
/// View, carrying its `relative_path` as an egui drag-and-drop payload
/// (see [`crate::EditorState::spawn_asset_entity`]).
///
/// `stats` is the last import pass's tally (see [`ImportStats`]); a
/// non-empty pass renders a one-line summary above the list.
pub fn show(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
    stats: ImportStats,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    if stats.total > 0 {
        ui.label(
            egui::RichText::new(format!(
                "{} imported \u{b7} {} cached \u{b7} {} failed",
                stats.imported, stats.cached, stats.failed
            ))
            .weak()
            .small(),
        );
    }

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
                    let row_id = egui::Id::new(("vge_asset_row", &entry.relative_path));
                    let response = ui
                        .dnd_drag_source(row_id, entry.relative_path.clone(), |ui| {
                            let _ = ui.selectable_label(is_selected, entry.name());
                        })
                        .response;
                    if response.clicked() {
                        *selected = Some(entry.relative_path.clone());
                    }
                    if response.double_clicked() && is_text_file(&entry.relative_path) {
                        *open_request = Some(entry.relative_path.clone());
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
    fn is_text_file_matches_editable_extensions() {
        assert!(is_text_file(Path::new("src/player.rs")));
        assert!(is_text_file(Path::new("Cargo.TOML")));
        assert!(is_text_file(Path::new("scenes/main.ron")));
        assert!(is_text_file(Path::new("shaders/pbr.wgsl")));
        assert!(!is_text_file(Path::new("models/house.glb")));
        assert!(!is_text_file(Path::new("textures/grass.png")));
        assert!(!is_text_file(Path::new("noextension")));
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
                    id: None,
                },
                AssetEntry {
                    relative_path: PathBuf::from("cube.gltf"),
                    kind: AssetKind::Mesh,
                    id: None,
                },
                AssetEntry {
                    relative_path: PathBuf::from("notes.txt"),
                    kind: AssetKind::Other,
                    id: None,
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
                id: None,
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
            id: None,
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
            AssetKind::Prefab,
            AssetKind::Other,
        ] {
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn dot_prefab_classifies_as_prefab_and_is_not_referenceable() {
        assert_eq!(
            AssetKind::from_extension("prefab"),
            AssetKind::Prefab,
            "case-insensitive extension match"
        );
        assert!(!AssetKind::Prefab.is_referenceable());
    }

    #[test]
    fn resolve_ids_writes_meta_for_referenceable_kinds_only() {
        let dir = temp_dir("resolve-ids");
        std::fs::write(dir.join("brick.png"), b"pixels").unwrap();
        std::fs::write(dir.join("hit.wav"), b"riff").unwrap();
        std::fs::write(dir.join("notes.txt"), b"text").unwrap();
        std::fs::write(dir.join("main.ron"), b"()").unwrap();

        let mut entries = scan(&dir).unwrap();
        let index = resolve_ids(&dir, &mut entries);

        let id_of = |name: &str| {
            entries
                .iter()
                .find(|e| e.relative_path == Path::new(name))
                .unwrap()
                .id
        };
        assert!(id_of("brick.png").is_some());
        assert!(id_of("hit.wav").is_some());
        assert!(id_of("notes.txt").is_none(), "Other kind gets no id");
        assert!(id_of("main.ron").is_none(), "Scene kind gets no id");

        assert!(dir.join("brick.png.meta").exists());
        assert!(!dir.join("notes.txt.meta").exists());

        // Index is two-way and consistent.
        let brick_id = id_of("brick.png").unwrap();
        assert_eq!(index.path_for(brick_id), Some(Path::new("brick.png")));
        assert_eq!(index.id_for(Path::new("brick.png")), Some(brick_id));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn asset_id_survives_a_rename_and_resolve_falls_back() {
        let dir = temp_dir("rename");
        std::fs::write(dir.join("old.png"), b"pixels").unwrap();

        let mut entries = scan(&dir).unwrap();
        resolve_ids(&dir, &mut entries);
        let original_id = entries[0].id.unwrap();

        // Rename the source *and* its sidecar (as a move that carries the
        // .meta would).
        std::fs::rename(dir.join("old.png"), dir.join("new.png")).unwrap();
        std::fs::rename(dir.join("old.png.meta"), dir.join("new.png.meta")).unwrap();

        let mut entries = scan(&dir).unwrap();
        let index = resolve_ids(&dir, &mut entries);

        assert_eq!(entries[0].relative_path, Path::new("new.png"));
        assert_eq!(
            entries[0].id,
            Some(original_id),
            "id is stable across rename"
        );
        assert_eq!(index.path_for(original_id), Some(Path::new("new.png")));

        // An unknown id resolves to the caller's fallback path.
        let stray = AssetId::new();
        assert_eq!(
            index.resolve(stray, Path::new("stale/old.png")),
            Path::new("stale/old.png")
        );
    }
}
