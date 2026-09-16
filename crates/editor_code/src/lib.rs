//! # editor_code
//!
//! The integrated code editor: a [Monaco] instance running in a [`wry`]
//! webview that is pinned as a child window over the studio's Code panel
//! rectangle. It is **not** composited into the egui canvas — the host
//! positions it every frame with [`CodeEditor::set_bounds_px`] and hides
//! it with [`CodeEditor::set_visible`] when the panel is hidden or a
//! modal is up.
//!
//! Communication is a small JSON channel:
//!
//! - host → editor: [`CodeEditor::open_file`], [`CodeEditor::notify_saved`]
//! - editor → host: [`CodeEditor::poll`] drains [`CodeEvent`]s
//!   ([`CodeEvent::BufferEdited`], [`CodeEvent::CursorMoved`],
//!   [`CodeEvent::SaveRequested`]).
//!
//! Monaco is served offline from a vendored `min/vs` build (see
//! `vendor/monaco/`) through a `monaco://` custom protocol.
//!
//! Language features (completion, hover, go-to-definition) go through the
//! host: Monaco's providers raise a [`CodeEvent::LspRequest`] carrying a
//! `nonce`, the host asks rust-analyzer, and [`CodeEditor::resolve_lsp`]
//! feeds the answer back to the waiting provider Promise.
//!
//! ## Packaging
//!
//! The vendored build is located at *runtime*, first hit wins: the
//! `EDITOR_CODE_MONACO_DIR` env var, then `monaco/` beside the
//! running executable, then `../Resources/monaco` (macOS `.app`
//! bundle), then the crate's `vendor/monaco` (the `cargo run` dev
//! path). `cargo build -p editor` runs a build script that stages
//! `vendor/monaco/` into `target/<profile>/monaco`, so a shipped
//! `editor` binary plus that folder runs with no source tree.
//!
//! [Monaco]: https://microsoft.github.io/monaco-editor/

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

use serde::Deserialize;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::http::{Request, Response, header::CONTENT_TYPE};
use wry::raw_window_handle::HasWindowHandle;
use wry::{Rect, WebView, WebViewBuilder, WebViewId};

/// The crate's own `vendor/monaco` — the `cargo run` dev location and
/// the last-resort fallback.
const DEV_MONACO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vendor/monaco");

/// The vendored-Monaco directory, resolved once at first use. See the
/// crate's "Packaging" docs for the search order.
fn monaco_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(resolve_monaco_dir)
}

/// Picks the vendored-Monaco directory: `EDITOR_CODE_MONACO_DIR`, then
/// `monaco/` next to the executable, then a macOS `.app`
/// `../Resources/monaco`, then the crate's `vendor/monaco`. Returns the
/// first that contains an `index.html`; if none do, returns the dev
/// path anyway so [`CodeError::Asset`] fires with a sensible location.
fn resolve_monaco_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(from_env) = std::env::var("EDITOR_CODE_MONACO_DIR")
        && !from_env.is_empty()
    {
        candidates.push(PathBuf::from(from_env));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        candidates.push(exe_dir.join("monaco"));
        candidates.push(exe_dir.join("../Resources/monaco"));
    }
    candidates.push(PathBuf::from(DEV_MONACO_DIR));

    for candidate in &candidates {
        if candidate.join("index.html").is_file() {
            return candidate.clone();
        }
    }
    PathBuf::from(DEV_MONACO_DIR)
}

/// Errors from setting up the code editor.
#[derive(Debug, thiserror::Error)]
pub enum CodeError {
    /// The webview could not be created (missing OS webview runtime, bad
    /// window handle, ...).
    #[error("failed to create the code-editor webview: {0}")]
    WebViewInit(String),
    /// A required vendored asset is missing.
    #[error("code-editor asset missing: {0}")]
    Asset(&'static str),
}

/// A rectangle in physical pixels, matching the studio's Code panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelRect {
    /// Left edge, physical px from the window's top-left.
    pub x: i32,
    /// Top edge, physical px from the window's top-left.
    pub y: i32,
    /// Width in physical px.
    pub width: u32,
    /// Height in physical px.
    pub height: u32,
}

impl PanelRect {
    fn to_wry(self) -> Rect {
        Rect {
            position: PhysicalPosition::new(self.x, self.y).into(),
            size: PhysicalSize::new(self.width.max(1), self.height.max(1)).into(),
        }
    }
}

/// Something the editor reported back to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEvent {
    /// The buffer for `path` changed and is now unsaved. `text` is the
    /// full new document (Monaco full-sync), for the LSP bridge.
    BufferEdited {
        /// The edited file.
        path: PathBuf,
        /// The complete current buffer text.
        text: String,
    },
    /// The caret moved within `path` (1-based line/column).
    CursorMoved {
        /// The active file.
        path: PathBuf,
        /// 1-based line.
        line: u32,
        /// 1-based column.
        column: u32,
    },
    /// The user pressed save (Cmd/Ctrl+S); `contents` is the full buffer.
    SaveRequested {
        /// The file to write.
        path: PathBuf,
        /// The full buffer text.
        contents: String,
    },
    /// The user picked a file in the editor's Files tree that isn't open
    /// yet. `path` is relative to the tree root last set with
    /// [`CodeEditor::set_file_tree`]; the host resolves it, reads the
    /// file, and calls [`CodeEditor::open_file`].
    OpenRequested {
        /// Path relative to the file-tree root.
        path: PathBuf,
    },
    /// The user asked for a new file or directory in the Files tree.
    /// `path` is relative to the tree root and comes from a text field
    /// in the webview — **untrusted**. The host must reject anything
    /// that leaves the project root (see `engine_project::Project::resolve`)
    /// before creating it, then re-send the tree.
    CreateRequested {
        /// Tree-root-relative path to create.
        path: PathBuf,
        /// `true` for a directory, `false` for an empty file.
        folder: bool,
    },
    /// The webview's script bridge is live. The host should (re)send the
    /// file tree and open its starting file on this — sending earlier
    /// races the Monaco/loader boot.
    Ready,
    /// The user closed a tab (✕ button or middle-click). The host should
    /// stop tracking `path` as an open document (drop it from any
    /// open-doc set, tell the language server it was closed). Any unsaved
    /// buffer is discarded — there is no save prompt.
    TabClosed {
        /// The file whose tab was closed.
        path: PathBuf,
    },
    /// A Monaco language provider needs an LSP answer. The host resolves
    /// `path` (relative to the active model, i.e. the file-tree root),
    /// asks rust-analyzer, and replies with [`CodeEditor::resolve_lsp`]
    /// carrying the same `nonce`. `line`/`col` are 0-based (LSP
    /// convention — Monaco's 1-based position is converted before the
    /// event is sent).
    LspRequest {
        /// Correlates the reply to the waiting provider Promise.
        nonce: u64,
        /// Which language feature asked.
        kind: LspKind,
        /// File the request is about, tree-root-relative.
        path: PathBuf,
        /// 0-based line.
        line: u32,
        /// 0-based column.
        col: u32,
        /// The new identifier — only set for [`LspKind::Rename`].
        new_name: Option<String>,
    },
}

/// Which Monaco language provider raised a [`CodeEvent::LspRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspKind {
    /// `registerCompletionItemProvider`.
    Completion,
    /// `registerHoverProvider`.
    Hover,
    /// `registerDefinitionProvider`.
    Definition,
    /// `registerReferenceProvider`.
    References,
    /// `registerSignatureHelpProvider`.
    SignatureHelp,
    /// `registerRenameProvider`.
    Rename,
}

impl LspKind {
    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "completion" => Some(Self::Completion),
            "hover" => Some(Self::Hover),
            "definition" => Some(Self::Definition),
            "references" => Some(Self::References),
            "signatureHelp" => Some(Self::SignatureHelp),
            "rename" => Some(Self::Rename),
            _ => None,
        }
    }
}

/// Severity of a [`Marker`], mapped to Monaco's `MarkerSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerSeverity {
    /// Red squiggle.
    Error,
    /// Yellow squiggle.
    Warning,
    /// Blue squiggle.
    Info,
    /// Faint squiggle.
    Hint,
}

impl MarkerSeverity {
    /// The numeric code Monaco's `MarkerSeverity` uses.
    fn monaco_code(self) -> u8 {
        match self {
            MarkerSeverity::Hint => 1,
            MarkerSeverity::Info => 2,
            MarkerSeverity::Warning => 4,
            MarkerSeverity::Error => 8,
        }
    }
}

/// One diagnostic squiggle for [`CodeEditor::set_diagnostics`]. Line and
/// column are 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// How the squiggle is drawn.
    pub severity: MarkerSeverity,
    /// Hover text.
    pub message: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_col: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawEvent {
    Edited {
        path: String,
        text: String,
    },
    Cursor {
        path: String,
        line: u32,
        column: u32,
    },
    Save {
        path: String,
        contents: String,
    },
    Open {
        path: String,
    },
    Ready,
    Closed {
        path: String,
    },
    Create {
        path: String,
        folder: bool,
    },
    Lsp {
        nonce: u64,
        kind: String,
        path: String,
        line: u32,
        col: u32,
        #[serde(default)]
        new_name: Option<String>,
    },
}

impl CodeEvent {
    /// Parses one IPC message. Returns `None` for anything that isn't a
    /// recognised event.
    pub fn parse(message: &str) -> Option<Self> {
        match serde_json::from_str::<RawEvent>(message).ok()? {
            RawEvent::Edited { path, text } => Some(Self::BufferEdited {
                path: path.into(),
                text,
            }),
            RawEvent::Cursor { path, line, column } => Some(Self::CursorMoved {
                path: path.into(),
                line,
                column,
            }),
            RawEvent::Save { path, contents } => Some(Self::SaveRequested {
                path: path.into(),
                contents,
            }),
            RawEvent::Open { path } => Some(Self::OpenRequested { path: path.into() }),
            RawEvent::Ready => Some(Self::Ready),
            RawEvent::Closed { path } => Some(Self::TabClosed { path: path.into() }),
            RawEvent::Create { path, folder } => Some(Self::CreateRequested {
                path: path.into(),
                folder,
            }),
            RawEvent::Lsp {
                nonce,
                kind,
                path,
                line,
                col,
                new_name,
            } => Some(Self::LspRequest {
                nonce,
                kind: LspKind::from_wire(&kind)?,
                path: path.into(),
                line,
                col,
                new_name,
            }),
        }
    }
}

/// The Monaco-backed code editor. Owns its webview and an event queue the
/// host drains each frame with [`CodeEditor::poll`].
pub struct CodeEditor {
    webview: WebView,
    events: Receiver<CodeEvent>,
    /// Last rect handed to [`CodeEditor::set_bounds_px`], so a call with
    /// an unchanged rect (the common case — the host syncs every frame)
    /// skips the OS call and the Monaco relayout.
    last_bounds: std::cell::Cell<Option<PanelRect>>,
}

impl CodeEditor {
    /// Creates the editor as a child webview of `window`, initially at
    /// `bounds`.
    ///
    /// # Errors
    ///
    /// [`CodeError::Asset`] if the vendored Monaco build is missing;
    /// [`CodeError::WebViewInit`] if the OS refuses to create the webview.
    pub fn new(window: &impl HasWindowHandle, bounds: PanelRect) -> Result<Self, CodeError> {
        if !monaco_dir().join("index.html").is_file() {
            return Err(CodeError::Asset("monaco/index.html"));
        }

        let (tx, rx): (Sender<CodeEvent>, Receiver<CodeEvent>) = channel();

        let webview = WebViewBuilder::new()
            .with_bounds(bounds.to_wry())
            .with_custom_protocol("monaco".into(), serve_asset)
            .with_ipc_handler(move |request: Request<String>| {
                tracing::debug!(message = %request.body(), "code editor ipc");
                if let Some(event) = CodeEvent::parse(request.body()) {
                    let _ = tx.send(event);
                }
            })
            .with_devtools(true)
            .with_url("monaco://localhost/index.html")
            .build_as_child(window)
            .map_err(|err| CodeError::WebViewInit(err.to_string()))?;

        // wry anchors the child webview to its superview with an
        // autoresizing mask, which overrides our explicit per-frame
        // `set_bounds` on the next AppKit layout pass — the webview would
        // stick to one edge and stop following the Code panel. Clearing
        // the mask makes `set_bounds` authoritative.
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSAutoresizingMaskOptions;
            use wry::WebViewExtMacOS;
            webview
                .webview()
                .setAutoresizingMask(NSAutoresizingMaskOptions::empty());
        }

        tracing::info!(
            monaco_dir = %monaco_dir().display(),
            ?bounds,
            "code editor webview created"
        );

        Ok(Self {
            webview,
            events: rx,
            last_bounds: std::cell::Cell::new(None),
        })
    }

    /// Opens the webview's developer tools (right-click → Inspect also
    /// works). Handy while wiring the editor up.
    pub fn open_devtools(&self) {
        self.webview.open_devtools();
    }

    /// Repositions the webview over the Code panel (physical pixels).
    ///
    /// Also nudges Monaco to re-measure: its `automaticLayout` observer
    /// is unreliable inside a `WKWebView` that starts hidden / zero-sized
    /// (it comes up collapsed to one line), so every bounds change also
    /// calls `editor.layout()` explicitly.
    pub fn set_bounds_px(&self, rect: PanelRect) {
        if self.last_bounds.get() == Some(rect) {
            return;
        }
        self.last_bounds.set(Some(rect));
        tracing::debug!(?rect, "code editor set_bounds");
        if let Err(err) = self.webview.set_bounds(rect.to_wry()) {
            tracing::warn!(error = %err, "code editor set_bounds failed");
        }
        self.eval("window.studio && window.studio.relayout && window.studio.relayout()");
    }

    /// Shows or hides the webview. Hide it when the Code panel is hidden
    /// or an egui modal is open (egui can't draw over a native view).
    pub fn set_visible(&self, visible: bool) {
        if let Err(err) = self.webview.set_visible(visible) {
            tracing::warn!(error = %err, "code editor set_visible failed");
        }
    }

    /// Opens `path` in the editor (creating or replacing its buffer).
    pub fn open_file(&self, path: &Path, contents: &str) {
        let payload = serde_json::json!({
            "path": path.to_string_lossy(),
            "language": language_for(path),
            "contents": contents,
        });
        self.eval(&format!(
            "window.studio && window.studio.openFile({}, {}, {})",
            json_field(&payload, "path"),
            json_field(&payload, "language"),
            json_field(&payload, "contents"),
        ));
    }

    /// Populates the editor's Files tree. `files` are paths relative to
    /// `root` (the project's source directory); `label` is the tree's
    /// heading. Clicking a file that isn't open yet produces a
    /// [`CodeEvent::OpenRequested`] carrying that relative path.
    pub fn set_file_tree(&self, label: &str, root: &Path, files: &[PathBuf]) {
        let files: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        let payload = serde_json::json!({
            "label": label,
            "root": root.to_string_lossy().replace('\\', "/"),
            "files": files,
        });
        self.eval(&format!(
            "window.studio && window.studio.setFiles({payload})"
        ));
    }

    /// Replaces the diagnostic squiggles Monaco shows for `path`. An
    /// empty `markers` clears them. Positions are 1-based (Monaco's
    /// convention) — the caller converts from LSP's 0-based positions.
    pub fn set_diagnostics(&self, path: &Path, markers: &[Marker]) {
        let payload: Vec<serde_json::Value> = markers
            .iter()
            .map(|marker| {
                serde_json::json!({
                    "startLineNumber": marker.start_line,
                    "startColumn": marker.start_col,
                    "endLineNumber": marker.end_line,
                    "endColumn": marker.end_col,
                    "message": marker.message,
                    "severity": marker.severity.monaco_code(),
                })
            })
            .collect();
        self.eval(&format!(
            "window.studio && window.studio.setDiagnostics({}, {})",
            serde_json::Value::from(path.to_string_lossy().as_ref()),
            serde_json::Value::Array(payload),
        ));
    }

    /// Answers a [`CodeEvent::LspRequest`]. `payload_json` is the reply
    /// already shaped for the Monaco provider that asked (a `suggestions`
    /// object for completion, a `contents` object for hover, a
    /// `{uri,range}` object for definition), or `null` for "nothing".
    /// `nonce` must match the request.
    pub fn resolve_lsp(&self, nonce: u64, payload_json: &str) {
        self.eval(&format!(
            "window.studio && window.studio.resolveLsp({nonce}, {payload_json})"
        ));
    }

    /// Moves the editor's caret to `path` `line`:`col` (1-based),
    /// scrolling it into view. Used for go-to-definition when the target
    /// is a file the host just opened. A no-op if `path` isn't an open
    /// model.
    pub fn reveal(&self, path: &Path, line: u32, col: u32) {
        self.eval(&format!(
            "window.studio && window.studio.reveal({}, {line}, {col})",
            serde_json::Value::from(path.to_string_lossy().as_ref()),
        ));
    }

    /// Tells the editor `path` is now saved (clears its dirty marker).
    pub fn notify_saved(&self, path: &Path) {
        self.eval(&format!(
            "window.studio && window.studio.markSaved({})",
            serde_json::Value::from(path.to_string_lossy().as_ref())
        ));
    }

    /// Asks the editor to save its active file — the same effect as the
    /// in-editor Ctrl/Cmd+S, but the host can call it when egui (not the
    /// webview) holds keyboard focus. A [`CodeEvent::SaveRequested`]
    /// follows if there is an active file.
    pub fn request_save(&self) {
        self.eval("window.studio && window.studio.saveActive()");
    }

    /// Gives the webview keyboard focus — called when the user clicks
    /// into the Code panel so typing goes to Monaco instead of egui.
    /// Best-effort: a failure is logged, not returned.
    pub fn focus(&self) {
        if let Err(err) = self.webview.focus() {
            tracing::debug!(error = %err, "code editor focus failed");
        }
    }

    /// Drains every event the editor has reported since the last call.
    pub fn poll(&self) -> Vec<CodeEvent> {
        self.events.try_iter().collect()
    }

    fn eval(&self, js: &str) {
        if let Err(err) = self.webview.evaluate_script(js) {
            tracing::warn!(error = %err, "code editor script eval failed");
        }
    }
}

/// A JSON-encoded scalar field of `value`, safe to splice into a JS call.
fn json_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .map_or_else(|| "null".to_string(), std::string::ToString::to_string)
}

/// Monaco language id for a file extension.
fn language_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("md") => "markdown",
        Some("json") => "json",
        Some("wgsl" | "glsl") => "wgsl",
        Some("ron") => "rust", // close enough for highlighting
        _ => "plaintext",
    }
}

/// Serves a file from the vendored Monaco directory over `monaco://`.
fn serve_asset(_id: WebViewId<'_>, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let raw = request.uri().path();
    let relative = raw.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    if relative.split('/').any(|segment| segment == "..") {
        return simple_response(403, b"forbidden");
    }

    let full = monaco_dir().join(relative);
    match std::fs::read(&full) {
        Ok(bytes) => {
            tracing::debug!(
                asset = relative,
                bytes = bytes.len(),
                "serving monaco asset"
            );
            Response::builder()
                .header(CONTENT_TYPE, content_type(relative))
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Owned(bytes))
                .unwrap_or_else(|_| simple_response(500, b"response build failed"))
        }
        Err(err) => {
            tracing::warn!(asset = relative, error = %err, "monaco asset not found");
            simple_response(404, b"not found")
        }
    }
}

fn simple_response(status: u16, body: &'static [u8]) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .body(Cow::Borrowed(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ttf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_event_kind() {
        assert_eq!(
            CodeEvent::parse(r#"{"type":"edited","path":"src/a.rs","text":"fn a() {}"}"#),
            Some(CodeEvent::BufferEdited {
                path: PathBuf::from("src/a.rs"),
                text: "fn a() {}".to_string(),
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"cursor","path":"a.rs","line":12,"column":4}"#),
            Some(CodeEvent::CursorMoved {
                path: PathBuf::from("a.rs"),
                line: 12,
                column: 4
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"save","path":"a.rs","contents":"fn main(){}"}"#),
            Some(CodeEvent::SaveRequested {
                path: PathBuf::from("a.rs"),
                contents: "fn main(){}".to_string()
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"create","path":"src/systems/ai.rs","folder":false}"#),
            Some(CodeEvent::CreateRequested {
                path: PathBuf::from("src/systems/ai.rs"),
                folder: false,
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"create","path":"src/systems","folder":true}"#),
            Some(CodeEvent::CreateRequested {
                path: PathBuf::from("src/systems"),
                folder: true,
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"open","path":"systems/move.rs"}"#),
            Some(CodeEvent::OpenRequested {
                path: PathBuf::from("systems/move.rs")
            })
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"ready"}"#),
            Some(CodeEvent::Ready)
        );
        assert_eq!(
            CodeEvent::parse(r#"{"type":"closed","path":"src/a.rs"}"#),
            Some(CodeEvent::TabClosed {
                path: PathBuf::from("src/a.rs")
            })
        );
        assert_eq!(
            CodeEvent::parse(
                r#"{"type":"lsp","nonce":9,"kind":"hover","path":"src/a.rs","line":3,"col":7}"#
            ),
            Some(CodeEvent::LspRequest {
                nonce: 9,
                kind: LspKind::Hover,
                path: PathBuf::from("src/a.rs"),
                line: 3,
                col: 7,
                new_name: None,
            })
        );
        assert_eq!(
            CodeEvent::parse(
                r#"{"type":"lsp","nonce":10,"kind":"rename","path":"src/a.rs","line":1,"col":4,"new_name":"foo"}"#
            ),
            Some(CodeEvent::LspRequest {
                nonce: 10,
                kind: LspKind::Rename,
                path: PathBuf::from("src/a.rs"),
                line: 1,
                col: 4,
                new_name: Some("foo".to_string()),
            })
        );
    }

    #[test]
    fn lsp_kind_maps_each_provider_and_rejects_others() {
        assert_eq!(LspKind::from_wire("completion"), Some(LspKind::Completion));
        assert_eq!(LspKind::from_wire("definition"), Some(LspKind::Definition));
        assert_eq!(LspKind::from_wire("references"), Some(LspKind::References));
        assert_eq!(
            LspKind::from_wire("signatureHelp"),
            Some(LspKind::SignatureHelp)
        );
        assert_eq!(LspKind::from_wire("rename"), Some(LspKind::Rename));
        assert_eq!(LspKind::from_wire("formatting"), None);
        // An unknown kind drops the whole event.
        assert_eq!(
            CodeEvent::parse(
                r#"{"type":"lsp","nonce":1,"kind":"formatting","path":"a.rs","line":0,"col":0}"#
            ),
            None
        );
    }

    #[test]
    fn rejects_junk_and_unknown_kinds() {
        assert_eq!(CodeEvent::parse("not json"), None);
        assert_eq!(CodeEvent::parse(r#"{"type":"wat","path":"x"}"#), None);
        assert_eq!(CodeEvent::parse(r#"{"type":"cursor","path":"x"}"#), None);
    }

    #[test]
    fn language_ids_by_extension() {
        assert_eq!(language_for(Path::new("player.rs")), "rust");
        assert_eq!(language_for(Path::new("Cargo.toml")), "toml");
        assert_eq!(language_for(Path::new("notes.md")), "markdown");
        assert_eq!(language_for(Path::new("shader.wgsl")), "wgsl");
        assert_eq!(language_for(Path::new("data.bin")), "plaintext");
        assert_eq!(language_for(Path::new("noext")), "plaintext");
    }

    #[test]
    fn content_types_cover_monaco_assets() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("vs/loader.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("vs/editor/editor.main.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("vs/base/codicon.ttf"), "font/ttf");
        assert_eq!(content_type("weird"), "application/octet-stream");
    }

    #[test]
    fn panel_rect_converts_with_a_minimum_size() {
        let r = PanelRect {
            x: 10,
            y: 20,
            width: 0,
            height: 0,
        }
        .to_wry();
        // Zero collapses to 1 so the webview never has an invalid size.
        match r.size {
            wry::dpi::Size::Physical(p) => assert_eq!((p.width, p.height), (1, 1)),
            other => panic!("expected physical size, got {other:?}"),
        }
    }

    #[test]
    fn the_vendored_monaco_build_is_present() {
        // With no env override and the test binary not beside a `monaco/`
        // folder, the resolver falls through to the crate's `vendor/`.
        let dir = monaco_dir();
        assert!(dir.join("index.html").is_file(), "index.html vendored");
        assert!(dir.join("vs/loader.js").is_file(), "Monaco loader vendored");
        assert!(
            dir.join("vs/editor/editor.main.js").is_file(),
            "Monaco editor entry vendored"
        );
    }

    #[test]
    fn resolve_monaco_dir_falls_through_to_the_dev_path() {
        // No env var set in the test process, and no `monaco/` next to
        // the test binary — resolution ends at `DEV_MONACO_DIR`.
        assert_eq!(resolve_monaco_dir(), Path::new(DEV_MONACO_DIR));
    }
}
