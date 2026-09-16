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
//! Lists whatever's on disk — not yet wired to `engine_asset`'s
//! [`AssetDatabase`](engine_asset::AssetDatabase) (which tracks
//! *registered* assets by UUID, not files/paths at all) or to the
//! importers that would actually load a selected file. That wiring —
//! selecting a file here registers/imports it — is future work.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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
        // Hidden files and directories are the tool's own bookkeeping
        // (`.DS_Store`, an editor's dotfolder), not project assets.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if file_type.is_dir() {
            scan_into(root, &path, depth + 1, entries)?;
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
    /// The half-typed name of a new folder while the prompt is open;
    /// `None` when it is closed.
    pub new_folder: Option<String>,
    /// A folder the host should create, relative to the assets root.
    /// Taken by the host, which validates it before touching the disk.
    pub new_folder_request: Option<PathBuf>,
    /// Whether the selected-asset preview strip is visible.
    pub preview_open: bool,
}

impl Default for AssetBrowser {
    fn default() -> Self {
        Self {
            view: AssetView::default(),
            target_dir: PathBuf::new(),
            new_folder: None,
            new_folder_request: None,
            preview_open: true,
        }
    }
}

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
pub fn show(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
    stats: ImportStats,
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    ui.horizontal(|ui| {
        for option in [AssetView::Folders, AssetView::Kinds] {
            ui.selectable_value(&mut browser.view, option, option.label());
        }
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

    // Both bounds are load-bearing. `max_height` from what the cursor
    // says is left, because a non-shrinking `ScrollArea` otherwise sizes
    // itself from the Ui's `max_rect`; and `min_scrolled_height(0)`
    // because egui's default floor is 64pt, which a squeezed browser
    // does not have — the list would keep that 64pt by drawing over the
    // preview strip below it.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(ui.available_height().max(0.0))
        .min_scrolled_height(0.0)
        .show(ui, |ui| {
            if entries.is_empty() && browser.target_dir.as_os_str().is_empty() {
                ui.label("No assets found.");
                return;
            }
            match browser.view {
                AssetView::Folders => {
                    let mut root = build_tree(entries);
                    root.ensure(&browser.target_dir);
                    show_folder_contents(ui, &root, entries, browser, selected, open_request);
                }
                AssetView::Kinds => show_kinds(ui, entries, selected, open_request),
            }
        });
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

/// Smallest the preview strip is allowed to get. Below this it shows
/// nothing useful, and the drag handle becomes hard to grab.
pub const PREVIEW_MIN_HEIGHT: f32 = 50.0;

/// The preview strip's `(max, default)` height for a browser panel with
/// `available` points left under its heading.
///
/// The preview is a *strip*, not a second panel: it may never take so
/// much of the browser that the file list has nowhere to go. A fixed
/// default (it was 210) does exactly that in a 240pt panel — the list
/// then draws over the preview, which is what a person sees as the rows
/// stacking on top of each other.
pub fn preview_strip_size(available: f32) -> (f32, f32) {
    // Half, so the list keeps at least as much room as the preview, and
    // never less than the minimum even in a panel dragged tiny.
    let max = (available * 0.5).max(PREVIEW_MIN_HEIGHT);
    (max, max.min(180.0))
}

/// The row above the tree: which folder is the target for an import or a
/// new folder, and the button that makes one.
///
/// A project keeps its textures in `textures/` and its audio in `audio/`
/// because someone put them there. Without this the browser can only
/// ever show one flat heap — every import lands in the assets root.
fn target_folder_bar(ui: &mut egui::Ui, browser: &mut AssetBrowser) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("\u{2795} Folder")
                .on_hover_text("New folder here")
                .clicked()
            {
                browser.new_folder = Some(String::new());
            }
        });
    });

    let Some(name) = browser.new_folder.as_mut() else {
        return;
    };
    let response = ui.add(
        egui::TextEdit::singleline(name)
            .hint_text("folder name")
            .desired_width(f32::INFINITY),
    );
    // Focus it once, when it opens — re-requesting every frame would
    // fight the user for the pointer.
    if !response.has_focus() && name.is_empty() {
        response.request_focus();
    }
    if response.lost_focus() {
        let typed = std::mem::take(name);
        let typed = typed.trim();
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !typed.is_empty() {
            browser.new_folder_request = Some(browser.target_dir.join(typed));
        }
        browser.new_folder = None;
    }
}

/// Draws one directory's subfolders and files. Recurses through
/// [`egui::CollapsingHeader`], so a collapsed folder costs one row
/// whatever it holds.
fn show_folder_contents(
    ui: &mut egui::Ui,
    folder: &AssetFolder,
    entries: &[AssetEntry],
    browser: &mut AssetBrowser,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) {
    for child in &folder.folders {
        let is_target = browser.target_dir == child.path;
        let label =
            egui::RichText::new(format!("\u{1f4c1} {}  {}", child.name, child.file_count()));
        let label = if is_target { label.strong() } else { label };
        let header = egui::CollapsingHeader::new(label)
            .id_salt(&child.path)
            .default_open(true)
            .show(ui, |ui| {
                show_folder_contents(ui, child, entries, browser, selected, open_request);
            });
        // Clicking the folder's name aims the import/new-folder target
        // at it; the triangle still just opens and closes it.
        if header.header_response.clicked() {
            browser.target_dir = child.path.clone();
        }
    }
    for &index in &folder.files {
        if let Some(entry) = entries.get(index) {
            let _ = asset_row(ui, entry, RowLabel::Name, selected, open_request);
        }
    }
}

/// Draws the by-kind grouping: one collapsible section per
/// [`AssetKind`] present, every file of that kind in it wherever it
/// lives on disk.
fn show_kinds(
    ui: &mut egui::Ui,
    entries: &[AssetEntry],
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
                    let _ = asset_row(ui, entry, RowLabel::Path, selected, open_request);
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
    Path,
}

/// One file row: selectable, draggable onto the Scene View, and — for a
/// text file — openable in the code editor by double-click.
fn asset_row(
    ui: &mut egui::Ui,
    entry: &AssetEntry,
    label: RowLabel,
    selected: &mut Option<PathBuf>,
    open_request: &mut Option<PathBuf>,
) -> egui::Response {
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
    let response =
        ui.add(egui::Button::selectable(is_selected, text).sense(egui::Sense::click_and_drag()));
    response.dnd_set_drag_payload(entry.relative_path.clone());
    if response.clicked() {
        *selected = Some(entry.relative_path.clone());
    }
    if response.double_clicked() && is_text_file(&entry.relative_path) {
        *open_request = Some(entry.relative_path.clone());
    }
    response
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
        let mut output = ctx.run_ui(input, |ui| {
            response = Some(asset_row(ui, entry, RowLabel::Name, selected, open_request));
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
                                // Exactly the shell's nesting: the
                                // preview claims a strip at the bottom
                                // before the list is drawn.
                                let (preview_max, preview_default) =
                                    preview_strip_size(ui.available_height());
                                egui::Panel::bottom("studio_asset_preview")
                                    .resizable(true)
                                    .default_size(preview_default)
                                    .min_size(PREVIEW_MIN_HEIGHT)
                                    .max_size(preview_max)
                                    .show(ui, |ui| {
                                        egui::ScrollArea::vertical()
                                            .auto_shrink([false, false])
                                            .min_scrolled_height(0.0)
                                            .show(ui, |ui| {
                                                ui.separator();
                                                ui.strong("Preview");
                                                ui.weak("Select an asset.");
                                            });
                                    });
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
    fn the_browser_and_its_preview_never_overlap_in_a_squeezed_dock() {
        // The preview used to claim a fixed 210pt strip, which in a
        // 240pt panel left the file list no room and put the rows on top
        // of the preview. Checked here at the sizes where that bites.
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
    fn the_preview_never_takes_more_than_half_the_browser() {
        let (max, default) = preview_strip_size(400.0);
        assert_eq!(max, 200.0);
        assert_eq!(default, 180.0, "capped by taste, not just by space");

        // In a panel too small to halve, it still keeps a usable strip
        // and the caller's `min_size` stops it going lower.
        let (max, default) = preview_strip_size(60.0);
        assert_eq!(max, PREVIEW_MIN_HEIGHT);
        assert_eq!(default, PREVIEW_MIN_HEIGHT);
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
}
