//! Asset browser panel: lists files under a project's assets directory.
#![cfg_attr(feature = "dock-shell", allow(dead_code))]
//!
//! Directory-scanning logic ([`scan`]) is split out from the egui
//! drawing ([`show`]) for the same reason [`crate::hierarchy`]/
//! [`crate::inspector`] split theirs — testable without a live UI.
//!
//! Treats the assets directory as untrusted input (missing, unreadable,
//! or containing symlink cycles) rather than assuming it's well-formed —
//! see [`scan`]'s docs for exactly how each case is handled.
//!
//! Lists whatever's on disk. Importing is handled by [`crate::import`]; the
//! browser itself stays responsible only for scanning and presenting paths.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use egui::AtomExt as _;
use engine_asset::{AssetId, AssetMeta};
use serde::{Deserialize, Serialize};

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
    /// `.gltf`/`.glb` — see `engine_asset::import_gltf_file`.
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
    scan_with_folders(root).map(|(entries, _)| entries)
}

/// Scans files and directory paths for the folder tree, including empty directories.
pub fn scan_with_folders(root: &Path) -> Result<(Vec<AssetEntry>, Vec<PathBuf>), EditorError> {
    let mut entries = Vec::new();
    let mut folders = Vec::new();
    if root.is_dir() {
        scan_into(root, root, 0, &mut entries, &mut folders)?;
    }
    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    folders.sort();
    Ok((entries, folders))
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

    /// Keeps stable IDs attached to their files after a validated move.
    pub fn relocate_paths(&mut self, from: &Path, to: &Path) {
        for path in self.by_id.values_mut() {
            if let Ok(suffix) = path.strip_prefix(from) {
                *path = to.join(suffix);
            }
        }
        self.by_path = self
            .by_id
            .iter()
            .map(|(id, path)| (path.clone(), *id))
            .collect();
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
    folders: &mut Vec<PathBuf>,
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
        // Hidden files and directories are the tool's own bookkeeping
        // (`.DS_Store`, an editor's dotfolder), not project assets.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if file_type.is_dir() {
            folders.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
            scan_into(root, &path, depth + 1, entries, folders)?;
        } else if file_type.is_file() {
            // `.meta` sidecars belong to the importer, not the project.
            // Listing them doubles every row in the browser and offers
            // the user a file they must never edit or drag into a scene.
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("meta"))
            {
                continue;
            }
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

/// How the Asset Browser arranges its rows.
///
/// Two views of the same files, because the two questions a person asks
/// of a project are different ones: *where does this live* (the folder
/// layout they created) and *what have I got* (every texture, wherever
/// it sits). Persisted per project in the editor session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetView {
    /// Mirror the directory layout under `assets/`.
    #[default]
    Folders,
    /// Group by [`AssetKind`], ignoring directories.
    Kinds,
}

impl AssetView {
    /// A short label for the view switcher.
    pub fn label(self) -> &'static str {
        match self {
            Self::Folders => "Folders",
            Self::Kinds => "Kinds",
        }
    }
}

/// The Asset Browser's own state: how it groups rows, which folder new
/// assets land in, and the two requests it hands back to the host.
///
/// The panel itself does no filesystem work — it records what the user
/// asked for and the shell binary carries it out, the same split the
/// File menu uses for [`crate::ProjectRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetBrowser {
    /// Folder tree or by-kind grouping.
    pub view: AssetView,
    /// The folder new assets are imported into and new folders are
    /// created in, relative to the assets root. Empty is the root.
    pub target_dir: PathBuf,
    /// Directories found on disk, including empty ones.
    pub folders: Vec<PathBuf>,
    /// The half-typed name of a new folder while the prompt is open;
    /// `None` when it is closed.
    pub new_folder: Option<String>,
    /// Folder selected when the new-folder button was clicked.
    new_folder_parent: PathBuf,
    /// Focus the name field only on the frame the prompt first opens.
    new_folder_needs_focus: bool,
    /// A folder the host should create, relative to the assets root.
    /// Taken by the host, which validates it before touching the disk.
    pub new_folder_request: Option<PathBuf>,
    /// Window-local logical pointer position supplied with an OS file drop.
    pub drop_position: Option<(i32, i32)>,
    /// File operation requested by the tree; the host validates and performs it.
    pub operation_request: Option<AssetOperation>,
    /// Last asset-operation failure, shown in the browser until another action.
    pub operation_error: Option<String>,
    clipboard: Option<AssetClipboard>,
    /// Existing asset to reveal in the platform file manager.
    pub reveal_request: Option<PathBuf>,
    /// Existing asset whose absolute path should go to the system clipboard.
    pub copy_path_request: Option<PathBuf>,
    renaming: Option<AssetRename>,
    delete_confirmation: Option<PathBuf>,
    /// Whether the selected-asset preview pane is visible.
    pub preview_open: bool,
}

impl Default for AssetBrowser {
    fn default() -> Self {
        Self {
            view: AssetView::default(),
            target_dir: PathBuf::new(),
            folders: Vec::new(),
            new_folder: None,
            new_folder_parent: PathBuf::new(),
            new_folder_needs_focus: false,
            new_folder_request: None,
            drop_position: None,
            operation_request: None,
            operation_error: None,
            clipboard: None,
            reveal_request: None,
            copy_path_request: None,
            renaming: None,
            delete_confirmation: None,
            preview_open: true,
        }
    }
}

/// A validated filesystem change requested by the Project Assets tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetOperation {
    /// Duplicate an asset into a folder, assigning fresh metadata IDs.
    Copy {
        /// Source path relative to the assets root.
        from: PathBuf,
        /// Destination folder relative to the assets root; empty means root.
        into: PathBuf,
    },
    /// Move or rename an asset, preserving its metadata.
    Relocate {
        /// Current path relative to the assets root.
        from: PathBuf,
        /// New path relative to the assets root.
        to: PathBuf,
    },
    /// Delete an asset after user confirmation.
    Delete {
        /// Path relative to the assets root.
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetRename {
    path: PathBuf,
    draft: String,
    focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetClipboard {
    path: PathBuf,
    cut: bool,
}

impl AssetBrowser {
    /// A cut item is consumed when its move succeeds; copied items stay pasteable.
    pub fn asset_moved(&mut self, from: &Path, to: &Path) {
        if let Some(clipboard) = &mut self.clipboard
            && let Ok(suffix) = clipboard.path.strip_prefix(from)
        {
            if clipboard.cut {
                self.clipboard = None;
            } else {
                clipboard.path = to.join(suffix);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FolderDrag(PathBuf);

/// One directory in the [`AssetView::Folders`] tree, built by
/// [`build_tree`] from a flat entry list.
///
/// `files` holds indices into the slice the tree was built from rather
/// than cloned entries — the tree is rebuilt every frame from data the
/// caller already owns, so it should not copy paths to do it. That
/// rebuild is one pass over the entries plus a short linear search per
/// path component; at a project's scale (hundreds to low thousands of
/// files) it costs less than caching it would cost to invalidate
/// correctly on every import, rename and hot-reload.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AssetFolder {
    /// This directory's own name. Empty for the root.
    pub name: String,
    /// Path relative to the assets root. Empty for the root — also the
    /// egui id salt, so two folders with the same name stay distinct.
    pub path: PathBuf,
    /// Subdirectories, sorted by name.
    pub folders: Vec<AssetFolder>,
    /// Indices of the files directly in this directory, in entry order.
    pub files: Vec<usize>,
}

impl AssetFolder {
    /// Total files in this directory and every directory below it.
    pub fn file_count(&self) -> usize {
        self.files.len()
            + self
                .folders
                .iter()
                .map(AssetFolder::file_count)
                .sum::<usize>()
    }

    /// Makes sure `relative` exists in the tree, creating empty folders
    /// down to it.
    ///
    /// [`scan`] only reports files, so a folder someone just made is not
    /// in the tree until something is put in it — and a folder you
    /// cannot see is a folder you cannot import into.
    pub fn ensure(&mut self, relative: &Path) {
        let mut folder = self;
        for component in relative.components() {
            folder = folder.child_mut(&component.as_os_str().to_string_lossy());
        }
    }

    fn child_mut(&mut self, name: &str) -> &mut Self {
        // Linear search: a directory holds a handful of subdirectories,
        // and a map here would cost more than it saves at that size.
        if let Some(position) = self.folders.iter().position(|f| f.name == name) {
            return &mut self.folders[position];
        }
        self.folders.push(Self {
            name: name.to_owned(),
            path: self.path.join(name),
            ..Self::default()
        });
        let last = self.folders.len() - 1;
        &mut self.folders[last]
    }
}

/// Builds the directory tree for `entries` (which [`scan`] has already
/// sorted by relative path) and returns its root.
///
/// An entry whose path has no file name — which [`scan`] never produces,
/// but a hand-built list could — is skipped rather than panicking.
pub fn build_tree(entries: &[AssetEntry]) -> AssetFolder {
    let mut root = AssetFolder::default();
    for (index, entry) in entries.iter().enumerate() {
        let mut folder = &mut root;
        let components: Vec<_> = entry.relative_path.components().collect();
        let Some((_, directories)) = components.split_last() else {
            continue;
        };
        for component in directories {
            folder = folder.child_mut(&component.as_os_str().to_string_lossy());
        }
        folder.files.push(index);
    }
    root
}

/// Scene mode's single expandable tree of folders and files.
pub fn show_scene(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    let mut root = build_tree(entries);
    for folder in &browser.folders {
        root.ensure(folder);
    }
    root.ensure(&browser.target_dir);
    if browser.new_folder.is_some() {
        root.ensure(&browser.new_folder_parent);
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(ui.available_height().max(0.0))
        .min_scrolled_height(0.0)
        .show(ui, |ui| {
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                ui.make_persistent_id("scene_asset_root"),
                true,
            );
            if browser.new_folder.is_some() {
                state.set_open(true);
            }
            let selected_root = selected.is_none() && browser.target_dir.as_os_str().is_empty();
            let selection_shape = ui.painter().add(egui::Shape::Noop);
            let header = ui.horizontal(|ui| {
                folder_disclosure(ui, &mut state);
                folder_label(ui, "assets")
            });
            paint_folder_row(ui, selection_shape, header.response.rect, selected_root);
            if header.inner.clicked()
                || dropped_on_folder(ui, header.response.rect, browser.drop_position)
            {
                browser.target_dir.clear();
                *selected = None;
            }
            folder_drop_target(ui, &header.response, Path::new(""), browser);
            header.inner.context_menu(|ui| {
                style_asset_context_menu(ui);
                paste_menu_item(ui, browser, Path::new(""));
                if ui.button("New Folder").clicked() {
                    browser.target_dir.clear();
                    browser.new_folder = Some(String::new());
                    browser.new_folder_parent.clear();
                    browser.new_folder_needs_focus = true;
                    ui.close();
                }
            });
            state.show_body_indented(&header.response, ui, |ui| {
                show_folder_contents(ui, &root, entries, browser, selected, open_request);
            });
        });
    delete_confirmation(ui, browser);
    operation_error_dialog(ui, browser);
}

/// Draws the asset browser: a directory tree or a by-kind grouping (see
/// [`AssetView`]), inside a scroll area.
///
/// The scroll area is what keeps the panel a panel. Without it the
/// list's height *is* the panel's height, egui persists that, and a
/// project with a few dozen files silently grows the browser until it
/// has eaten the whole dock and is drawing over the Hierarchy.
///
/// A row click updates `*selected`; double-clicking a text file (see
/// [`is_text_file`]) puts its [`AssetEntry::relative_path`] into
/// `*open_request` for the host to open in the code editor. A row can
/// also be dragged onto the Scene View, carrying its `relative_path` as
/// an egui drag-and-drop payload (see
/// [`crate::EditorState::spawn_asset_entity`]).
///
/// `stats` is the last import pass's tally (see [`ImportStats`]); a
/// non-empty pass renders a one-line summary beside the view switcher.
#[cfg_attr(not(feature = "dock-shell"), allow(dead_code))]
pub fn show(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
    stats: ImportStats,
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    ui.horizontal(|ui| {
        ui.menu_button("⋯", |ui| {
            for option in [AssetView::Folders, AssetView::Kinds] {
                ui.selectable_value(&mut browser.view, option, option.label());
            }
        });
        if stats.total > 0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} imported \u{b7} {} cached \u{b7} {} failed",
                        stats.imported, stats.cached, stats.failed
                    ))
                    .weak()
                    .small(),
                );
            });
        }
    });

    if browser.view == AssetView::Folders {
        target_folder_bar(ui, browser);
    }

    // Bound the list to the space left in its panel, even when squeezed.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(ui.available_height().max(0.0))
        .min_scrolled_height(0.0)
        .show(ui, |ui| {
            if entries.is_empty()
                && browser.folders.is_empty()
                && browser.target_dir.as_os_str().is_empty()
                && browser.new_folder.is_none()
            {
                ui.label("No assets found.");
                return;
            }
            match browser.view {
                AssetView::Folders => {
                    let mut root = build_tree(entries);
                    for folder in &browser.folders {
                        root.ensure(folder);
                    }
                    root.ensure(&browser.target_dir);
                    if browser.new_folder.is_some() {
                        root.ensure(&browser.new_folder_parent);
                    }
                    show_folder_contents(ui, &root, entries, browser, selected, open_request);
                }
                AssetView::Kinds => show_kinds(ui, entries, browser, selected, open_request),
            }
        });
    delete_confirmation(ui, browser);
    operation_error_dialog(ui, browser);
}

/// The Asset Browser panel's largest allowed height in a dock with
/// `available` points — 70%, so the Hierarchy above it always keeps a
/// usable share.
///
/// A bottom panel is capped only by its parent's size, so without this a
/// browser dragged (or grown) to the full dock leaves the Hierarchy zero
/// height and the two draw over each other. Panels stacked in one column
/// have to leave room for each other; nobody else will.
pub fn browser_panel_max(available: f32) -> f32 {
    (available * 0.7).max(BROWSER_MIN_HEIGHT)
}

/// Smallest the Asset Browser panel is allowed to get.
pub const BROWSER_MIN_HEIGHT: f32 = 90.0;

/// The row above the tree: which folder is the target for an import or a
/// new folder, and the button that makes one.
///
/// A project keeps its textures in `textures/` and its audio in `audio/`
/// because someone put them there. Without this the browser can only
/// ever show one flat heap — every import lands in the assets root.
fn target_folder_bar(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            new_folder_button(ui, browser);
        });
    });
}

pub(crate) fn new_folder_button(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    let response = ui
        .add(
            egui::Button::new((
                egui::Atom::default().atom_size(egui::vec2(14.0, 14.0)),
                "Folder",
            ))
            .min_size(egui::vec2(76.0, 24.0))
            .corner_radius(6),
        )
        .on_hover_text("New folder here");
    let center = response.rect.left_center() + egui::vec2(ui.spacing().button_padding.x + 7.0, 0.0);
    let stroke = egui::Stroke::new(1.5, ui.visuals().text_color());
    ui.painter().line_segment(
        [
            center + egui::vec2(-4.0, 0.0),
            center + egui::vec2(4.0, 0.0),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(0.0, -4.0),
            center + egui::vec2(0.0, 4.0),
        ],
        stroke,
    );
    if response.clicked() {
        browser.new_folder = Some(String::new());
        browser.new_folder_parent = browser.target_dir.clone();
        browser.new_folder_needs_focus = true;
    }
}

fn new_folder_prompt(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    let Some(name) = browser.new_folder.as_mut() else {
        return;
    };
    let (response, cancel) = ui
        .horizontal(|ui| {
            ui.add_space(18.0);
            folder_icon(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cancel = ui.small_button("Cancel").clicked();
                let response = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("folder name")
                        .desired_width(ui.available_width()),
                );
                (response, cancel)
            })
            .inner
        })
        .inner;
    if browser.new_folder_needs_focus {
        response.request_focus();
        browser.new_folder_needs_focus = false;
    }
    if cancel || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        browser.new_folder = None;
        return;
    }
    if response.lost_focus() {
        let typed = std::mem::take(name);
        let typed = typed.trim();
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !typed.is_empty() {
            browser.new_folder_request = Some(browser.new_folder_parent.join(typed));
        }
        browser.new_folder = None;
    }
}

fn begin_rename(browser: &mut AssetBrowser, path: &Path) {
    browser.renaming = Some(AssetRename {
        path: path.to_path_buf(),
        draft: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        focus: true,
    });
    browser.operation_error = None;
}

fn rename_prompt(ui: &mut egui::Ui, browser: &mut AssetBrowser) -> egui::Response {
    let Some(renaming) = browser.renaming.as_mut() else {
        return ui.label("");
    };
    let response =
        ui.add(egui::TextEdit::singleline(&mut renaming.draft).desired_width(ui.available_width()));
    if renaming.focus {
        response.request_focus();
        renaming.focus = false;
    }
    let cancel = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let submit = ui.input(|input| input.key_pressed(egui::Key::Enter));
    if cancel || submit || response.lost_focus() {
        let mut close = true;
        if submit && !cancel {
            let name = renaming.draft.trim();
            let single_name = std::path::Path::new(name)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                && std::path::Path::new(name).components().count() == 1
                && !name.starts_with('.')
                && !name.to_ascii_lowercase().ends_with(".meta");
            if !single_name {
                browser.operation_error = Some("Enter a single visible asset name".to_owned());
                close = false;
            } else if name != renaming.path.file_name().unwrap_or_default() {
                browser.operation_request = Some(AssetOperation::Relocate {
                    from: renaming.path.clone(),
                    to: renaming.path.with_file_name(name),
                });
            }
        }
        if close {
            browser.renaming = None;
        }
    }
    response
}

fn style_asset_context_menu(ui: &mut egui::Ui) {
    ui.set_min_width(180.0);
    ui.spacing_mut().button_padding = egui::vec2(10.0, 7.0);
    ui.spacing_mut().item_spacing.y = 4.0;
}

fn asset_context_menu(ui: &mut egui::Ui, browser: &mut AssetBrowser, path: &Path) {
    style_asset_context_menu(ui);
    if ui.button("Copy").clicked() {
        browser.clipboard = Some(AssetClipboard {
            path: path.to_path_buf(),
            cut: false,
        });
        ui.close();
    }
    if ui.button("Cut").clicked() {
        browser.clipboard = Some(AssetClipboard {
            path: path.to_path_buf(),
            cut: true,
        });
        ui.close();
    }
    if browser.folders.iter().any(|folder| folder == path) {
        paste_menu_item(ui, browser, path);
    }
    if ui.button("Rename").clicked() {
        begin_rename(browser, path);
        ui.close();
    }
    if ui.button("Delete").clicked() {
        browser.delete_confirmation = Some(path.to_path_buf());
        browser.operation_error = None;
        ui.close();
    }
    #[cfg(target_os = "macos")]
    let reveal_label = "Reveal in Finder";
    #[cfg(not(target_os = "macos"))]
    let reveal_label = "Show in File Manager";
    if ui.button(reveal_label).clicked() {
        browser.reveal_request = Some(path.to_path_buf());
        ui.close();
    }
    if ui.button("Copy Path").clicked() {
        browser.copy_path_request = Some(path.to_path_buf());
        ui.close();
    }
}

fn paste_menu_item(ui: &mut egui::Ui, browser: &mut AssetBrowser, target: &Path) {
    if ui
        .add_enabled(browser.clipboard.is_some(), egui::Button::new("Paste"))
        .clicked()
    {
        queue_paste(browser, target);
        ui.close();
    }
}

fn queue_paste(browser: &mut AssetBrowser, target: &Path) {
    let Some(clipboard) = &browser.clipboard else {
        return;
    };
    if clipboard.cut {
        if let Some(to) = move_destination(&clipboard.path, false, target) {
            browser.operation_request = Some(AssetOperation::Relocate {
                from: clipboard.path.clone(),
                to,
            });
        }
    } else {
        browser.operation_request = Some(AssetOperation::Copy {
            from: clipboard.path.clone(),
            into: target.to_path_buf(),
        });
    }
}

fn delete_confirmation(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    let Some(path) = browser.delete_confirmation.clone() else {
        return;
    };
    let response = crate::theme::modal(
        ui.ctx(),
        ui.make_persistent_id("delete_asset_modal"),
        |ui| {
            ui.set_width(320.0);
            ui.heading("Delete asset?");
            ui.label(format!(
                "Delete {} and its contents permanently?",
                path.display()
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("Delete").fill(egui::Color32::from_rgb(115, 47, 56)))
                    .clicked()
                {
                    browser.operation_request = Some(AssetOperation::Delete { path: path.clone() });
                    browser.delete_confirmation = None;
                }
                if ui.button("Cancel").clicked() {
                    browser.delete_confirmation = None;
                }
            });
        },
    );
    if response.should_close() {
        browser.delete_confirmation = None;
    }
}

fn operation_error_dialog(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    let Some(error) = browser.operation_error.clone() else {
        return;
    };
    let response = crate::theme::modal(
        ui.ctx(),
        ui.make_persistent_id("asset_operation_error_modal"),
        |ui| {
            ui.set_width(340.0);
            ui.heading("Could not change asset");
            ui.label(error);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("OK").fill(crate::theme::ACCENT_FILL))
                    .clicked()
                {
                    browser.operation_error = None;
                }
            });
        },
    );
    if response.should_close() {
        browser.operation_error = None;
    }
}

fn move_destination(from: &Path, is_dir: bool, target: &Path) -> Option<PathBuf> {
    if is_dir && (from == target || target.starts_with(from)) {
        return None;
    }
    let destination = target.join(from.file_name()?);
    (destination != from).then_some(destination)
}

fn folder_drop_target(
    ui: &egui::Ui,
    response: &egui::Response,
    target: &Path,
    browser: &mut AssetBrowser,
) {
    let file = response.dnd_hover_payload::<PathBuf>();
    let folder = response.dnd_hover_payload::<FolderDrag>();
    let file_destination = file
        .as_ref()
        .and_then(|from| move_destination(from, false, target));
    let folder_destination = folder
        .as_ref()
        .and_then(|from| move_destination(&from.0, true, target));
    let external_hover = ui.input(|input| {
        !input.raw.hovered_files.is_empty()
            && input
                .pointer
                .hover_pos()
                .is_some_and(|pos| response.rect.contains(pos))
    });
    if file_destination.is_some() || folder_destination.is_some() || external_hover {
        ui.painter().rect_filled(
            response.rect,
            3.0,
            ui.visuals().selection.bg_fill.linear_multiply(0.6),
        );
    }
    if let Some(to) = file_destination
        && let Some(from) = response.dnd_release_payload::<PathBuf>()
    {
        browser.operation_request = Some(AssetOperation::Relocate {
            from: (*from).clone(),
            to,
        });
    } else if let Some(to) = folder_destination
        && let Some(from) = response.dnd_release_payload::<FolderDrag>()
    {
        browser.operation_request = Some(AssetOperation::Relocate {
            from: from.0.clone(),
            to,
        });
    }
}

/// Draws one directory's subfolders and files. Recurses through
/// [`egui::collapsing_header::CollapsingState`], so a collapsed folder costs one row
/// whatever it holds.
#[cfg_attr(not(feature = "dock-shell"), allow(dead_code))]
fn show_folder_contents(
    ui: &mut egui::Ui,
    folder: &AssetFolder,
    entries: &[AssetEntry],
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    if browser.new_folder.is_some() && browser.new_folder_parent == folder.path {
        new_folder_prompt(ui, browser);
    }
    for child in &folder.folders {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            ui.make_persistent_id(&child.path),
            true,
        );
        if browser.new_folder.is_some() && browser.new_folder_parent.starts_with(&child.path) {
            state.set_open(true);
        }
        let is_renaming = browser
            .renaming
            .as_ref()
            .is_some_and(|renaming| renaming.path == child.path);
        let selected_folder = selected.is_none() && browser.target_dir == child.path;
        let selection_shape = ui.painter().add(egui::Shape::Noop);
        let header = ui.horizontal(|ui| {
            folder_disclosure(ui, &mut state);
            if is_renaming {
                folder_icon(ui);
                rename_prompt(ui, browser)
            } else {
                let response = folder_label(ui, &child.name);
                response.dnd_set_drag_payload(FolderDrag(child.path.clone()));
                response
            }
        });
        if !is_renaming {
            paint_folder_row(ui, selection_shape, header.response.rect, selected_folder);
            if header.inner.clicked()
                || dropped_on_folder(ui, header.response.rect, browser.drop_position)
            {
                browser.target_dir = child.path.clone();
                *selected = None;
            }
            folder_drop_target(ui, &header.response, &child.path, browser);
            header.inner.context_menu(|ui| {
                asset_context_menu(ui, browser, &child.path);
            });
        }
        state.show_body_indented(&header.response, ui, |ui| {
            show_folder_contents(ui, child, entries, browser, selected, open_request);
        });
    }
    for &index in &folder.files {
        if let Some(entry) = entries.get(index) {
            let _ = asset_row(ui, entry, RowLabel::Name, browser, selected, open_request);
        }
    }
}

fn dropped_on_folder(ui: &egui::Ui, rect: egui::Rect, drop_position: Option<(i32, i32)>) -> bool {
    ui.input(|input| {
        !input.raw.dropped_files.is_empty()
            && drop_position
                .map(|(x, y)| egui::pos2(x as f32, y as f32))
                .or_else(|| input.pointer.hover_pos())
                .is_some_and(|pos| rect.contains(pos))
    })
}

fn folder_disclosure(ui: &mut egui::Ui, state: &mut egui::collapsing_header::CollapsingState) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 20.0), egui::Sense::click());
    if response.clicked() {
        state.toggle(ui);
    }
    let center = rect.center();
    let points = if state.is_open() {
        [
            center + egui::vec2(-4.0, -2.0),
            center + egui::vec2(0.0, 2.0),
            center + egui::vec2(4.0, -2.0),
        ]
    } else {
        [
            center + egui::vec2(-2.0, -4.0),
            center + egui::vec2(2.0, 0.0),
            center + egui::vec2(-2.0, 4.0),
        ]
    };
    ui.painter().add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(1.5, ui.visuals().text_color()),
    ));
}

fn paint_folder_row(
    ui: &egui::Ui,
    shape: egui::layers::ShapeIdx,
    rect: egui::Rect,
    selected: bool,
) {
    let fill = if selected {
        Some(ui.visuals().selection.bg_fill)
    } else if ui.rect_contains_pointer(rect) {
        Some(ui.visuals().widgets.hovered.bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter()
            .set(shape, egui::Shape::rect_filled(rect, 6.0, fill));
    }
}

fn folder_label(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let button = egui::Button::selectable(
        false,
        (
            egui::Atom::default().atom_size(egui::vec2(16.0, 16.0)),
            name.to_owned()
                .atom_grow(true)
                .atom_align(egui::Align2::LEFT_CENTER),
        ),
    )
    .fill(egui::Color32::TRANSPARENT);
    let response = ui
        .add_sized(
            [ui.available_width(), ui.spacing().interact_size.y],
            button.truncate().sense(egui::Sense::click_and_drag()),
        )
        .on_hover_text(name);
    paint_tree_icon(ui, &response, 5.0, TreeIcon::Folder);
    response
}

/// Draws the by-kind grouping: one collapsible section per
/// [`AssetKind`] present, every file of that kind in it wherever it
/// lives on disk.
#[cfg_attr(not(feature = "dock-shell"), allow(dead_code))]
fn show_kinds(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    let mut kinds: Vec<AssetKind> = entries.iter().map(|entry| entry.kind).collect();
    kinds.sort();
    kinds.dedup();

    for kind in kinds {
        egui::CollapsingHeader::new(kind.label())
            .default_open(true)
            .show(ui, |ui| {
                for entry in entries.iter().filter(|entry| entry.kind == kind) {
                    let _ = asset_row(ui, entry, RowLabel::Path, browser, selected, open_request);
                }
            });
    }
}

/// What a row shows: its file name (the folder tree already shows where
/// it lives) or its whole relative path (the kind grouping does not, and
/// two `grass.png`s in different folders must stay tellable apart).
#[derive(Clone, Copy)]
enum RowLabel {
    Name,
    #[cfg_attr(not(feature = "dock-shell"), allow(dead_code))]
    Path,
}

#[derive(Clone, Copy)]
enum TreeIcon {
    Folder,
    File(AssetKind),
    Code,
    Config,
}

pub(crate) fn folder_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 16.0), egui::Sense::hover());
    paint_icon(ui, rect.min + egui::vec2(0.0, 1.0), TreeIcon::Folder);
}

fn paint_tree_icon(ui: &egui::Ui, row: &egui::Response, inset: f32, icon: TreeIcon) {
    let origin = row.rect.left_center() + egui::vec2(inset, -7.0);
    paint_icon(ui, origin, icon);
}

fn paint_icon(ui: &egui::Ui, origin: egui::Pos2, icon: TreeIcon) {
    let point = |x: f32, y: f32| origin + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.2, ui.visuals().weak_text_color());
    let painter = ui.painter();
    let outline = match icon {
        TreeIcon::Folder => vec![
            point(1.0, 3.0),
            point(5.0, 3.0),
            point(7.0, 5.0),
            point(13.0, 5.0),
            point(13.0, 12.0),
            point(1.0, 12.0),
        ],
        _ => vec![
            point(2.0, 1.0),
            point(9.0, 1.0),
            point(12.0, 4.0),
            point(12.0, 13.0),
            point(2.0, 13.0),
        ],
    };
    painter.add(egui::Shape::closed_line(outline, stroke));
    match icon {
        TreeIcon::Folder => {}
        TreeIcon::File(AssetKind::Texture) => {
            painter.add(egui::Shape::line(
                vec![
                    point(4.0, 10.0),
                    point(6.0, 8.0),
                    point(8.0, 10.0),
                    point(10.0, 7.0),
                ],
                stroke,
            ));
            painter.circle_stroke(point(5.0, 5.0), 0.8, stroke);
        }
        TreeIcon::File(AssetKind::Mesh) => {
            painter.add(egui::Shape::closed_line(
                vec![
                    point(4.0, 7.0),
                    point(7.0, 5.0),
                    point(10.0, 7.0),
                    point(7.0, 10.0),
                ],
                stroke,
            ));
        }
        TreeIcon::File(AssetKind::Scene) => {
            painter.add(egui::Shape::closed_line(
                vec![
                    point(4.0, 5.0),
                    point(10.0, 5.0),
                    point(10.0, 10.0),
                    point(4.0, 10.0),
                ],
                stroke,
            ));
        }
        TreeIcon::File(AssetKind::Audio) => {
            painter.line_segment([point(6.0, 6.0), point(10.0, 5.0)], stroke);
            painter.line_segment([point(10.0, 5.0), point(10.0, 10.0)], stroke);
            painter.circle_stroke(point(8.0, 10.0), 1.0, stroke);
        }
        TreeIcon::Code => {
            painter.add(egui::Shape::line(
                vec![point(6.0, 6.0), point(4.0, 8.0), point(6.0, 10.0)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![point(8.0, 6.0), point(10.0, 8.0), point(8.0, 10.0)],
                stroke,
            ));
        }
        TreeIcon::Config => {
            painter.circle_stroke(point(7.0, 8.0), 2.0, stroke);
            painter.line_segment([point(7.0, 4.0), point(7.0, 5.0)], stroke);
            painter.line_segment([point(7.0, 11.0), point(7.0, 12.0)], stroke);
        }
        TreeIcon::File(_) => {
            painter.line_segment([point(4.0, 7.0), point(10.0, 7.0)], stroke);
            painter.line_segment([point(4.0, 10.0), point(9.0, 10.0)], stroke);
        }
    }
}

/// One file row: selectable, draggable onto the Scene View, and — for a
/// text file — openable in the code editor by double-click.
fn asset_row(
    ui: &mut egui::Ui,
    entry: &AssetEntry,
    label: RowLabel,
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) -> egui::Response {
    if browser
        .renaming
        .as_ref()
        .is_some_and(|renaming| renaming.path == entry.relative_path)
    {
        return ui
            .horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                paint_icon(ui, rect.min, TreeIcon::File(entry.kind));
                rename_prompt(ui, browser)
            })
            .inner;
    }
    let is_selected = selected.as_deref() == Some(entry.relative_path.as_path());
    let text = match label {
        RowLabel::Name => entry.name(),
        RowLabel::Path => entry.relative_path.to_string_lossy().into_owned(),
    };
    // One response owns both interactions. The old nested shape discarded
    // the selectable label's response and inspected `dnd_drag_source`'s
    // outer drag response instead. That made every hover show a Grab cursor
    // and allowed the inner widget to consume clicks before selection saw
    // them. `dnd_set_drag_payload` deliberately preserves the normal button
    // cursor when its response also senses clicks.
    let response = ui
        .add_sized(
            [ui.available_width(), ui.spacing().interact_size.y],
            egui::Button::selectable(
                is_selected,
                (
                    egui::Atom::default().atom_size(egui::vec2(16.0, 16.0)),
                    text.clone()
                        .atom_grow(true)
                        .atom_align(egui::Align2::LEFT_CENTER),
                ),
            )
            .truncate()
            .sense(egui::Sense::click_and_drag()),
        )
        .on_hover_text(&text);
    let icon = match entry.relative_path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if ["lua", "rs", "wgsl"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind)) =>
        {
            TreeIcon::Code
        }
        Some(ext) if ext.eq_ignore_ascii_case("toml") => TreeIcon::Config,
        Some(ext)
            if ["ogg", "mp3", "flac"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind)) =>
        {
            TreeIcon::File(AssetKind::Audio)
        }
        Some(ext) if ext.eq_ignore_ascii_case("obj") => TreeIcon::File(AssetKind::Mesh),
        Some(ext) if ext.eq_ignore_ascii_case("scene") => TreeIcon::File(AssetKind::Scene),
        _ => TreeIcon::File(entry.kind),
    };
    paint_tree_icon(ui, &response, 5.0, icon);
    response.dnd_set_drag_payload(entry.relative_path.clone());
    if response.clicked() {
        *selected = Some(entry.relative_path.clone());
    }
    if response.double_clicked() && is_text_file(&entry.relative_path) {
        *open_request = Some(entry.relative_path.clone());
    }
    response.context_menu(|ui| {
        asset_context_menu(ui, browser, &entry.relative_path);
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    struct TestDroppedFile(PathBuf);

    impl egui::DroppedFile for TestDroppedFile {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|err| err.to_string())
        }
    }

    #[test]
    fn scan_reports_empty_directories_for_the_folder_tree() {
        let root = std::env::temp_dir().join(format!(
            "vge-empty-folders-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        let (files, folders) = scan_with_folders(&root).unwrap();
        assert!(files.is_empty());
        assert_eq!(
            folders,
            vec![
                PathBuf::from("empty"),
                PathBuf::from("nested"),
                PathBuf::from("nested/deep")
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_folder_stays_in_tree_after_selecting_the_root() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("empty")],
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_scene(ui, &[], &mut browser, &mut selected, &mut open_request);
        });
        let visible = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text() == "empty",
            _ => false,
        });
        output.textures_delta.clear();
        assert!(visible);
    }

    #[test]
    fn empty_legacy_browser_still_shows_the_pending_folder_name() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            new_folder: Some("draft".to_owned()),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(
                ui,
                &[],
                ImportStats::default(),
                &mut browser,
                &mut selected,
                &mut open_request,
            );
        });
        let visible = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text() == "draft",
            _ => false,
        });
        output.textures_delta.clear();
        assert!(visible);
    }

    #[test]
    fn escape_cancels_an_unfinished_new_folder() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            new_folder: Some(String::new()),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        for (time, events) in [
            (0.0, vec![]),
            (
                0.1,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            );
            output.textures_delta.clear();
        }
        assert_eq!(browser.new_folder, None);
        assert_eq!(browser.new_folder_request, None);
    }

    #[test]
    fn cancel_button_discards_a_new_folder_name() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            new_folder: Some("draft".to_owned()),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events, browser: &mut AssetBrowser| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 200.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], browser, &mut selected, &mut open_request),
            )
        };
        let mut output = draw(0.0, vec![], &mut browser);
        let cancel = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Cancel" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
            }
            _ => None,
        });
        output.textures_delta.clear();
        let cancel = cancel.expect("visible Cancel button");
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            let mut output = draw(time, vec![pointer_button(cancel, pressed)], &mut browser);
            output.textures_delta.clear();
        }
        assert_eq!(browser.new_folder, None);
        assert_eq!(browser.new_folder_request, None);
    }

    #[test]
    fn enter_still_requests_a_new_folder() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            new_folder: Some("draft".to_owned()),
            new_folder_needs_focus: true,
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        for (time, events) in [
            (0.0, vec![]),
            (
                0.1,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            );
            output.textures_delta.clear();
        }
        assert_eq!(browser.new_folder, None);
        assert_eq!(browser.new_folder_request, Some(PathBuf::from("draft")));
    }

    #[test]
    fn new_folder_name_appears_inside_the_selected_folder() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("audio/old.wav"),
            kind: AssetKind::Audio,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            target_dir: PathBuf::from("audio"),
            new_folder: Some("draft".to_owned()),
            new_folder_parent: PathBuf::from("audio"),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request);
        });
        let text_y = |label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => Some(text.pos.y),
                _ => None,
            })
        };
        let audio = text_y("audio").expect("selected folder label");
        let draft = text_y("draft").expect("editable folder name");
        let old_file = text_y("old.wav").expect("existing child file");
        output.textures_delta.clear();
        assert!(
            audio < draft && draft < old_file,
            "{audio}, {draft}, {old_file}"
        );
    }

    #[test]
    fn dropping_on_a_nested_folder_selects_it_as_import_target() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("audio/old.wav"),
            kind: AssetKind::Audio,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events, dropped_files, browser: &mut AssetBrowser| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 200.0),
                    )),
                    time: Some(time),
                    events,
                    dropped_files,
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, browser, &mut selected, &mut open_request),
            )
        };
        let mut output = draw(0.0, Vec::new(), Vec::new(), &mut browser);
        let audio = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "audio" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("audio row");
        output.textures_delta.clear();
        let mut output = draw(
            0.1,
            vec![egui::Event::PointerMoved(audio)],
            vec![std::sync::Arc::new(TestDroppedFile(PathBuf::from(
                "/tmp/source",
            )))],
            &mut browser,
        );
        output.textures_delta.clear();
        assert_eq!(browser.target_dir, PathBuf::from("audio"));
    }

    #[test]
    fn drop_uses_native_position_when_egui_pointer_is_stale() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("audio/old.wav"),
            kind: AssetKind::Audio,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request);
        });
        let audio = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "audio" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("audio row");
        output.textures_delta.clear();
        browser.drop_position = Some((audio.x.round() as i32, audio.y.round() as i32));
        let mut output = ctx.run_ui(
            egui::RawInput {
                dropped_files: vec![std::sync::Arc::new(TestDroppedFile(PathBuf::from(
                    "/tmp/source",
                )))],
                ..Default::default()
            },
            |ui| {
                show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request);
            },
        );
        output.textures_delta.clear();
        assert_eq!(browser.target_dir, PathBuf::from("audio"));
    }

    #[test]
    fn dragging_an_asset_over_a_folder_highlights_it_and_moves_on_release() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("source/picture.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("target")],
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events, browser: &mut AssetBrowser| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, Vec::new(), &mut browser);
        let target = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "target" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("target folder row");
        first.textures_delta.clear();

        egui::DragAndDrop::set_payload(&ctx, PathBuf::from("source/picture.png"));
        let mut hovered = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(target),
                pointer_button(target, true),
            ],
            &mut browser,
        );
        let fill = ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .selection
            .bg_fill
            .linear_multiply(0.6);
        assert!(hovered.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => {
                rect.fill == fill && rect.rect.contains(target)
            }
            _ => false,
        }));
        hovered.textures_delta.clear();
        let mut released = draw(0.2, vec![pointer_button(target, false)], &mut browser);
        released.textures_delta.clear();
        assert_eq!(
            browser.operation_request,
            Some(AssetOperation::Relocate {
                from: PathBuf::from("source/picture.png"),
                to: PathBuf::from("target/picture.png"),
            })
        );
    }

    #[test]
    fn asset_operation_error_appears_in_a_dismissible_dialog() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            operation_error: Some("An item named test already exists in that folder.".into()),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
        );
        output.textures_delta.clear();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
        );
        let dismiss = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "OK" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            dismiss.is_some(),
            "asset error should have a dismissal button"
        );
    }

    #[test]
    fn asset_alert_has_a_padded_rounded_card() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut browser = AssetBrowser {
            operation_error: Some("An item already exists in that folder.".into()),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = || {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(500.0, 300.0),
                    )),
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw();
        first.textures_delta.clear();
        let mut output = draw();
        let text_rect = |label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let title = text_rect("Could not change asset").expect("alert title");
        let ok = text_rect("OK").expect("alert action");
        fn card_shape(
            shape: &egui::epaint::Shape,
            title: egui::Pos2,
            action: egui::Pos2,
        ) -> Option<&egui::epaint::RectShape> {
            match shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.rect.width() < 450.0
                        && rect.rect.contains(title)
                        && rect.rect.contains(action) =>
                {
                    Some(rect)
                }
                egui::epaint::Shape::Vec(shapes) => shapes
                    .iter()
                    .filter_map(|shape| card_shape(shape, title, action))
                    .max_by_key(|rect| rect.fill.a()),
                _ => None,
            }
        }
        let card = output
            .shapes
            .iter()
            .filter_map(|shape| card_shape(&shape.shape, title.center(), ok.center()))
            .max_by_key(|rect| rect.fill.a())
            .expect("alert card");
        assert!(title.left() - card.rect.left() >= 18.0);
        assert!(title.top() - card.rect.top() >= 16.0);
        assert!(card.corner_radius.nw >= 10);
        assert!(card.rect.right() - ok.right() <= 40.0);
        output.textures_delta.clear();
    }

    #[test]
    fn dragging_a_folder_over_another_folder_highlights_and_queues_move() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("source"), PathBuf::from("target")],
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events, browser: &mut AssetBrowser| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, Vec::new(), &mut browser);
        let target = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "target" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("target folder row");
        first.textures_delta.clear();
        egui::DragAndDrop::set_payload(&ctx, FolderDrag(PathBuf::from("source")));
        let mut hovered = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(target),
                pointer_button(target, true),
            ],
            &mut browser,
        );
        let fill = ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .selection
            .bg_fill
            .linear_multiply(0.6);
        assert!(hovered.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => rect.fill == fill && rect.rect.contains(target),
            _ => false,
        }));
        hovered.textures_delta.clear();
        let mut released = draw(0.2, vec![pointer_button(target, false)], &mut browser);
        released.textures_delta.clear();
        assert_eq!(
            browser.operation_request,
            Some(AssetOperation::Relocate {
                from: PathBuf::from("source"),
                to: PathBuf::from("target/source"),
            })
        );
    }

    #[test]
    fn folder_row_starts_a_folder_drag() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("source")],
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, Vec::new());
        let source = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "source" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("source folder row");
        first.textures_delta.clear();
        let mut pressed = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(source),
                pointer_button(source, true),
            ],
        );
        pressed.textures_delta.clear();
        let mut dragged = draw(
            0.2,
            vec![egui::Event::PointerMoved(source + egui::vec2(20.0, 0.0))],
        );
        dragged.textures_delta.clear();
        assert_eq!(
            egui::DragAndDrop::payload::<FolderDrag>(&ctx)
                .expect("folder drag payload")
                .0,
            PathBuf::from("source")
        );
    }

    #[test]
    fn right_clicking_an_asset_opens_rename_and_delete_actions() {
        let ctx = egui::Context::default();
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("sprite.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, Vec::new());
        let file = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "sprite.png" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("asset row");
        first.textures_delta.clear();
        let secondary = |pressed| egui::Event::PointerButton {
            pos: file,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut opening = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(file),
                secondary(true),
                secondary(false),
            ],
        );
        opening.textures_delta.clear();
        let mut menu = draw(0.2, Vec::new());
        let labels: Vec<_> = menu
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        menu.textures_delta.clear();
        assert!(labels.iter().any(|label| label == "Rename"));
        assert!(labels.iter().any(|label| label == "Delete"));
        assert!(labels.iter().any(|label| label == "Copy"));
        assert!(labels.iter().any(|label| label == "Cut"));
        assert!(labels.iter().any(|label| label == "Copy Path"));
        #[cfg(target_os = "macos")]
        assert!(labels.iter().any(|label| label == "Reveal in Finder"));
    }

    #[test]
    fn right_clicking_a_folder_offers_paste_and_file_actions() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("target")],
            clipboard: Some(AssetClipboard {
                path: PathBuf::from("sprite.png"),
                cut: false,
            }),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, Vec::new());
        let folder = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "target" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .expect("target folder");
        first.textures_delta.clear();
        let secondary = |pressed| egui::Event::PointerButton {
            pos: folder,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut opening = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(folder),
                secondary(true),
                secondary(false),
            ],
        );
        opening.textures_delta.clear();
        let mut menu = draw(0.2, Vec::new());
        let labels: Vec<_> = menu
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        menu.textures_delta.clear();
        assert!(labels.iter().any(|label| label == "Paste"));
        assert!(labels.iter().any(|label| label == "Copy"));
        assert!(labels.iter().any(|label| label == "Cut"));
    }

    #[test]
    fn selected_folder_highlight_extends_across_its_disclosure_arrow() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("audio")],
            target_dir: PathBuf::from("audio"),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_scene(ui, &[], &mut browser, &mut selected, &mut open_request);
        });
        let label = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "audio" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .expect("audio label");
        let fill = ctx.style_of(egui::Theme::Dark).visuals.selection.bg_fill;
        let covers_arrow = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => {
                rect.fill == fill
                    && rect
                        .rect
                        .contains(label.left_center() - egui::vec2(36.0, 0.0))
                    && rect.rect.contains(label.center())
            }
            _ => false,
        });
        output.textures_delta.clear();
        assert!(covers_arrow, "selected fill should include the arrow");
    }

    #[test]
    fn copy_and_cut_paste_queue_the_respective_asset_operations() {
        let mut browser = AssetBrowser {
            clipboard: Some(AssetClipboard {
                path: PathBuf::from("source/picture.png"),
                cut: false,
            }),
            ..AssetBrowser::default()
        };
        queue_paste(&mut browser, Path::new("target"));
        assert_eq!(
            browser.operation_request,
            Some(AssetOperation::Copy {
                from: PathBuf::from("source/picture.png"),
                into: PathBuf::from("target"),
            })
        );
        browser.clipboard.as_mut().unwrap().cut = true;
        queue_paste(&mut browser, Path::new("target"));
        assert_eq!(
            browser.operation_request,
            Some(AssetOperation::Relocate {
                from: PathBuf::from("source/picture.png"),
                to: PathBuf::from("target/picture.png"),
            })
        );
        browser.asset_moved(
            Path::new("source/picture.png"),
            Path::new("target/picture.png"),
        );
        assert!(browser.clipboard.is_none(), "cut is consumed after a move");
    }

    #[test]
    fn folder_move_refuses_its_own_descendant_as_a_drop_target() {
        assert_eq!(
            move_destination(Path::new("source"), true, Path::new("source/child")),
            None
        );
        assert_eq!(
            move_destination(Path::new("source/picture.png"), false, Path::new("target")),
            Some(PathBuf::from("target/picture.png"))
        );
    }

    #[test]
    fn rename_field_rejects_a_path_instead_of_a_single_name() {
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            renaming: Some(AssetRename {
                path: PathBuf::from("textures/old.png"),
                draft: "elsewhere/new.png".to_owned(),
                focus: true,
            }),
            ..AssetBrowser::default()
        };
        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            rename_prompt(ui, &mut browser);
        });
        first.textures_delta.clear();
        let mut second = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                rename_prompt(ui, &mut browser);
            },
        );
        second.textures_delta.clear();
        assert!(browser.operation_request.is_none());
    }

    #[test]
    fn selecting_a_file_does_not_highlight_its_import_folder() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut browser = AssetBrowser::default();
        let mut selected = Some(PathBuf::from("player.png"));
        let mut open_request = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request);
        });
        let root_y = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "assets" => {
                    Some(text.pos.y + text.galley.size().y / 2.0)
                }
                _ => None,
            })
            .expect("root label");
        let file_y = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "player.png" => {
                    Some(text.pos.y + text.galley.size().y / 2.0)
                }
                _ => None,
            })
            .expect("file label");
        let selected_color = ctx.style_of(egui::Theme::Dark).visuals.selection.bg_fill;
        let highlighted = |y: f32| {
            output.shapes.iter().any(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect) => {
                    rect.fill == selected_color && rect.rect.top() <= y && y <= rect.rect.bottom()
                }
                _ => false,
            })
        };
        let file_highlighted = highlighted(file_y);
        let root_highlighted = highlighted(root_y);
        output.textures_delta.clear();
        assert!(file_highlighted, "selected file has no highlight");
        assert!(!root_highlighted, "root folder remains highlighted");
    }

    #[test]
    fn hovering_a_folder_highlights_the_arrow_and_label_together() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut browser = AssetBrowser::default();
        let mut selected = Some(PathBuf::from("player.png"));
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 200.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut initial = draw(0.0, vec![]);
        let label = initial.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "assets" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        initial.textures_delta.clear();
        let label = label.expect("root folder label");
        let arrow = egui::pos2(9.0, label.center().y);
        let hover_fill = ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .widgets
            .hovered
            .bg_fill;
        for (time, pointer) in [(0.1, arrow), (0.2, label.center())] {
            let mut output = draw(time, vec![egui::Event::PointerMoved(pointer)]);
            let full_row_highlight = output.shapes.iter().any(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect) => {
                    rect.fill == hover_fill
                        && rect.rect.contains(arrow)
                        && rect.rect.contains(label.center())
                }
                _ => false,
            });
            output.textures_delta.clear();
            assert!(
                full_row_highlight,
                "hover at {pointer:?} misses part of folder row"
            );
        }
    }

    #[test]
    fn context_menu_hover_does_not_highlight_the_folder_behind_it() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut browser = AssetBrowser {
            folders: vec![PathBuf::from("behind_menu")],
            ..AssetBrowser::default()
        };
        let mut selected = Some(PathBuf::from("other.png"));
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, vec![]);
        let text_center = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
        };
        let root = text_center(&first, "assets").expect("root folder");
        let child = text_center(&first, "behind_menu").expect("child folder");
        first.textures_delta.clear();
        let secondary = |pressed| egui::Event::PointerButton {
            pos: root,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut opening = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(root),
                secondary(true),
                secondary(false),
            ],
        );
        opening.textures_delta.clear();
        let mut menu = draw(0.2, vec![]);
        let paste = text_center(&menu, "Paste").expect("root context menu");
        menu.textures_delta.clear();
        assert!(
            (paste.y - child.y).abs() < 12.0,
            "menu does not cover child row"
        );
        let mut hovered = draw(0.3, vec![egui::Event::PointerMoved(paste)]);
        let hover_fill = ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .widgets
            .hovered
            .bg_fill;
        let child_arrow = egui::pos2(child.x - 30.0, child.y);
        let behind_is_hovered = hovered.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => {
                rect.fill == hover_fill && rect.rect.contains(child_arrow)
            }
            _ => false,
        });
        hovered.textures_delta.clear();
        assert!(
            !behind_is_hovered,
            "menu hover leaked to the folder beneath it"
        );
    }

    #[test]
    fn root_context_menu_items_have_room_to_breathe() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 240.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &[], &mut browser, &mut selected, &mut open_request),
            )
        };
        let mut first = draw(0.0, vec![]);
        let text_center = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
        };
        let root = text_center(&first, "assets").expect("root folder");
        first.textures_delta.clear();
        let secondary = |pressed| egui::Event::PointerButton {
            pos: root,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut opening = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(root),
                secondary(true),
                secondary(false),
            ],
        );
        opening.textures_delta.clear();
        let mut menu = draw(0.2, vec![]);
        let paste = text_center(&menu, "Paste").expect("Paste menu item");
        let new_folder = text_center(&menu, "New Folder").expect("New Folder menu item");
        menu.textures_delta.clear();
        assert!(
            new_folder.y - paste.y >= 30.0,
            "context menu rows are too tight"
        );
    }

    #[test]
    fn clicking_a_folder_clears_the_file_selection() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = Some(PathBuf::from("player.png"));
        let mut open_request = None;
        let mut draw = |time, events, selected: &mut Option<PathBuf>| {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, &mut browser, selected, &mut open_request),
            )
        };
        let mut output = draw(0.0, vec![], &mut selected);
        let root = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "assets" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
            }
            _ => None,
        });
        output.textures_delta.clear();
        let root = root.expect("root folder label");
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            let mut output = draw(time, vec![pointer_button(root, pressed)], &mut selected);
            output.textures_delta.clear();
        }
        assert_eq!(selected, None);
    }

    #[test]
    fn scene_tree_shows_files_inside_expanded_folders_without_selecting_them() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("sprites/player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut labels = Vec::new();
        for frame in 0..8 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                time: Some(f64::from(frame) * 0.1),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request);
            });
            if frame == 7 {
                labels = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                        _ => None,
                    })
                    .collect();
            }
            output.textures_delta.clear();
        }
        assert!(
            labels.iter().any(|label| label.contains("player.png")),
            "{labels:?}"
        );
    }

    #[test]
    fn scene_tree_draws_outline_icons_for_folders_and_files() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("sprites/player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request),
        );
        let outlined_icons = output
            .shapes
            .iter()
            .filter(|shape| match &shape.shape {
                egui::epaint::Shape::Path(path) => path.points.len() >= 5,
                _ => false,
            })
            .count();
        output.textures_delta.clear();
        assert!(
            outlined_icons >= 3,
            "expected root, folder, and file outline icons, got {outlined_icons}"
        );
    }

    #[test]
    fn clicking_a_file_in_the_tree_selects_it_without_changing_the_import_folder() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("sprites/player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut file_label = None;
        for frame in 0..8 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    time: Some(f64::from(frame) * 0.1),
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request),
            );
            if frame == 7 {
                file_label = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.text().contains("player.png") =>
                    {
                        Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                    }
                    _ => None,
                });
            }
            output.textures_delta.clear();
        }
        let pointer = file_label.expect("nested file row").center();
        for (time, pressed) in [(0.8, true), (0.9, false)] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    time: Some(time),
                    events: vec![pointer_button(pointer, pressed)],
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request),
            );
            output.textures_delta.clear();
        }
        assert_eq!(
            selected.as_deref(),
            Some(entries[0].relative_path.as_path())
        );
        assert!(browser.target_dir.as_os_str().is_empty());
    }

    #[test]
    fn toggling_the_root_disclosure_preserves_the_import_folder() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("sprites/player.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser {
            target_dir: PathBuf::from("sprites"),
            ..AssetBrowser::default()
        };
        let mut selected = None;
        let mut open_request = None;
        let mut draw =
            |time, events, browser: &mut AssetBrowser, selected: &mut Option<PathBuf>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(240.0, 260.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| show_scene(ui, &entries, browser, selected, &mut open_request),
                )
            };
        let mut output = draw(0.0, vec![], &mut browser, &mut selected);
        let root_label = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text().trim() == "assets" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        output.textures_delta.clear();
        let root_label = root_label.expect("root folder label");
        let arrow = egui::pos2(9.0, root_label.center().y);
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            let mut output = draw(
                time,
                vec![pointer_button(arrow, pressed)],
                &mut browser,
                &mut selected,
            );
            output.textures_delta.clear();
        }
        assert_eq!(browser.target_dir, PathBuf::from("sprites"));
    }

    #[test]
    fn narrow_tree_truncates_file_names_and_reveals_more_when_widened() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("textures/terrain/very_long_file_name.png"),
            kind: AssetKind::Texture,
            id: None,
        }];
        let ctx = egui::Context::default();
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut widths = Vec::new();
        for frame in 0..16 {
            let panel_width = if frame < 8 { 130.0 } else { 420.0 };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(panel_width, 260.0),
                    )),
                    time: Some(f64::from(frame) * 0.1),
                    ..Default::default()
                },
                |ui| show_scene(ui, &entries, &mut browser, &mut selected, &mut open_request),
            );
            if frame == 7 || frame == 15 {
                let label = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text)
                            if text.galley.text().contains("very_long")
                                || text.galley.text().contains("…") =>
                        {
                            Some((
                                text.galley.rows[0].text(),
                                text.galley.size().x,
                                text.galley.size().y,
                            ))
                        }
                        _ => None,
                    })
                    .expect("nested file label");
                assert!(label.2 < 25.0, "file name wrapped: {label:?}");
                assert!(label.1 < panel_width, "file name escaped panel: {label:?}");
                widths.push(label);
            }
            output.textures_delta.clear();
        }
        assert!(
            widths[0].0.contains('…'),
            "narrow label should elide: {:?}",
            widths[0]
        );
        assert!(
            widths[1].0.contains("very_long_file_name.png"),
            "wide label should show full name: {:?}",
            widths[1]
        );
    }

    #[test]
    fn common_asset_extensions_draw_their_file_type_icons() {
        fn icon_shapes(path: &str) -> (usize, usize) {
            let ctx = egui::Context::default();
            let entry = AssetEntry {
                relative_path: PathBuf::from(path),
                kind: AssetKind::from_path(Path::new(path)),
                id: None,
            };
            let mut selected = None;
            let mut open_request = None;
            let mut browser = AssetBrowser::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                asset_row(
                    ui,
                    &entry,
                    RowLabel::Name,
                    &mut browser,
                    &mut selected,
                    &mut open_request,
                );
            });
            let circles = output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::epaint::Shape::Circle(_)))
                .count();
            let paths = output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::epaint::Shape::Path(_)))
                .count();
            output.textures_delta.clear();
            (circles, paths)
        }

        assert!(
            icon_shapes("audio/music.ogg").0 > 0,
            ".ogg needs an audio-note icon"
        );
        assert!(
            icon_shapes("audio/music.flac").0 > 0,
            ".flac needs an audio-note icon"
        );
        assert!(
            icon_shapes("models/tree.obj").1 >= 2,
            ".obj needs a mesh icon"
        );
        assert!(
            icon_shapes("project.toml").0 > 0,
            ".toml needs a settings icon"
        );
        assert!(
            icon_shapes("scenes/main.ron").1 >= 2,
            ".ron needs a scene icon"
        );
    }

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

    fn asset_row_frame(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        entry: &AssetEntry,
        selected: &mut Option<PathBuf>,
        open_request: &mut Option<PathBuf>,
    ) -> (egui::Response, egui::CursorIcon) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 480.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut response = None;
        let mut browser = AssetBrowser::default();
        let mut output = ctx.run_ui(input, |ui| {
            response = Some(asset_row(
                ui,
                entry,
                RowLabel::Name,
                &mut browser,
                selected,
                open_request,
            ));
        });
        output.textures_delta.clear();
        (
            response.expect("asset row was drawn"),
            output.platform_output.cursor_icon,
        )
    }

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn asset_row_click_selects_without_a_grab_cursor() {
        let ctx = egui::Context::default();
        let entry = AssetEntry {
            relative_path: PathBuf::from("scripts/player.rs"),
            kind: AssetKind::from_path(Path::new("scripts/player.rs")),
            id: None,
        };
        let mut selected = None;
        let mut open_request = None;

        let (initial, _) =
            asset_row_frame(&ctx, 0.0, vec![], &entry, &mut selected, &mut open_request);
        let pointer = initial.rect.center();
        let (_, cursor) = asset_row_frame(
            &ctx,
            0.1,
            vec![egui::Event::PointerMoved(pointer)],
            &entry,
            &mut selected,
            &mut open_request,
        );
        assert_ne!(cursor, egui::CursorIcon::Grab);
        assert_ne!(cursor, egui::CursorIcon::Grabbing);

        let _ = asset_row_frame(
            &ctx,
            0.2,
            vec![pointer_button(pointer, true)],
            &entry,
            &mut selected,
            &mut open_request,
        );
        let _ = asset_row_frame(
            &ctx,
            0.3,
            vec![pointer_button(pointer, false)],
            &entry,
            &mut selected,
            &mut open_request,
        );
        assert_eq!(selected.as_deref(), Some(entry.relative_path.as_path()));
    }

    #[test]
    fn asset_row_drag_still_sets_the_asset_payload() {
        let ctx = egui::Context::default();
        let entry = AssetEntry {
            relative_path: PathBuf::from("textures/grass.png"),
            kind: AssetKind::from_path(Path::new("textures/grass.png")),
            id: None,
        };
        let mut selected = None;
        let mut open_request = None;
        let (initial, _) =
            asset_row_frame(&ctx, 0.0, vec![], &entry, &mut selected, &mut open_request);
        let pointer = initial.rect.center();

        let _ = asset_row_frame(
            &ctx,
            0.1,
            vec![
                egui::Event::PointerMoved(pointer),
                pointer_button(pointer, true),
            ],
            &entry,
            &mut selected,
            &mut open_request,
        );
        let _ = asset_row_frame(
            &ctx,
            0.2,
            vec![egui::Event::PointerMoved(pointer + egui::vec2(20.0, 0.0))],
            &entry,
            &mut selected,
            &mut open_request,
        );

        let payload = egui::DragAndDrop::payload::<PathBuf>(&ctx)
            .expect("dragging an asset row should publish its path");
        assert_eq!(payload.as_path(), entry.relative_path.as_path());
    }

    /// Runs the Asset Browser inside the same panel nesting the studio
    /// shell uses (a bottom panel inside the left dock), settling the
    /// layout over several passes, and returns the panel's final height
    /// plus every text row it drew, top to bottom.
    ///
    /// This is the only kind of test that catches a *layout* regression
    /// without a person looking at the window: egui reports where each
    /// widget landed, so "the panel grew until it covered the
    /// Hierarchy" is an assertion, not an eyeball.
    fn run_browser_layout(entries: &[AssetEntry], dock_height: f32) -> (f32, Vec<egui::Rect>) {
        const PANEL_DEFAULT: f32 = 240.0;

        let ctx = egui::Context::default();
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, dock_height),
            )),
            ..Default::default()
        };
        let mut browser = AssetBrowser::default();
        let mut selected = None;
        let mut open_request = None;
        let mut panel_height = 0.0;
        let mut rows = Vec::new();

        // Several passes: egui settles panel sizes over frames, and the
        // collapse animation needs the clock to advance.
        for pass in 0..12 {
            input.time = Some(f64::from(pass) * 0.1);
            let mut output = ctx.run_ui(input.clone(), |ui| {
                let mut left_open = true;
                egui::Panel::left("studio_left_dock")
                    .resizable(true)
                    .default_size(240.0)
                    .min_size(170.0)
                    .show_collapsible(ui, &mut left_open, |ui| {
                        let mut browser_open = true;
                        let shown = egui::Panel::bottom("studio_asset_panel")
                            .resizable(true)
                            // Order matters: `default_size` widens the
                            // size range, so the caps come after it.
                            .default_size(PANEL_DEFAULT)
                            .min_size(BROWSER_MIN_HEIGHT)
                            .max_size(browser_panel_max(ui.available_height()))
                            .show_collapsible(ui, &mut browser_open, |ui| {
                                ui.heading("Asset Browser");
                                ui.separator();
                                show(
                                    ui,
                                    entries,
                                    ImportStats::default(),
                                    &mut browser,
                                    &mut selected,
                                    &mut open_request,
                                );
                            });
                        if let Some(shown) = shown {
                            panel_height = shown.response.rect.height();
                        }
                        ui.heading("Hierarchy");
                    });
            });
            // The test never uploads anything to a GPU, but epaint
            // insists its font-atlas delta be acknowledged before drop.
            output.textures_delta.clear();

            if pass + 1 == 12 {
                rows = output
                    .shapes
                    .iter()
                    .filter_map(|clipped| match &clipped.shape {
                        // Only what the viewer can actually see: a row
                        // scrolled out of the panel is still a shape,
                        // but it is clipped away before it is drawn.
                        egui::epaint::Shape::Text(text) => {
                            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                            clipped.clip_rect.intersects(rect).then_some(rect)
                        }
                        _ => None,
                    })
                    .collect();
                rows.sort_by(|a, b| a.min.y.total_cmp(&b.min.y));
            }
        }
        (panel_height, rows)
    }

    fn asset_entries(count: usize) -> Vec<AssetEntry> {
        let mut entries: Vec<AssetEntry> = (0..count)
            .map(|index| {
                let folder = ["models", "textures/terrain", "textures/props", "audio"][index % 4];
                let name = format!("{folder}/asset_{index:03}.png");
                AssetEntry {
                    relative_path: PathBuf::from(&name),
                    kind: AssetKind::from_path(Path::new(&name)),
                    id: None,
                }
            })
            .collect();
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        entries
    }

    #[test]
    fn a_long_file_list_scrolls_instead_of_growing_the_panel() {
        // The bug this guards: with no scroll area the list's height
        // *was* the panel's height, egui persisted it, and 22 files were
        // enough to grow the browser from 240px to 584px — swallowing
        // the dock and drawing over the Hierarchy.
        let (panel_height, _) = run_browser_layout(&asset_entries(300), 800.0);
        assert!(
            panel_height <= 241.0,
            "panel grew to {panel_height}px instead of staying at its 240px size"
        );
    }

    #[test]
    fn an_empty_browser_still_holds_its_panel_size() {
        let (panel_height, _) = run_browser_layout(&[], 800.0);
        assert!(
            (panel_height - 240.0).abs() <= 1.0,
            "empty browser sized the panel to {panel_height}px, not 240px"
        );
    }

    #[test]
    fn no_two_rows_are_drawn_on_top_of_each_other() {
        let (_, rows) = run_browser_layout(&asset_entries(300), 800.0);
        assert!(rows.len() > 5, "expected the browser to draw rows");
        // Two labels may share a y (the view switcher's buttons sit side
        // by side) or an x (indented rows down the tree). Only sharing
        // both is one label drawn over another.
        for (index, first) in rows.iter().enumerate() {
            for second in &rows[index + 1..] {
                if second.min.y >= first.max.y {
                    break;
                }
                let overlap = first.intersect(*second);
                assert!(
                    overlap.width() <= 0.5 || overlap.height() <= 0.5,
                    "two labels are drawn on top of each other: {first:?} and {second:?}"
                );
            }
        }
    }

    #[test]
    fn file_rows_never_overlap_in_a_squeezed_dock() {
        for dock_height in [240.0, 320.0, 480.0, 800.0] {
            let (_, rows) = run_browser_layout(&asset_entries(60), dock_height);
            for (index, first) in rows.iter().enumerate() {
                for second in &rows[index + 1..] {
                    if second.min.y >= first.max.y {
                        break;
                    }
                    let overlap = first.intersect(*second);
                    assert!(
                        overlap.width() <= 0.5 || overlap.height() <= 0.5,
                        "at dock height {dock_height}, two labels overlap: \
                         {first:?} and {second:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn build_tree_mirrors_the_directory_layout() {
        let entries = vec![
            AssetEntry {
                relative_path: PathBuf::from("models/rock.gltf"),
                kind: AssetKind::Mesh,
                id: None,
            },
            AssetEntry {
                relative_path: PathBuf::from("textures/terrain/grass.png"),
                kind: AssetKind::Texture,
                id: None,
            },
            AssetEntry {
                relative_path: PathBuf::from("textures/props/bark.png"),
                kind: AssetKind::Texture,
                id: None,
            },
            AssetEntry {
                relative_path: PathBuf::from("readme.md"),
                kind: AssetKind::Other,
                id: None,
            },
        ];
        let root = build_tree(&entries);

        assert_eq!(root.files, vec![3], "loose files stay at the root");
        assert_eq!(root.folders.len(), 2);
        assert_eq!(root.folders[0].name, "models");
        assert_eq!(root.folders[0].files, vec![0]);

        let textures = &root.folders[1];
        assert_eq!(textures.name, "textures");
        assert!(textures.files.is_empty(), "no files sit directly in it");
        assert_eq!(textures.folders.len(), 2);
        assert_eq!(textures.folders[0].path, PathBuf::from("textures/terrain"));
        assert_eq!(textures.file_count(), 2);
        assert_eq!(root.file_count(), entries.len());
    }

    #[test]
    fn ensure_makes_a_folder_visible_before_it_holds_anything() {
        let entries = vec![AssetEntry {
            relative_path: PathBuf::from("models/rock.gltf"),
            kind: AssetKind::Mesh,
            id: None,
        }];
        let mut root = build_tree(&entries);
        assert_eq!(root.folders.len(), 1);

        root.ensure(Path::new("textures/terrain"));
        assert_eq!(root.folders.len(), 2, "an empty folder is still a folder");
        let textures = root
            .folders
            .iter()
            .find(|f| f.name == "textures")
            .expect("created");
        assert_eq!(textures.folders[0].path, PathBuf::from("textures/terrain"));
        assert_eq!(textures.file_count(), 0);

        // Idempotent: ensuring twice does not double the tree.
        root.ensure(Path::new("textures/terrain"));
        assert_eq!(root.folders.len(), 2);
    }

    #[test]
    fn scan_hides_meta_sidecars_and_hidden_files() {
        let dir = temp_dir("meta-hidden");
        std::fs::write(dir.join("brick.png"), b"pixels").unwrap();
        std::fs::write(dir.join("brick.png.meta"), b"()").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(dir.join(".cache")).unwrap();
        std::fs::write(dir.join(".cache").join("stale.png"), b"pixels").unwrap();

        let entries = scan(&dir).unwrap();
        let paths: Vec<_> = entries.iter().map(|e| e.relative_path.clone()).collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("brick.png")],
            "the importer's own bookkeeping is not a project asset"
        );
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

    #[test]
    fn relocating_a_folder_repoints_all_indexed_descendants() {
        let mut index = AssetIndex::new();
        let id = AssetId::new();
        index.insert(id, PathBuf::from("source/nested/picture.png"));

        index.relocate_paths(Path::new("source"), Path::new("target/source"));

        assert_eq!(
            index.path_for(id),
            Some(Path::new("target/source/nested/picture.png"))
        );
        assert_eq!(index.id_for(Path::new("source/nested/picture.png")), None);
    }
}
