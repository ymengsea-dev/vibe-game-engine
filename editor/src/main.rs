//! RustyEngine Studio: a standalone window running the egui-based studio
//! shell on top of the engine's own renderer/platform layers.
//!
//! Opens a project — the path given as the first CLI argument, or a
//! default dev project created in the working directory (`RustyEngine
//! Project`, git-ignored) on first run. Assets, the scene, and the
//! editor session (`.studio/session.ron`: last workspace, panel layout,
//! open scene) all resolve against that project.
//!
//! The shell draws a menu bar, a toolbar (play/pause/stop, workspace
//! switcher, gizmo mode, transform space, build config), a status bar,
//! and the dockable panels: Hierarchy, Inspector/Lighting, Asset
//! Browser, Code (a Monaco webview via `editor_code`, floated over the
//! panel), AI Assistant (placeholder), Console/Problems/Output,
//! Profiler, and the central Scene panel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_code::{CodeEditor, CodeEvent, LspKind, Marker, MarkerSeverity, PanelRect};
use editor_lsp::{
    CompletionOut, DefinitionOut, FileDiagnostics, HoverOut, LspClient, LspDiagnostic, LspPayload,
    LspResponse, ReferencesOut, RenameOut, SignatureHelpOut, SymbolOut, TextEditOut,
    WorkspaceSymbolsOut,
};
use engine::ecs::components as ecs_components;
use engine::ecs::components::Name;
use engine::ecs::prelude::{ChildOf, World};
use engine::editor::{Diagnostic, MAX_TEXT_HITS, Severity as ProblemSeverity, SymbolHit};
use engine::prelude::*;
use engine::project::MANIFEST_FILE;

/// Default project directory when the editor is launched with no path
/// argument — created on first run, reopened afterwards. Relative to the
/// working directory; git-ignored.
const DEFAULT_PROJECT_DIR: &str = "RustyEngineProject";

/// Starter `src/main.rs` written into a freshly created project.
const STARTER_MAIN: &str = "//! Entry point for your RustyEngine game.\n\nfn main() {\n    println!(\"Hello from RustyEngine Studio.\");\n}\n";

/// Starter `src/lib.rs` written into a freshly created project.
const STARTER_LIB: &str = "//! Game components and systems.\n\n/// Marker component for the entity the player controls.\npub struct Player;\n";

/// A minimal `Cargo.toml` for a fresh project, so rust-analyzer has a
/// package to load. `project_name` is sanitised into a valid crate name.
fn starter_cargo_toml(project_name: &str) -> String {
    let mut crate_name: String = project_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if crate_name.is_empty() || crate_name.starts_with(|c: char| c.is_ascii_digit()) {
        crate_name.insert(0, '_');
    }
    format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n"
    )
}

/// Writes `contents` to `path` unless a file is already there. Best
/// effort — a failure is logged, not propagated (a missing starter file
/// only means an empty code editor on first run).
fn seed_file(path: &std::path::Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(err) = std::fs::write(path, contents) {
        tracing::warn!(error = %err, path = %path.display(), "could not seed starter file");
    }
}

/// Builds the starting scene: a "Parent Cube" with one child "Child
/// Cube", plus an independent "Light" root — enough to exercise multiple
/// roots, a parent/child pair, and a leaf with no children. Each has a
/// `Transform`, so the inspector panel has translation/rotation/scale to
/// edit and the Scene View has a position to draw each at. From here,
/// the hierarchy panel's "+ Cube"/"Delete" buttons add to or shrink this
/// same world live.
fn placeholder_world() -> World {
    let mut world = World::new();
    let parent = world
        .spawn((
            Name::new("Parent Cube"),
            ecs_components::Transform::default(),
        ))
        .id();
    world.spawn((
        Name::new("Child Cube"),
        ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
            1.5, 0.0, 0.0,
        ))),
        ChildOf(parent),
    ));
    world.spawn((
        Name::new("Light"),
        ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
            0.0, 3.0, 0.0,
        ))),
    ));
    world
}

/// Text-file extensions the code editor's Files tree lists from `src/`.
const CODE_TREE_EXTENSIONS: [&str; 6] = ["rs", "toml", "ron", "wgsl", "glsl", "md"];

/// How many not-yet-open files a find-references result may pull into the
/// editor so Monaco's peek view can render them. A large result on a
/// common symbol shouldn't open hundreds of models.
const REFERENCE_PREOPEN_LIMIT: usize = 25;

/// Lists the source files under `root`, as paths relative to it, sorted.
/// Non-existent `root` yields an empty list.
fn src_file_list(root: &std::path::Path) -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, depth: u32, out: &mut Vec<PathBuf>) {
        if depth > 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, depth + 1, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| CODE_TREE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
                && let Ok(rel) = path.strip_prefix(base)
            {
                out.push(rel.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out.sort();
    out
}

/// Positions the Monaco webview over the Code panel (scaling the panel's
/// logical-point rect to physical pixels) and forwards its events into
/// editor state (`DirtyState`, cursor, file writes). On [`CodeEvent::Ready`]
/// it pushes the `src/` file tree and opens `pending_open` (the starting
/// file); [`CodeEvent::OpenRequested`] and the Asset Browser's
/// `file_open_request` both resolve to a path and open it.
#[allow(clippy::too_many_arguments)]
fn sync_code_editor(
    code: Option<&CodeEditor>,
    state: &mut EditorState,
    pending_open: &mut Option<PathBuf>,
    lsp: &mut Option<LspClient>,
    lsp_waiters: &mut HashMap<i64, u64>,
    lsp_symbol_request: &mut Option<i64>,
    lsp_script_request: &mut Option<(i64, String)>,
    open_docs: &mut HashSet<PathBuf>,
    scale: f64,
) {
    let Some(code) = code else {
        state.file_open_request = None;
        return;
    };

    // Hidden while a drag-resize is in progress so the native webview
    // doesn't lag/overhang the panel edge.
    let show_webview = state.code_panel_visible && !state.code_panel_resizing;
    code.set_visible(show_webview);
    if std::mem::take(&mut state.code_save_requested) {
        code.request_save();
    }
    if show_webview && let Some([x, y, w, h]) = state.code_panel_rect {
        code.set_bounds_px(PanelRect {
            x: (f64::from(x) * scale).round() as i32,
            y: (f64::from(y) * scale).round() as i32,
            width: (f64::from(w) * scale).round().max(1.0) as u32,
            height: (f64::from(h) * scale).round().max(1.0) as u32,
        });
    }

    for event in code.poll() {
        match event {
            CodeEvent::BufferEdited { path, text } => {
                state.dirty.mark_document(&path);
                if let Some(lsp) = lsp.as_mut() {
                    lsp.did_change(&path, &text);
                }
            }
            CodeEvent::CursorMoved { line, column, .. } => {
                state.cursor_line = line;
                state.cursor_column = column;
            }
            CodeEvent::SaveRequested { path, contents } => match std::fs::write(&path, contents) {
                Ok(()) => {
                    state.dirty.clear_document(&path);
                    code.notify_saved(&path);
                    tracing::info!(path = %path.display(), "file saved");
                }
                Err(err) => {
                    tracing::warn!(error = %err, path = %path.display(), "file save failed");
                }
            },
            CodeEvent::OpenRequested { path } => {
                let absolute = state.project.src_dir().join(&path);
                open_in_code_editor(code, lsp.as_mut(), open_docs, &absolute);
            }
            CodeEvent::Ready => {
                let src = state.project.src_dir();
                code.set_file_tree("src", &src, &src_file_list(&src));
                if let Some(rel) = pending_open.take() {
                    open_in_code_editor(code, lsp.as_mut(), open_docs, &src.join(rel));
                }
            }
            CodeEvent::TabClosed { path } => {
                open_docs.remove(&path);
                if let Some(lsp) = lsp.as_mut() {
                    lsp.did_close(&path);
                }
                state.dirty.clear_document(&path);
                tracing::debug!(path = %path.display(), "code editor tab closed");
            }
            CodeEvent::LspRequest {
                nonce,
                kind,
                path,
                line,
                col,
                new_name,
            } => {
                if let Some(lsp) = lsp.as_mut() {
                    // `path` is tree-root-relative, or already absolute
                    // for a file opened from the Asset Browser — `join`
                    // with an absolute path returns it unchanged.
                    let absolute = state.project.src_dir().join(&path);
                    let request_id = match kind {
                        LspKind::Completion => lsp.request_completion(&absolute, line, col),
                        LspKind::Hover => lsp.request_hover(&absolute, line, col),
                        LspKind::Definition => lsp.request_definition(&absolute, line, col),
                        LspKind::References => lsp.request_references(&absolute, line, col, true),
                        LspKind::SignatureHelp => lsp.request_signature_help(&absolute, line, col),
                        LspKind::Rename => lsp.request_rename(
                            &absolute,
                            line,
                            col,
                            new_name.as_deref().unwrap_or_default(),
                        ),
                    };
                    lsp_waiters.insert(request_id, nonce);
                }
            }
        }
    }

    if let Some(relative) = state.file_open_request.take() {
        let absolute = state.project.assets_dir().join(&relative);
        open_in_code_editor(code, lsp.as_mut(), open_docs, &absolute);
    }

    // A clicked Problems row: open the file (root-relative) and move the
    // caret to the reported line.
    if let Some(diagnostic) = state.problem_jump.take()
        && let Some(relative) = diagnostic.file
    {
        let absolute = state.project.root().join(&relative);
        if !open_docs.contains(&absolute) {
            open_in_code_editor(code, lsp.as_mut(), open_docs, &absolute);
        }
        code.reveal(&absolute, diagnostic.line.unwrap_or(1), 1);
    }

    // Search tab: run a text search, fire a symbol query, or jump to a
    // clicked result.
    if std::mem::take(&mut state.search.run_text_requested) {
        let root = state.project.root().to_path_buf();
        state.search.text_results =
            engine::editor::run_text_search(&root, &state.search.query, MAX_TEXT_HITS);
    }
    if std::mem::take(&mut state.search.symbol_query_dirty) {
        let query = state.search.query.text.clone();
        if let Some(lsp) = lsp.as_mut()
            && !query.is_empty()
        {
            *lsp_symbol_request = Some(lsp.request_workspace_symbols(&query));
        } else {
            state.search.symbol_results.clear();
        }
    }
    if let Some(jump) = state.search.jump.take() {
        let absolute = state.project.root().join(&jump.path);
        if !open_docs.contains(&absolute) {
            open_in_code_editor(code, lsp.as_mut(), open_docs, &absolute);
        }
        code.reveal(&absolute, jump.line.max(1), jump.col.max(1));
    }

    // Inspector "Open Script": resolve the component type's declaration
    // via rust-analyzer's workspace symbols.
    if let Some(type_name) = state.open_script_request.take() {
        if let Some(lsp) = lsp.as_mut() {
            let id = lsp.request_workspace_symbols(&type_name);
            *lsp_script_request = Some((id, type_name));
        } else {
            tracing::info!(type_name = %type_name, "open script needs rust-analyzer");
        }
    }
}

/// Drains a pending hierarchy "Make Prefab" request: write the new
/// `.prefab` file under `assets/prefabs/`, then rescan + re-import the
/// asset list so the file shows in the browser, and select it. Failures
/// are logged, not fatal.
fn handle_prefab_request(state: &mut EditorState) {
    let Some(entity) = state.create_prefab_request.take() else {
        return;
    };
    let assets_dir = state.project.assets_dir();
    match engine::editor::write_prefab(&state.world, entity, &assets_dir) {
        Ok(path) => {
            match scan_assets(&assets_dir) {
                Ok(assets) => state.assets = assets,
                Err(err) => {
                    tracing::warn!(error = %err, "asset rescan after prefab save failed");
                }
            }
            state.asset_index = state.importer.run(&assets_dir, &mut state.assets);
            if let Ok(relative) = path.strip_prefix(&assets_dir) {
                state.selected_asset = Some(relative.to_path_buf());
            }
            tracing::info!(path = %path.display(), "saved prefab");
        }
        Err(err) => tracing::warn!(error = %err, "could not save prefab"),
    }
}

/// Whether `path` is an `engine_asset` `.meta` sidecar — the file the
/// importer itself writes. Changes to these are filtered out of the
/// hot-reload trigger so a `.meta` write can't drive another rescan.
fn is_meta_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("meta"))
}

/// Reacts to asset source-file changes reported by the watcher: rescans
/// the assets directory and re-runs the import pass (cache-gated, so
/// unchanged files aren't re-decoded and vanished ones are evicted).
/// Clears the asset selection if its file is gone. Failures are logged,
/// not fatal.
fn handle_asset_changes(state: &mut EditorState, watcher: Option<&AssetWatcher>) {
    let Some(watcher) = watcher else {
        return;
    };
    let changed = watcher.poll_changes();
    if changed.iter().all(|path| is_meta_path(path)) {
        // Nothing but sidecar writes (or nothing at all) — ignore.
        return;
    }

    let assets_dir = state.project.assets_dir();
    match scan_assets(&assets_dir) {
        Ok(assets) => state.assets = assets,
        Err(err) => {
            tracing::warn!(error = %err, "asset rescan after a file change failed");
            return;
        }
    }
    state.asset_index = state.importer.run(&assets_dir, &mut state.assets);

    if let Some(selected) = &state.selected_asset
        && !state
            .assets
            .iter()
            .any(|entry| &entry.relative_path == selected)
    {
        state.selected_asset = None;
    }

    let stats = state.importer.stats();
    tracing::info!(
        changed = changed.len(),
        imported = stats.imported,
        cached = stats.cached,
        failed = stats.failed,
        "re-imported changed assets"
    );
}

/// Picks the best `workspace/symbol` match for an "Open Script" jump: a
/// `struct` / `enum` / `trait` named exactly `type_name`, preferring one
/// inside the project (`root`) over a dependency's source.
fn pick_script_symbol<'a>(
    symbols: &'a [SymbolOut],
    type_name: &str,
    root: &Path,
) -> Option<&'a SymbolOut> {
    // LSP `SymbolKind`: struct = 23, enum = 10, trait = 11.
    const DECL_KINDS: [u8; 3] = [23, 10, 11];
    let is_decl =
        |symbol: &SymbolOut| symbol.name == type_name && DECL_KINDS.contains(&symbol.kind);
    symbols
        .iter()
        .find(|symbol| is_decl(symbol) && symbol.path.starts_with(root))
        .or_else(|| symbols.iter().find(|symbol| is_decl(symbol)))
}

/// Converts `editor_lsp` workspace symbols (absolute path, 0-based) into
/// the Search panel's [`SymbolHit`] (project-root-relative, 1-based).
fn to_symbol_hits(result: &WorkspaceSymbolsOut, root: &Path) -> Vec<SymbolHit> {
    result
        .symbols
        .iter()
        .map(|symbol| SymbolHit {
            name: symbol.name.clone(),
            kind: symbol.kind,
            container: symbol.container.clone(),
            path: symbol
                .path
                .strip_prefix(root)
                .unwrap_or(&symbol.path)
                .to_path_buf(),
            line: symbol.line + 1,
            col: symbol.col + 1,
        })
        .collect()
}

/// Reads `absolute`, hands it to the Monaco webview, and — for a Rust
/// file — tells rust-analyzer it's open. Records it in `open_docs` so a
/// later go-to-definition into the same file doesn't reopen (and reset)
/// it. Logs either way.
fn open_in_code_editor(
    code: &CodeEditor,
    lsp: Option<&mut LspClient>,
    open_docs: &mut HashSet<PathBuf>,
    absolute: &Path,
) {
    match std::fs::read_to_string(absolute) {
        Ok(contents) => {
            code.open_file(absolute, &contents);
            if let Some(lsp) = lsp
                && absolute.extension().is_some_and(|ext| ext == "rs")
            {
                lsp.did_open(absolute, &contents);
            }
            open_docs.insert(absolute.to_path_buf());
            // Hand keyboard focus to the webview so the user can type
            // right away instead of clicking into it first.
            code.focus();
            tracing::info!(path = %absolute.display(), "opened in code editor");
        }
        Err(err) => {
            tracing::warn!(error = %err, path = %absolute.display(), "could not open file");
        }
    }
}

/// Neutral JSON for Monaco's completion provider (see `index.html`).
fn completion_json(result: &CompletionOut) -> String {
    let items: Vec<serde_json::Value> = result
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "label": item.label,
                "kind": item.kind,
                "detail": item.detail,
                "documentation": item.documentation,
                "insertText": item.insert_text,
                "isSnippet": item.is_snippet,
                "sortText": item.sort_text,
            })
        })
        .collect();
    serde_json::json!({ "items": items }).to_string()
}

/// Neutral JSON for Monaco's hover provider. Positions go 0-based →
/// 1-based here.
fn hover_json(result: &HoverOut) -> String {
    let range = result.range.map(|r| {
        serde_json::json!({
            "startLineNumber": r.start_line + 1,
            "startColumn": r.start_col + 1,
            "endLineNumber": r.end_line + 1,
            "endColumn": r.end_col + 1,
        })
    });
    serde_json::json!({ "markdown": result.markdown, "range": range }).to_string()
}

/// Neutral JSON for Monaco's definition provider. `path` is absolute (the
/// Monaco model key); positions go 0-based → 1-based here.
fn definition_json(result: &DefinitionOut) -> String {
    let targets: Vec<serde_json::Value> = result
        .targets
        .iter()
        .map(|target| {
            serde_json::json!({
                "path": target.path.to_string_lossy(),
                "startLineNumber": target.start_line + 1,
                "startColumn": target.start_col + 1,
                "endLineNumber": target.end_line + 1,
                "endColumn": target.end_col + 1,
            })
        })
        .collect();
    serde_json::json!({ "targets": targets }).to_string()
}

/// Neutral JSON for Monaco's reference provider. `path` is absolute;
/// positions go 0-based → 1-based here.
fn references_json(result: &ReferencesOut) -> String {
    let locations: Vec<serde_json::Value> = result
        .locations
        .iter()
        .map(|location| {
            serde_json::json!({
                "path": location.path.to_string_lossy(),
                "startLineNumber": location.start_line + 1,
                "startColumn": location.start_col + 1,
                "endLineNumber": location.end_line + 1,
                "endColumn": location.end_col + 1,
            })
        })
        .collect();
    serde_json::json!({ "locations": locations }).to_string()
}

/// Neutral JSON for Monaco's signature-help provider. No position
/// conversion — signature help has no ranges.
fn signature_help_json(result: &SignatureHelpOut) -> String {
    let signatures: Vec<serde_json::Value> = result
        .signatures
        .iter()
        .map(|signature| {
            let parameters: Vec<serde_json::Value> = signature
                .parameters
                .iter()
                .map(|parameter| {
                    serde_json::json!({
                        "label": parameter.label,
                        "documentation": parameter.documentation,
                    })
                })
                .collect();
            serde_json::json!({
                "label": signature.label,
                "documentation": signature.documentation,
                "parameters": parameters,
            })
        })
        .collect();
    serde_json::json!({
        "signatures": signatures,
        "activeSignature": result.active_signature,
        "activeParameter": result.active_parameter,
    })
    .to_string()
}

/// Neutral JSON for Monaco's rename provider — only the files Monaco has
/// an open model for (the host writes the rest to disk itself).
/// Positions go 0-based → 1-based here.
fn rename_json(open_edits: &[&editor_lsp::FileEdits]) -> String {
    let file_edits: Vec<serde_json::Value> = open_edits
        .iter()
        .map(|file| {
            let edits: Vec<serde_json::Value> = file
                .edits
                .iter()
                .map(|edit| {
                    serde_json::json!({
                        "startLineNumber": edit.range.start_line + 1,
                        "startColumn": edit.range.start_col + 1,
                        "endLineNumber": edit.range.end_line + 1,
                        "endColumn": edit.range.end_col + 1,
                        "text": edit.new_text,
                    })
                })
                .collect();
            serde_json::json!({
                "path": file.path.to_string_lossy(),
                "edits": edits,
            })
        })
        .collect();
    serde_json::json!({ "fileEdits": file_edits }).to_string()
}

/// Applies a set of LSP text edits to `text`, returning the new content.
/// Edits are applied last-first (by start position) so earlier edits
/// don't shift later ones. Out-of-range positions are clamped, so this
/// never panics.
fn apply_text_edits(text: &str, edits: &[TextEditOut]) -> String {
    // Byte offset of the start of each line (line 0 starts at 0).
    let mut line_starts = vec![0usize];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(index + 1);
        }
    }
    let offset_of = |line: u32, col: u32| -> usize {
        let line = line as usize;
        let Some(&line_start) = line_starts.get(line) else {
            return text.len();
        };
        let line_end = line_starts
            .get(line + 1)
            .map_or(text.len(), |&next| next.saturating_sub(1));
        let line_text = &text[line_start..line_end];
        line_start + char_col_to_byte(line_text, col)
    };

    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let start = offset_of(edit.range.start_line, edit.range.start_col);
            let end = offset_of(edit.range.end_line, edit.range.end_col).max(start);
            (start, end, edit.new_text.as_str())
        })
        .collect();
    // Last edit first; stable sort so equal-start edits keep server order.
    spans.sort_by_key(|span| std::cmp::Reverse(span.0));

    let mut out = text.to_owned();
    for (start, end, new_text) in spans {
        let start = start.min(out.len());
        let end = end.min(out.len());
        out.replace_range(start..end, new_text);
    }
    out
}

/// Byte offset within `line` for a 0-based column. LSP counts UTF-16
/// code units; this walks them and stops at the first char boundary at
/// or past `col`. A `col` past the end returns `line.len()`.
fn char_col_to_byte(line: &str, col: u32) -> usize {
    let target = col as usize;
    let mut utf16 = 0usize;
    for (byte_index, ch) in line.char_indices() {
        if utf16 >= target {
            return byte_index;
        }
        utf16 += ch.len_utf16();
    }
    line.len()
}

/// Converts an LSP diagnostic (0-based positions) to a Monaco marker
/// (1-based).
fn to_marker(diagnostic: &LspDiagnostic) -> Marker {
    Marker {
        severity: match diagnostic.severity {
            editor_lsp::Severity::Error => MarkerSeverity::Error,
            editor_lsp::Severity::Warning => MarkerSeverity::Warning,
            editor_lsp::Severity::Information => MarkerSeverity::Info,
            editor_lsp::Severity::Hint => MarkerSeverity::Hint,
        },
        message: diagnostic.message.clone(),
        start_line: diagnostic.start_line + 1,
        start_col: diagnostic.start_col + 1,
        end_line: diagnostic.end_line + 1,
        end_col: diagnostic.end_col + 1,
    }
}

/// Converts an LSP diagnostic to a Problems-tab row for `file` (a path
/// relative to the project root).
fn to_problem(diagnostic: &LspDiagnostic, file: &Path) -> Diagnostic {
    Diagnostic {
        severity: match diagnostic.severity {
            editor_lsp::Severity::Error => ProblemSeverity::Error,
            editor_lsp::Severity::Warning => ProblemSeverity::Warning,
            editor_lsp::Severity::Information | editor_lsp::Severity::Hint => ProblemSeverity::Info,
        },
        code: diagnostic.code.clone(),
        message: diagnostic.message.clone(),
        file: Some(file.to_path_buf()),
        line: Some(diagnostic.start_line + 1),
    }
}

/// [`PlatformHandler`] that stands up a GPU context, an [`EditorShell`],
/// and a [`Viewport`], and draws the shell's UI every frame. Owns the
/// editor state and the path its session is persisted to.
struct EditorHandler {
    gpu: Option<GpuContext>,
    shell: Option<EditorShell>,
    viewport: Option<Viewport>,
    window: Option<Arc<Window>>,
    /// The Monaco code editor, or `None` if the OS webview couldn't be
    /// created — the studio stays usable without the Code panel.
    code: Option<CodeEditor>,
    state: EditorState,
    session_path: PathBuf,
    /// Last dirty state pushed to the window title, so `set_title` is only
    /// called when the `●` marker actually changes.
    title_dirty: bool,
    /// Source file (relative to `src/`) to open in the code editor once
    /// its webview reports [`CodeEvent::Ready`]. Taken on first use.
    pending_open: Option<PathBuf>,
    /// The session last written to disk — `autosave_session` compares
    /// against it to skip redundant writes.
    saved_session: EditorSession,
    /// Earliest instant `autosave_session` will write again (throttle).
    next_session_save: std::time::Instant,
    /// Earliest instant `autosave_scene` will write the crash backup
    /// again (throttle).
    next_scene_autosave: std::time::Instant,
    /// rust-analyzer, or `None` if it isn't installed / wouldn't start.
    /// Started in `on_window_ready`.
    lsp: Option<LspClient>,
    /// Latest diagnostics per file (absolute path), merged into the
    /// Problems tab each frame. rust-analyzer republishes a file's set
    /// wholesale, so an entry is replaced, not appended.
    lsp_problems: HashMap<PathBuf, Vec<Diagnostic>>,
    /// rust-analyzer request id → the Monaco provider `nonce` waiting on
    /// it. An entry is removed when its reply is routed back.
    lsp_waiters: HashMap<i64, u64>,
    /// Request id of the outstanding `workspace/symbol` query (Search
    /// tab), or `None`. Only the latest query matters.
    lsp_symbol_request: Option<i64>,
    /// Outstanding `workspace/symbol` id + the component type name for a
    /// pending Inspector "Open Script" jump.
    lsp_script_request: Option<(i64, String)>,
    /// Absolute paths the code editor currently has a model for, so
    /// go-to-definition into an already-open file doesn't reopen it.
    open_docs: HashSet<PathBuf>,
    /// Polls the project's assets directory for source-file changes so
    /// an edited/added/removed asset is re-imported without a restart.
    /// `None` if the directory couldn't be watched.
    asset_watcher: Option<AssetWatcher>,
}

impl EditorHandler {
    /// Persists the current workspace / panel layout / open scene to the
    /// project's `.studio/session.ron`. Called on shutdown.
    fn save_session(&self) {
        match self.state.capture_session().save(&self.session_path) {
            Ok(()) => tracing::info!(path = %self.session_path.display(), "editor session saved"),
            Err(err) => tracing::warn!(error = %err, "failed to save editor session"),
        }
    }

    /// Updates the OS window title to reflect the project name and a `●`
    /// marker while anything is unsaved — but only when that changes.
    fn sync_window_title(&mut self) {
        let dirty = self.state.dirty.any();
        if dirty == self.title_dirty {
            return;
        }
        self.title_dirty = dirty;
        if let Some(window) = &self.window {
            let marker = if dirty { " ●" } else { "" };
            window.set_title(&format!(
                "RustyEngine Studio — {}{marker}",
                self.state.project.name()
            ));
        }
    }
}

impl PlatformHandler for EditorHandler {
    fn on_window_ready(&mut self, window: Arc<Window>) {
        tracing::info!("editor window ready");
        let gpu = match GpuContext::new(Arc::clone(&window)) {
            Ok(gpu) => gpu,
            Err(err) => {
                tracing::error!(error = %err, "failed to initialize GPU context");
                return;
            }
        };
        let mut shell = EditorShell::new(&window, gpu.device(), gpu.config().format);
        let viewport = match Viewport::new(&gpu, &mut shell, 640, 480) {
            Ok(viewport) => viewport,
            Err(err) => {
                tracing::error!(error = %err, "failed to create editor viewport");
                return;
            }
        };

        window.set_title(&format!(
            "RustyEngine Studio — {}",
            self.state.project.name()
        ));

        // The Monaco webview, floated over the Code panel. Corrected to
        // the real panel rect on the first frame; a placeholder rect is
        // fine until then. Failure (no OS webview runtime) is non-fatal.
        let initial = PanelRect {
            x: 0,
            y: 400,
            width: 800,
            height: 200,
        };
        match CodeEditor::new(window.as_ref(), initial) {
            Ok(code) => self.code = Some(code),
            Err(err) => tracing::warn!(error = %err, "code editor unavailable"),
        }

        // rust-analyzer for the project. `None` (not installed / no
        // Cargo.toml) just means no completion or inline errors.
        self.lsp = LspClient::start(self.state.project.root());

        self.gpu = Some(gpu);
        self.shell = Some(shell);
        self.viewport = Some(viewport);
        self.window = Some(window);
    }

    fn on_raw_window_event(&mut self, window: &Window, event: &WindowEvent) {
        if let Some(shell) = &mut self.shell {
            // Consumed-or-not is unused for now — there's no other
            // input-reactive system in this binary yet to defer to egui.
            let _ = shell.handle_window_event(window, event);
        }
    }

    fn should_exit(&self) -> bool {
        // Set by File → Exit, or by the unsaved-changes dialog resolving
        // to Save/Discard while a Quit was pending.
        if self.state.exit_requested {
            self.save_session();
            true
        } else {
            false
        }
    }

    fn on_close_requested(&mut self) -> bool {
        if self.state.dirty.any() {
            // Keep running; the shell shows the "unsaved changes" dialog,
            // which sets `exit_requested` on Save/Discard.
            self.state.pending_action = Some(engine::editor::PendingAction::Quit);
            false
        } else {
            self.save_session();
            true
        }
    }

    fn on_event(&mut self, event: PlatformEvent) {
        match event {
            PlatformEvent::CloseRequested => {
                tracing::info!("close requested");
            }
            PlatformEvent::Resized { width, height } => {
                if let Some(gpu) = &mut self.gpu
                    && let Err(err) = gpu.resize(width, height)
                {
                    tracing::warn!(error = %err, "skipped surface resize");
                }
            }
            PlatformEvent::RedrawRequested => {
                // Reflects last frame's dirty state — a frame of lag on
                // the title marker is imperceptible.
                self.sync_window_title();

                self.render_frame();
                self.pump_lsp();
                // Keep the panel layout on disk even if the process is
                // killed (Ctrl-C on `cargo run`) instead of closed
                // cleanly — the graceful exit path may never run.
                self.autosave_session();
                self.autosave_scene();
            }
            PlatformEvent::KeyboardInput { .. }
            | PlatformEvent::MouseButtonInput { .. }
            | PlatformEvent::CursorMoved { .. }
            | PlatformEvent::MouseWheel { .. } => {}
        }
    }
}

impl EditorHandler {
    /// Runs one editor frame: play tick, viewport render, egui shell,
    /// code-editor sync, and the wgpu present. A no-op until
    /// [`PlatformHandler::on_window_ready`] has stood everything up.
    fn render_frame(&mut self) {
        {
            let (Some(gpu), Some(shell), Some(viewport), Some(window)) =
                (&self.gpu, &mut self.shell, &mut self.viewport, &self.window)
            else {
                return;
            };

            self.state.profiler.begin_frame();

            let span = std::time::Instant::now();
            self.state.play.tick(&mut self.state.world);
            self.state.profiler.record_span("play", span.elapsed());

            let span = std::time::Instant::now();
            let entity_transforms = self.state.entity_transforms();
            self.state.profiler.record_span("extract", span.elapsed());

            let span = std::time::Instant::now();
            viewport.set_dimension(gpu, self.state.dimension.is_2d());
            viewport.render(
                gpu,
                &entity_transforms,
                self.state.gizmo_mode,
                self.state.gizmo_origin(),
            );
            self.state.profiler.record_span("viewport", span.elapsed());

            let span = std::time::Instant::now();
            let mut full_output = shell.run_frame(window, viewport, &mut self.state);
            self.state.profiler.record_span("ui", span.elapsed());

            sync_code_editor(
                self.code.as_ref(),
                &mut self.state,
                &mut self.pending_open,
                &mut self.lsp,
                &mut self.lsp_waiters,
                &mut self.lsp_symbol_request,
                &mut self.lsp_script_request,
                &mut self.open_docs,
                window.scale_factor(),
            );

            handle_prefab_request(&mut self.state);
            handle_asset_changes(&mut self.state, self.asset_watcher.as_ref());

            // Unconditional, even though the frame itself might not
            // get drawn below (surface acquisition can skip a frame) —
            // see `EditorShell::apply_texture_updates`'s docs for why
            // texture handling can't share that fate.
            shell.apply_texture_updates(gpu.device(), gpu.queue(), &mut full_output);

            let size = window.inner_size();
            let screen_descriptor = ScreenDescriptor {
                size_in_pixels: [size.width, size.height],
                pixels_per_point: window.scale_factor() as f32,
            };

            let span = std::time::Instant::now();
            let result = gpu.render_with(|device, queue, encoder, view| {
                shell.render(
                    device,
                    queue,
                    encoder,
                    view,
                    &screen_descriptor,
                    full_output,
                );
            });
            self.state
                .profiler
                .record_span("egui-render", span.elapsed());
            self.state.profiler.end_frame();

            if let Err(err) = result {
                tracing::error!(error = %err, "editor frame render failed");
            }
        }
    }

    /// Writes the session to disk when the layout has changed, at most
    /// once a second. Complements the graceful-exit save so the panel
    /// layout survives a Ctrl-C / kill of `cargo run`.
    fn autosave_session(&mut self) {
        let now = std::time::Instant::now();
        if now < self.next_session_save {
            return;
        }
        self.next_session_save = now + std::time::Duration::from_secs(1);

        let current = self.state.capture_session();
        if current == self.saved_session {
            return;
        }
        match current.save(&self.session_path) {
            Ok(()) => {
                tracing::debug!("session autosaved");
                self.saved_session = current;
            }
            Err(err) => tracing::warn!(error = %err, "session autosave failed"),
        }
    }

    /// Every 5 seconds, if the scene has unsaved edits, writes a crash
    /// backup to `.studio/autosave.ron` (atomically). Cleared by an
    /// explicit save; picked up on the next launch by
    /// [`engine::editor::EditorState::recover_from_autosave`].
    fn autosave_scene(&mut self) {
        let now = std::time::Instant::now();
        if now < self.next_scene_autosave {
            return;
        }
        self.next_scene_autosave = now + std::time::Duration::from_secs(5);
        if !self.state.dirty.scene() {
            return;
        }
        match self.state.autosave_scene() {
            Ok(()) => tracing::debug!("scene autosaved"),
            Err(err) => tracing::warn!(error = %err, "scene autosave failed"),
        }
    }

    /// Drains everything rust-analyzer has sent since the last frame:
    /// diagnostics into Monaco's squiggles and the Problems tab, and
    /// replies to completion / hover / definition requests back to the
    /// Monaco provider that asked.
    fn pump_lsp(&mut self) {
        let (diagnostics, responses) = match self.lsp.as_ref() {
            Some(lsp) => (lsp.poll_diagnostics(), lsp.poll_responses()),
            None => return,
        };
        if !diagnostics.is_empty() {
            self.apply_lsp_diagnostics(diagnostics);
        }
        for response in responses {
            self.apply_lsp_response(response);
        }
    }

    /// Routes one server reply: `workspace/symbol` into the Search tab,
    /// everything else back to the waiting Monaco provider.
    fn apply_lsp_response(&mut self, response: LspResponse) {
        if let LspPayload::WorkspaceSymbols(result) = &response.payload {
            if self.lsp_symbol_request == Some(response.request_id) {
                self.lsp_symbol_request = None;
                self.state.search.symbol_results =
                    to_symbol_hits(result, self.state.project.root());
            } else if self
                .lsp_script_request
                .as_ref()
                .is_some_and(|(id, _)| *id == response.request_id)
                && let Some((_, type_name)) = self.lsp_script_request.take()
            {
                let root = self.state.project.root().to_path_buf();
                if let Some(symbol) = pick_script_symbol(&result.symbols, &type_name, &root) {
                    let absolute = symbol.path.clone();
                    let (line, col) = (symbol.line + 1, symbol.col + 1);
                    if let Some(code) = self.code.as_ref() {
                        if !self.open_docs.contains(&absolute) {
                            open_in_code_editor(
                                code,
                                self.lsp.as_mut(),
                                &mut self.open_docs,
                                &absolute,
                            );
                        }
                        code.reveal(&absolute, line, col);
                    }
                } else {
                    tracing::info!(
                        type_name = %type_name,
                        "open script: no matching declaration in the workspace"
                    );
                }
            }
            return;
        }
        let Some(nonce) = self.lsp_waiters.remove(&response.request_id) else {
            return;
        };
        let json = match &response.payload {
            LspPayload::Completion(result) => completion_json(result),
            LspPayload::Hover(result) => hover_json(result),
            LspPayload::Definition(result) => {
                // The jump target may be a file Monaco has no model for.
                // Open it (and notify rust-analyzer) first so Monaco can
                // navigate once the reply lands.
                if let Some(target) = result.targets.first() {
                    let needs_open =
                        !self.open_docs.contains(&target.path) && target.path.is_file();
                    if needs_open {
                        let target = target.path.clone();
                        if let Some(code) = self.code.as_ref() {
                            open_in_code_editor(
                                code,
                                self.lsp.as_mut(),
                                &mut self.open_docs,
                                &target,
                            );
                        }
                    }
                }
                definition_json(result)
            }
            LspPayload::References(result) => {
                // Pre-open referenced files (capped) so Monaco's peek
                // view can render each one.
                let to_open: Vec<PathBuf> = result
                    .locations
                    .iter()
                    .map(|location| location.path.clone())
                    .filter(|path| !self.open_docs.contains(path) && path.is_file())
                    .take(REFERENCE_PREOPEN_LIMIT)
                    .collect();
                for path in to_open {
                    if self.open_docs.contains(&path) {
                        continue; // a duplicate within this batch
                    }
                    if let Some(code) = self.code.as_ref() {
                        open_in_code_editor(code, self.lsp.as_mut(), &mut self.open_docs, &path);
                    }
                }
                references_json(result)
            }
            LspPayload::SignatureHelp(result) => signature_help_json(result),
            LspPayload::Rename(result) => self.apply_rename(result),
            // Handled above by the early return.
            LspPayload::WorkspaceSymbols(_) => return,
        };
        if let Some(code) = self.code.as_ref() {
            code.resolve_lsp(nonce, &json);
        }
    }

    /// Applies a rename's `WorkspaceEdit`. Files with no open Monaco
    /// model are edited on disk here (rust-analyzer's file watcher picks
    /// them up); the JSON returned lists only the *open* files' edits,
    /// which Monaco applies in-buffer (that then flows through
    /// `BufferEdited` → `did_change` + dirty like any edit).
    fn apply_rename(&self, result: &RenameOut) -> String {
        let mut open_edits: Vec<&editor_lsp::FileEdits> = Vec::new();
        for file in &result.file_edits {
            if self.open_docs.contains(&file.path) {
                open_edits.push(file);
                continue;
            }
            match std::fs::read_to_string(&file.path) {
                Ok(text) => {
                    let updated = apply_text_edits(&text, &file.edits);
                    if let Err(err) = std::fs::write(&file.path, updated) {
                        tracing::warn!(
                            error = %err, path = %file.path.display(),
                            "rename: could not write file (partial rename)"
                        );
                    } else {
                        tracing::info!(path = %file.path.display(), "rename: wrote file");
                    }
                }
                Err(err) => tracing::warn!(
                    error = %err, path = %file.path.display(),
                    "rename: could not read file (skipped)"
                ),
            }
        }
        rename_json(&open_edits)
    }

    /// Merges a batch of diagnostic updates into Monaco and the Problems
    /// tab.
    fn apply_lsp_diagnostics(&mut self, updates: Vec<FileDiagnostics>) {
        let root = self.state.project.root().to_path_buf();
        for file in updates {
            if let Some(code) = self.code.as_ref() {
                let markers: Vec<Marker> = file.items.iter().map(to_marker).collect();
                code.set_diagnostics(&file.path, &markers);
            }
            let relative = file
                .path
                .strip_prefix(&root)
                .unwrap_or(&file.path)
                .to_path_buf();
            let rows: Vec<Diagnostic> = file
                .items
                .iter()
                .map(|item| to_problem(item, &relative))
                .collect();
            if rows.is_empty() {
                self.lsp_problems.remove(&file.path);
            } else {
                self.lsp_problems.insert(file.path.clone(), rows);
            }
        }
        let mut all: Vec<Diagnostic> = self.lsp_problems.values().flatten().cloned().collect();
        all.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.message.cmp(&b.message)));
        tracing::debug!(count = all.len(), "diagnostics updated");
        self.state.diagnostics.set(all);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let console_log = ConsoleLog::new();
    logging::init_default_with_layer(ConsoleLayer::new(console_log.clone()))?;

    // Project root: first CLI argument, else the default dev project.
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROJECT_DIR));
    let is_new_project = !root.join(MANIFEST_FILE).exists();
    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let project = Project::open_or_create(&root, project_name)?;
    if is_new_project {
        // Seed the fresh project's main scene with the demo world, so the
        // first run still shows something to edit.
        let scene = Scene::from_world(&mut placeholder_world());
        scene.save_to_file(&project.main_scene_path())?;
        tracing::info!(path = %project.main_scene_path().display(), "seeded starter scene");
        // Seed a starter source tree so the code editor has real files.
        seed_file(&project.src_dir().join("main.rs"), STARTER_MAIN);
        seed_file(&project.src_dir().join("lib.rs"), STARTER_LIB);
    }
    // Ensure a `Cargo.toml` exists (also for projects created before this)
    // so rust-analyzer has a package to analyse.
    seed_file(
        &project.root().join("Cargo.toml"),
        &starter_cargo_toml(project.name()),
    );

    let config = EngineConfig::new(project.name(), env!("CARGO_PKG_VERSION"));
    let mut app = App::new(config)?;

    let assets_dir = project.assets_dir();
    let assets = match scan_assets(&assets_dir) {
        Ok(assets) => assets,
        Err(err) => {
            // A missing/unreadable assets directory shouldn't stop the
            // editor from opening — the panel just shows nothing.
            tracing::warn!(error = %err, dir = %assets_dir.display(), "failed to scan assets directory");
            Vec::new()
        }
    };

    let session_path = project.studio_dir().join(SESSION_FILE);
    let session = EditorSession::load(&session_path);
    let saved_session = session.clone();

    let mut state = EditorState::new(project, World::new(), assets, console_log);
    state.apply_session(session);
    // Import every referenceable asset (mesh / texture / audio), gated by
    // an `ImportCache` so unchanged sources aren't re-decoded, and build
    // the id↔path index so the scene load below can re-resolve a moved or
    // renamed reference to its current file.
    let assets_dir = state.project.assets_dir();
    state.asset_index = state.importer.run(&assets_dir, &mut state.assets);
    let import_stats = state.importer.stats();
    tracing::info!(
        total = import_stats.total,
        imported = import_stats.imported,
        cached = import_stats.cached,
        failed = import_stats.failed,
        "asset import pass complete"
    );
    if let Err(err) = state.load_scene() {
        tracing::warn!(
            error = %err,
            path = %state.scene_path.display(),
            "could not load the scene; starting empty"
        );
    }

    // Crash recovery: if a `.studio/autosave.ron` newer than the scene
    // is present, a previous session ended with unsaved edits — load
    // those into the world instead (leaving the scene file untouched
    // until an explicit save).
    match state.recover_from_autosave() {
        Ok(true) => tracing::warn!(
            "recovered unsaved scene changes from a previous session; save to keep them"
        ),
        Ok(false) => {}
        Err(err) => tracing::warn!(error = %err, "autosave recovery failed"),
    }

    // Watch the assets directory so an edited/added/removed source file
    // is re-imported live. Non-fatal if it can't start (e.g. the dir
    // doesn't exist yet) — the editor just won't hot-reload assets.
    let asset_watcher = match AssetWatcher::watch(
        &assets_dir,
        std::time::Duration::from_millis(500),
    ) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            tracing::warn!(error = %err, dir = %assets_dir.display(), "asset hot-reload disabled");
            None
        }
    };

    let window_config = WindowConfig::new(app.config().app_name.clone(), 1280, 720);
    run_windowed(
        window_config,
        EditorHandler {
            gpu: None,
            shell: None,
            viewport: None,
            window: None,
            code: None,
            state,
            session_path,
            title_dirty: false,
            pending_open: Some(PathBuf::from("main.rs")),
            saved_session,
            next_session_save: std::time::Instant::now(),
            next_scene_autosave: std::time::Instant::now(),
            lsp: None,
            lsp_problems: HashMap::new(),
            lsp_waiters: HashMap::new(),
            lsp_symbol_request: None,
            lsp_script_request: None,
            open_docs: HashSet::new(),
            asset_watcher,
        },
    )?;

    app.shutdown()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_lsp::RangeOut;

    fn edit(sl: u32, sc: u32, el: u32, ec: u32, new_text: &str) -> TextEditOut {
        TextEditOut {
            range: RangeOut {
                start_line: sl,
                start_col: sc,
                end_line: el,
                end_col: ec,
            },
            new_text: new_text.to_owned(),
        }
    }

    #[test]
    fn char_col_to_byte_counts_utf16_units() {
        assert_eq!(char_col_to_byte("hello", 0), 0);
        assert_eq!(char_col_to_byte("hello", 3), 3);
        assert_eq!(char_col_to_byte("hello", 99), 5);
        // "é" is one UTF-16 unit but two UTF-8 bytes.
        assert_eq!(char_col_to_byte("é_x", 1), 2);
        assert_eq!(char_col_to_byte("é_x", 2), 3);
    }

    #[test]
    fn apply_text_edits_replaces_two_sites_on_one_line() {
        // `let helper = helper;` → rename both `helper` to `renamed`.
        let src = "let helper = helper;\n";
        let out = apply_text_edits(
            src,
            &[edit(0, 4, 0, 10, "renamed"), edit(0, 13, 0, 19, "renamed")],
        );
        assert_eq!(out, "let renamed = renamed;\n");
    }

    #[test]
    fn apply_text_edits_spans_multiple_lines() {
        let src = "fn helper() {}\n\nfn main() { helper(); }\n";
        let out = apply_text_edits(
            src,
            &[edit(0, 3, 0, 9, "renamed"), edit(2, 12, 2, 18, "renamed")],
        );
        assert_eq!(out, "fn renamed() {}\n\nfn main() { renamed(); }\n");
    }

    #[test]
    fn apply_text_edits_clamps_out_of_range_positions() {
        let src = "abc\n";
        // Line 9 doesn't exist: both ends clamp to EOF — no panic, and
        // the replacement lands at the end rather than corrupting text.
        let out = apply_text_edits(src, &[edit(9, 0, 9, 3, "X")]);
        assert_eq!(out, "abc\nX");
    }

    fn symbol(name: &str, kind: u8, path: &str) -> SymbolOut {
        SymbolOut {
            name: name.to_string(),
            kind,
            container: None,
            path: PathBuf::from(path),
            line: 0,
            col: 0,
        }
    }

    #[test]
    fn pick_script_symbol_prefers_an_in_project_declaration() {
        let root = Path::new("/proj");
        let symbols = [
            symbol("Camera", 12, "/proj/src/uses_camera.rs"), // fn, wrong kind
            symbol("Camera", 23, "/home/u/.cargo/engine/camera.rs"), // struct, dependency
            symbol("Camera", 23, "/proj/src/camera.rs"),      // struct, in project
        ];
        let picked = pick_script_symbol(&symbols, "Camera", root).expect("a match");
        assert_eq!(picked.path, PathBuf::from("/proj/src/camera.rs"));
    }

    #[test]
    fn pick_script_symbol_falls_back_to_a_dependency_declaration() {
        let root = Path::new("/proj");
        let symbols = [
            symbol("Transform", 6, "/proj/src/x.rs"), // method, wrong kind
            symbol("Transform", 23, "/home/u/.cargo/engine/transform.rs"), // struct, dep
        ];
        let picked = pick_script_symbol(&symbols, "Transform", root).expect("a match");
        assert_eq!(
            picked.path,
            PathBuf::from("/home/u/.cargo/engine/transform.rs")
        );
    }

    #[test]
    fn pick_script_symbol_returns_none_without_a_named_declaration() {
        let root = Path::new("/proj");
        let symbols = [
            symbol("Camera", 12, "/proj/src/a.rs"), // fn, not a decl kind
            symbol("Other", 23, "/proj/src/b.rs"),  // struct, wrong name
        ];
        assert!(pick_script_symbol(&symbols, "Camera", root).is_none());
        assert!(pick_script_symbol(&[], "Camera", root).is_none());
    }

    #[test]
    fn is_meta_path_matches_only_meta_sidecars() {
        assert!(is_meta_path(Path::new("assets/rock.gltf.meta")));
        assert!(is_meta_path(Path::new("brick.png.META")));
        assert!(!is_meta_path(Path::new("assets/rock.gltf")));
        assert!(!is_meta_path(Path::new("notes")));
    }

    #[test]
    fn rename_json_lists_only_the_passed_files_one_based() {
        let file = editor_lsp::FileEdits {
            path: PathBuf::from("/tmp/p/src/a.rs"),
            edits: vec![edit(0, 3, 0, 9, "renamed")],
        };
        let json: serde_json::Value =
            serde_json::from_str(&rename_json(&[&file])).expect("valid json");
        let entry = &json["fileEdits"][0];
        assert_eq!(entry["path"], "/tmp/p/src/a.rs");
        assert_eq!(entry["edits"][0]["startLineNumber"], 1);
        assert_eq!(entry["edits"][0]["startColumn"], 4);
        assert_eq!(entry["edits"][0]["text"], "renamed");
    }
}
