//! rust-analyzer LSP bridge: spawns the server for a project, syncs open
//! documents with full-text updates, drains `publishDiagnostics` into an
//! owned shape the editor can show in Monaco and the Problems tab, and
//! answers the language requests the editor raises — `textDocument/`
//! `completion`, `hover`, `definition`, `references`, `signatureHelp`,
//! `rename` (from Monaco's providers), and `workspace/symbol` (from the
//! project search panel).
//!
//! Requests are fire-and-poll: a `request_*` call returns an id, the
//! reader thread routes the matching reply into an inbox, and
//! [`LspClient::poll_responses`] drains it — nothing blocks a frame
//! waiting on the server.
//!
//! Still later iterations: `prepareRename` validation, formatting, code
//! actions, call hierarchy, and bundling the server binary.
//!
//! Editor-only: the `editor` binary depends on this crate directly, so
//! nothing from the LSP stack reaches a runtime build.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A diagnostic's severity, mapped from LSP's numeric `DiagnosticSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A hard error.
    Error,
    /// A warning.
    Warning,
    /// An informational note.
    Information,
    /// A hint.
    Hint,
}

/// One diagnostic, positions kept 0-based exactly as LSP reports them —
/// the host converts to 1-based for Monaco and the Problems table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    /// How serious the finding is.
    pub severity: Severity,
    /// Human-readable description.
    pub message: String,
    /// Diagnostic code (`E0599`, `unused_variables`, …) if the server
    /// gave one.
    pub code: Option<String>,
    /// 0-based start line.
    pub start_line: u32,
    /// 0-based start column (UTF-16 code units, per LSP).
    pub start_col: u32,
    /// 0-based end line.
    pub end_line: u32,
    /// 0-based end column.
    pub end_col: u32,
}

/// The current diagnostics for one file. rust-analyzer republishes a
/// file's full set each time, so this replaces whatever was shown for
/// `path` before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostics {
    /// Absolute path the diagnostics belong to.
    pub path: PathBuf,
    /// The file's complete current diagnostic set (may be empty — that
    /// means "clear this file").
    pub items: Vec<LspDiagnostic>,
}

/// A half-open range in a document, 0-based, as LSP reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeOut {
    /// 0-based start line.
    pub start_line: u32,
    /// 0-based start column.
    pub start_col: u32,
    /// 0-based end line.
    pub end_line: u32,
    /// 0-based end column.
    pub end_col: u32,
}

/// One completion candidate, flattened from the server's `CompletionItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItemOut {
    /// Text shown in the completion list.
    pub label: String,
    /// LSP `CompletionItemKind` as its raw number (1..=25); `0` if the
    /// server gave none.
    pub kind: u8,
    /// Signature / type detail line, if any.
    pub detail: Option<String>,
    /// Documentation body (Markdown or plain), if any.
    pub documentation: Option<String>,
    /// Text to insert; falls back to `label` when the server gives none.
    pub insert_text: String,
    /// `insert_text` is an LSP snippet (`$0`, `${1:name}` placeholders).
    pub is_snippet: bool,
    /// Server-provided ordering key, if any.
    pub sort_text: Option<String>,
}

/// The result of a completion request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionOut {
    /// Candidates, in the order the server returned them.
    pub items: Vec<CompletionItemOut>,
}

/// The result of a hover request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverOut {
    /// Hover text as Markdown (parts joined, code blocks fenced).
    pub markdown: String,
    /// The source range the hover describes, if the server gave one.
    pub range: Option<RangeOut>,
}

/// One place a definition request resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationOut {
    /// Absolute path of the target file.
    pub path: PathBuf,
    /// 0-based start line.
    pub start_line: u32,
    /// 0-based start column.
    pub start_col: u32,
    /// 0-based end line.
    pub end_line: u32,
    /// 0-based end column.
    pub end_col: u32,
}

/// The result of a go-to-definition request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionOut {
    /// Targets, usually one; empty when the server found nothing.
    pub targets: Vec<LocationOut>,
}

/// The result of a find-references request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferencesOut {
    /// Every reference site the server found (declaration included when
    /// the request asked for it); empty when none.
    pub locations: Vec<LocationOut>,
}

/// One parameter within a [`SignatureInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamInfo {
    /// The parameter's label — always resolved to a string (an offset
    /// pair into the signature label is sliced out here).
    pub label: String,
    /// Documentation for the parameter, if any.
    pub documentation: Option<String>,
}

/// One candidate signature in a [`SignatureHelpOut`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    /// The whole signature rendered as one line.
    pub label: String,
    /// Documentation for the signature, if any.
    pub documentation: Option<String>,
    /// The signature's parameters, in order.
    pub parameters: Vec<ParamInfo>,
}

/// The result of a signature-help request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelpOut {
    /// Candidate signatures; empty means "nothing to show".
    pub signatures: Vec<SignatureInfo>,
    /// Index into `signatures` the server considers active.
    pub active_signature: u32,
    /// Index of the active parameter within the active signature, if the
    /// server gave one.
    pub active_parameter: Option<u32>,
}

/// One text replacement within a file, 0-based as LSP reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEditOut {
    /// The span to replace.
    pub range: RangeOut,
    /// The text to put in its place.
    pub new_text: String,
}

/// Every edit a rename makes to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdits {
    /// Absolute path of the file to edit.
    pub path: PathBuf,
    /// The edits, in the order the server listed them (the applier sorts
    /// them start-descending before splicing).
    pub edits: Vec<TextEditOut>,
}

/// The result of a rename request: a `WorkspaceEdit` flattened to
/// per-file edit lists, with file-create/rename/delete operations and
/// non-`file://` targets dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOut {
    /// One entry per affected file; empty when the server refused the
    /// rename or found nothing.
    pub file_edits: Vec<FileEdits>,
}

/// One match from a `workspace/symbol` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOut {
    /// The symbol's name.
    pub name: String,
    /// LSP `SymbolKind` as its raw number.
    pub kind: u8,
    /// Enclosing scope (`containerName`), if the server gave one.
    pub container: Option<String>,
    /// Absolute path of the file the symbol is in.
    pub path: PathBuf,
    /// 0-based line (0 if the server gave a bare `{uri}` location).
    pub line: u32,
    /// 0-based column.
    pub col: u32,
}

/// The result of a `workspace/symbol` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbolsOut {
    /// Matches, in the order the server returned them; non-`file://`
    /// entries are dropped.
    pub symbols: Vec<SymbolOut>,
}

/// The payload of an [`LspResponse`], one variant per `request_*` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspPayload {
    /// Reply to [`LspClient::request_completion`].
    Completion(CompletionOut),
    /// Reply to [`LspClient::request_hover`].
    Hover(HoverOut),
    /// Reply to [`LspClient::request_definition`].
    Definition(DefinitionOut),
    /// Reply to [`LspClient::request_references`].
    References(ReferencesOut),
    /// Reply to [`LspClient::request_signature_help`].
    SignatureHelp(SignatureHelpOut),
    /// Reply to [`LspClient::request_rename`].
    Rename(RenameOut),
    /// Reply to [`LspClient::request_workspace_symbols`].
    WorkspaceSymbols(WorkspaceSymbolsOut),
}

/// A server reply to one of the `request_*` calls, tagged with the id the
/// caller was handed so it can match it to the pending UI request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspResponse {
    /// The id returned by the `request_*` call.
    pub request_id: i64,
    /// The parsed reply.
    pub payload: LspPayload,
}

/// What kind of reply a still-outstanding request id is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Completion,
    Hover,
    Definition,
    References,
    SignatureHelp,
    Rename,
    WorkspaceSymbol,
}

/// A running rust-analyzer, with the documents it has been told about and
/// the thread draining its output.
pub struct LspClient {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_request_id: i64,
    doc_versions: HashMap<PathBuf, i64>,
    /// Request ids sent but not yet answered, and what each expects.
    /// Shared with the reader thread, which removes an id when its reply
    /// arrives.
    pending: Arc<Mutex<HashMap<i64, PendingKind>>>,
    inbox: Arc<Mutex<Vec<FileDiagnostics>>>,
    responses: Arc<Mutex<Vec<LspResponse>>>,
    alive: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl LspClient {
    /// Starts rust-analyzer with its working directory at `root` (which
    /// must contain a `Cargo.toml`), performs the `initialize` /
    /// `initialized` handshake, and begins reading its output.
    ///
    /// Returns `None` — with a logged reason — if the server binary
    /// can't be found or won't start; the editor stays fully usable
    /// without it.
    pub fn start(root: &Path) -> Option<Self> {
        let program = resolve_server()?;
        let mut child = match Command::new(&program)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(program = %program.display(), error = %err, "could not start rust-analyzer");
                return None;
            }
        };

        let stdin = Arc::new(Mutex::new(child.stdin.take()?));
        let stdout = child.stdout.take()?;

        let inbox: Arc<Mutex<Vec<FileDiagnostics>>> = Arc::new(Mutex::new(Vec::new()));
        let responses: Arc<Mutex<Vec<LspResponse>>> = Arc::new(Mutex::new(Vec::new()));
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let reader = spawn_reader(
            stdout,
            Arc::clone(&inbox),
            Arc::clone(&responses),
            Arc::clone(&pending),
            Arc::clone(&stdin),
            Arc::clone(&alive),
        )
        .ok()?;

        let mut client = Self {
            child,
            stdin,
            next_request_id: 1,
            doc_versions: HashMap::new(),
            pending,
            inbox,
            responses,
            alive,
            reader: Some(reader),
        };

        let root_uri = path_to_uri(root);
        client.request(
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false },
                        "synchronization": { "didSave": false },
                        "completion": {
                            "completionItem": {
                                "snippetSupport": true,
                                "documentationFormat": ["markdown", "plaintext"]
                            },
                            "contextSupport": true
                        },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "definition": { "linkSupport": true }
                    },
                    "workspace": { "workspaceFolders": true, "configuration": true }
                },
                "workspaceFolders": [{ "uri": root_uri, "name": "project" }]
            }),
        );
        client.notify("initialized", serde_json::json!({}));
        tracing::info!(root = %root.display(), "rust-analyzer started");
        Some(client)
    }

    /// Tells the server a document is now open, with its full text.
    pub fn did_open(&mut self, path: &Path, text: &str) {
        let version = 1;
        self.doc_versions.insert(path.to_path_buf(), version);
        let uri = path_to_uri(path);
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "rust",
                    "version": version,
                    "text": text,
                }
            }),
        );
    }

    /// Sends a full-document update for an already-open document. A no-op
    /// if [`LspClient::did_open`] was never called for `path`.
    pub fn did_change(&mut self, path: &Path, text: &str) {
        let Some(version) = self.doc_versions.get_mut(path) else {
            return;
        };
        *version += 1;
        let version = *version;
        let uri = path_to_uri(path);
        self.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        );
    }

    /// Tells the server a document was closed.
    pub fn did_close(&mut self, path: &Path) {
        if self.doc_versions.remove(path).is_none() {
            return;
        }
        let uri = path_to_uri(path);
        self.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        );
    }

    /// Drains every diagnostic update received since the last call.
    pub fn poll_diagnostics(&self) -> Vec<FileDiagnostics> {
        match self.inbox.lock() {
            Ok(mut inbox) => std::mem::take(&mut *inbox),
            Err(_) => Vec::new(),
        }
    }

    /// Drains every reply to a `request_*` call received since the last
    /// call, each tagged with the request id the caller was handed.
    pub fn poll_responses(&self) -> Vec<LspResponse> {
        match self.responses.lock() {
            Ok(mut out) => std::mem::take(&mut *out),
            Err(_) => Vec::new(),
        }
    }

    /// Asks the server for completions at `path` `line`:`col` (both
    /// 0-based). Returns the request id; the reply arrives through
    /// [`LspClient::poll_responses`] tagged with it.
    pub fn request_completion(&mut self, path: &Path, line: u32, col: u32) -> i64 {
        self.text_document_request(
            "textDocument/completion",
            PendingKind::Completion,
            path,
            line,
            col,
        )
    }

    /// Asks the server for hover info at `path` `line`:`col` (0-based).
    /// Returns the request id.
    pub fn request_hover(&mut self, path: &Path, line: u32, col: u32) -> i64 {
        self.text_document_request("textDocument/hover", PendingKind::Hover, path, line, col)
    }

    /// Asks the server to resolve the definition at `path` `line`:`col`
    /// (0-based). Returns the request id.
    pub fn request_definition(&mut self, path: &Path, line: u32, col: u32) -> i64 {
        self.text_document_request(
            "textDocument/definition",
            PendingKind::Definition,
            path,
            line,
            col,
        )
    }

    /// Asks the server for every reference to the symbol at `path`
    /// `line`:`col` (0-based). `include_declaration` adds the symbol's
    /// own declaration to the results. Returns the request id.
    pub fn request_references(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        include_declaration: bool,
    ) -> i64 {
        let id = self.alloc_request_id(PendingKind::References);
        let uri = path_to_uri(path);
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/references",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col },
                "context": { "includeDeclaration": include_declaration }
            }
        }));
        id
    }

    /// Asks the server for signature help at `path` `line`:`col`
    /// (0-based) — the parameter hints shown while typing a call.
    /// Returns the request id.
    pub fn request_signature_help(&mut self, path: &Path, line: u32, col: u32) -> i64 {
        self.text_document_request(
            "textDocument/signatureHelp",
            PendingKind::SignatureHelp,
            path,
            line,
            col,
        )
    }

    /// Asks the server for workspace symbols matching `query` (its own
    /// fuzzy matching — pass the raw text). Returns the request id.
    pub fn request_workspace_symbols(&mut self, query: &str) -> i64 {
        let id = self.alloc_request_id(PendingKind::WorkspaceSymbol);
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "workspace/symbol",
            "params": { "query": query }
        }));
        id
    }

    /// Asks the server to rename the symbol at `path` `line`:`col`
    /// (0-based) to `new_name`. The reply is a `WorkspaceEdit`, flattened
    /// into [`RenameOut`]. Returns the request id.
    pub fn request_rename(&mut self, path: &Path, line: u32, col: u32, new_name: &str) -> i64 {
        let id = self.alloc_request_id(PendingKind::Rename);
        let uri = path_to_uri(path);
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/rename",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col },
                "newName": new_name
            }
        }));
        id
    }

    /// Reserves the next request id and records what reply it expects.
    fn alloc_request_id(&mut self, kind: PendingKind) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, kind);
        }
        id
    }

    fn text_document_request(
        &mut self,
        method: &str,
        kind: PendingKind,
        path: &Path,
        line: u32,
        col: u32,
    ) -> i64 {
        let id = self.alloc_request_id(kind);
        let uri = path_to_uri(path);
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col }
            }
        }));
        id
    }

    fn request(&mut self, method: &str, params: serde_json::Value) {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }));
    }

    fn notify(&self, method: &str, params: serde_json::Value) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params,
        }));
    }

    fn send(&self, message: &serde_json::Value) {
        match self.stdin.lock() {
            Ok(mut stdin) => {
                if let Err(err) = write_message(&mut *stdin, message) {
                    tracing::debug!(error = %err, "rust-analyzer write failed (server gone?)");
                }
            }
            Err(_) => tracing::debug!("rust-analyzer stdin lock poisoned"),
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        // Best-effort graceful shutdown, then make sure it's gone.
        self.request("shutdown", serde_json::Value::Null);
        self.notify("exit", serde_json::Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Finds the rust-analyzer binary, in order: `rustup which
/// rust-analyzer` (active toolchain), `rustup which --toolchain stable
/// rust-analyzer`, any `~/.rustup/toolchains/*/bin/rust-analyzer`, then a
/// bare `rust-analyzer` on `PATH`. Each `rustup which` returns an
/// absolute path to the real binary, so the result is safe to spawn from
/// any working directory.
fn resolve_server() -> Option<PathBuf> {
    let via_rustup = |args: &[&str]| -> Option<PathBuf> {
        let output = Command::new("rustup").args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        path.is_file().then_some(path)
    };

    if let Some(path) = via_rustup(&["which", "rust-analyzer"])
        .or_else(|| via_rustup(&["which", "--toolchain", "stable", "rust-analyzer"]))
    {
        return Some(path);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let toolchains = PathBuf::from(home).join(".rustup/toolchains");
        if let Ok(entries) = std::fs::read_dir(&toolchains) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin/rust-analyzer");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    if Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("rust-analyzer"));
    }

    tracing::warn!("rust-analyzer not found; install with `rustup component add rust-analyzer`");
    None
}

/// Spawns the thread that frames and parses server output until EOF or
/// `alive` is cleared. It sorts each message into one of three buckets:
/// a `publishDiagnostics` notification updates `inbox`; a reply to a
/// tracked request id lands in `responses`; a request *from* the server
/// gets a minimal reply written straight back through `stdin`.
fn spawn_reader(
    stdout: std::process::ChildStdout,
    inbox: Arc<Mutex<Vec<FileDiagnostics>>>,
    responses: Arc<Mutex<Vec<LspResponse>>>,
    pending: Arc<Mutex<HashMap<i64, PendingKind>>>,
    stdin: Arc<Mutex<ChildStdin>>,
    alive: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("studio-lsp-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            while alive.load(Ordering::SeqCst) {
                match read_message(&mut reader) {
                    Ok(Some(message)) => {
                        if let Some(file) = diagnostics_from(&message)
                            && let Ok(mut inbox) = inbox.lock()
                        {
                            // Supersede any pending update for the same file.
                            inbox.retain(|existing| existing.path != file.path);
                            inbox.push(file);
                        } else if let Some(response) = response_from(&message, &pending)
                            && let Ok(mut out) = responses.lock()
                        {
                            out.push(response);
                        } else if let Some(reply) = server_request_reply(&message)
                            && let Ok(mut stdin) = stdin.lock()
                        {
                            let _ = write_message(&mut *stdin, &reply);
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(err) => {
                        tracing::debug!(error = %err, "rust-analyzer read ended");
                        break;
                    }
                }
            }
        })
}

/// Reads one `Content-Length`-framed JSON-RPC message. `Ok(None)` on EOF.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<serde_json::Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }
    let length = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message had no Content-Length",
        )
    })?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// Writes one `Content-Length`-framed JSON-RPC message.
fn write_message(stdin: &mut impl Write, message: &serde_json::Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()
}

/// Pulls a [`FileDiagnostics`] out of a `textDocument/publishDiagnostics`
/// notification; `None` for any other message.
fn diagnostics_from(message: &serde_json::Value) -> Option<FileDiagnostics> {
    if message.get("method")?.as_str()? != "textDocument/publishDiagnostics" {
        return None;
    }
    let params: lsp_types::PublishDiagnosticsParams =
        serde_json::from_value(message.get("params")?.clone()).ok()?;
    let path = uri_to_path(params.uri.as_str())?;
    let items = params.diagnostics.iter().map(convert_diagnostic).collect();
    Some(FileDiagnostics { path, items })
}

fn convert_diagnostic(diagnostic: &lsp_types::Diagnostic) -> LspDiagnostic {
    let severity = match diagnostic.severity {
        Some(s) if s == lsp_types::DiagnosticSeverity::WARNING => Severity::Warning,
        Some(s) if s == lsp_types::DiagnosticSeverity::INFORMATION => Severity::Information,
        Some(s) if s == lsp_types::DiagnosticSeverity::HINT => Severity::Hint,
        _ => Severity::Error,
    };
    let code = diagnostic.code.as_ref().map(|code| match code {
        lsp_types::NumberOrString::Number(number) => number.to_string(),
        lsp_types::NumberOrString::String(text) => text.clone(),
    });
    LspDiagnostic {
        severity,
        message: diagnostic.message.clone(),
        code,
        start_line: diagnostic.range.start.line,
        start_col: diagnostic.range.start.character,
        end_line: diagnostic.range.end.line,
        end_col: diagnostic.range.end.character,
    }
}

/// Pulls a tracked reply out of a JSON-RPC response. A response has an
/// integer `id` and no `method`; the id must still be in `pending`
/// (which this removes). `None` for notifications, server requests, or
/// replies to ids we don't track (`initialize`, `shutdown`).
fn response_from(
    message: &serde_json::Value,
    pending: &Arc<Mutex<HashMap<i64, PendingKind>>>,
) -> Option<LspResponse> {
    if message.get("method").is_some() {
        return None;
    }
    let id = message.get("id")?.as_i64()?;
    let kind = pending.lock().ok()?.remove(&id)?;
    let result = message
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let payload = match kind {
        PendingKind::Completion => LspPayload::Completion(parse_completion(&result)),
        PendingKind::Hover => LspPayload::Hover(parse_hover(&result)),
        PendingKind::Definition => LspPayload::Definition(parse_definition(&result)),
        PendingKind::References => LspPayload::References(parse_references(&result)),
        PendingKind::SignatureHelp => LspPayload::SignatureHelp(parse_signature_help(&result)),
        PendingKind::Rename => LspPayload::Rename(parse_rename(&result)),
        PendingKind::WorkspaceSymbol => {
            LspPayload::WorkspaceSymbols(parse_workspace_symbols(&result))
        }
    };
    Some(LspResponse {
        request_id: id,
        payload,
    })
}

/// Builds a minimal reply to a request *from* the server (a message with
/// both `id` and `method`). rust-analyzer blocks its progress on some of
/// these, so an empty-but-valid answer keeps it moving; `None` if the
/// message isn't a server request.
fn server_request_reply(message: &serde_json::Value) -> Option<serde_json::Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method")?.as_str()?;
    let result = match method {
        // One (null = "use defaults") settings object per requested item.
        "workspace/configuration" => {
            let count = message
                .get("params")
                .and_then(|params| params.get("items"))
                .and_then(|items| items.as_array())
                .map_or(0, Vec::len);
            serde_json::Value::Array(vec![serde_json::Value::Null; count])
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create" => serde_json::Value::Null,
        // Anything else: decline so the server doesn't keep waiting.
        _ => {
            return Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "not handled by studio" }
            }));
        }
    };
    Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Flattens a completion result — a bare `CompletionItem[]` or a
/// `CompletionList { items }` — into [`CompletionOut`].
fn parse_completion(result: &serde_json::Value) -> CompletionOut {
    let empty: [serde_json::Value; 0] = [];
    let items: &[serde_json::Value] = match result {
        serde_json::Value::Array(items) => items.as_slice(),
        serde_json::Value::Object(map) => map
            .get("items")
            .and_then(serde_json::Value::as_array)
            .map_or(&empty[..], Vec::as_slice),
        _ => &empty,
    };
    CompletionOut {
        items: items.iter().map(completion_item_from_json).collect(),
    }
}

fn completion_item_from_json(value: &serde_json::Value) -> CompletionItemOut {
    let label = str_field(value, "label").unwrap_or_default();
    let insert_text = str_field(value, "insertText")
        .or_else(|| {
            value
                .get("textEdit")
                .and_then(|edit| edit.get("newText"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| label.clone());
    CompletionItemOut {
        label,
        kind: value
            .get("kind")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u8,
        detail: str_field(value, "detail"),
        documentation: value.get("documentation").and_then(markup_to_string),
        insert_text,
        is_snippet: value
            .get("insertTextFormat")
            .and_then(serde_json::Value::as_u64)
            == Some(2),
        sort_text: str_field(value, "sortText"),
    }
}

/// Flattens a `Hover` result — its `contents` may be a string, a
/// `MarkedString`, an array of those, or a `MarkupContent` — into
/// [`HoverOut`].
fn parse_hover(result: &serde_json::Value) -> HoverOut {
    let markdown = match result.get("contents") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Object(map)) => map
            .get("value")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(marked_string_to_md)
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    HoverOut {
        markdown,
        range: result.get("range").and_then(range_from_json),
    }
}

fn marked_string_to_md(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    let body = value.get("value")?.as_str()?;
    match value.get("language").and_then(serde_json::Value::as_str) {
        Some(lang) if !lang.is_empty() => Some(format!("```{lang}\n{body}\n```")),
        _ => Some(body.to_owned()),
    }
}

/// Flattens a definition result — `Location`, `Location[]`, or
/// `LocationLink[]` — into [`DefinitionOut`], dropping any entry with a
/// non-`file://` or malformed URI.
fn parse_definition(result: &serde_json::Value) -> DefinitionOut {
    let mut targets = Vec::new();
    match result {
        serde_json::Value::Object(_) => targets.extend(location_from_json(result)),
        serde_json::Value::Array(items) => {
            for item in items {
                targets.extend(location_from_json(item));
            }
        }
        _ => {}
    }
    DefinitionOut { targets }
}

fn location_from_json(value: &serde_json::Value) -> Option<LocationOut> {
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))
        .and_then(serde_json::Value::as_str)?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("targetRange"))
        .and_then(range_from_json)?;
    Some(LocationOut {
        path: uri_to_path(uri)?,
        start_line: range.start_line,
        start_col: range.start_col,
        end_line: range.end_line,
        end_col: range.end_col,
    })
}

fn range_from_json(value: &serde_json::Value) -> Option<RangeOut> {
    let start = value.get("start")?;
    let end = value.get("end")?;
    Some(RangeOut {
        start_line: start.get("line")?.as_u64()? as u32,
        start_col: start.get("character")?.as_u64()? as u32,
        end_line: end.get("line")?.as_u64()? as u32,
        end_col: end.get("character")?.as_u64()? as u32,
    })
}

/// Flattens a references result — `Location[]` or null — into
/// [`ReferencesOut`], dropping any non-`file://` or malformed entry.
fn parse_references(result: &serde_json::Value) -> ReferencesOut {
    let locations = match result {
        serde_json::Value::Array(items) => items.iter().filter_map(location_from_json).collect(),
        _ => Vec::new(),
    };
    ReferencesOut { locations }
}

/// Flattens a `SignatureHelp` result — or null — into
/// [`SignatureHelpOut`].
fn parse_signature_help(result: &serde_json::Value) -> SignatureHelpOut {
    let signatures = result
        .get("signatures")
        .and_then(serde_json::Value::as_array)
        .map(|sigs| sigs.iter().map(signature_from_json).collect())
        .unwrap_or_default();
    SignatureHelpOut {
        signatures,
        active_signature: result
            .get("activeSignature")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        active_parameter: result
            .get("activeParameter")
            .and_then(serde_json::Value::as_u64)
            .map(|index| index as u32),
    }
}

fn signature_from_json(value: &serde_json::Value) -> SignatureInfo {
    let label = value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let parameters = value
        .get("parameters")
        .and_then(serde_json::Value::as_array)
        .map(|params| {
            params
                .iter()
                .map(|param| ParamInfo {
                    label: param_label(param, &label),
                    documentation: param.get("documentation").and_then(markup_to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    SignatureInfo {
        label,
        documentation: value.get("documentation").and_then(markup_to_string),
        parameters,
    }
}

/// A `ParameterInformation.label`: a string as-is, or a `[start, end]`
/// pair of offsets into `signature_label` sliced out by char index
/// (LSP counts UTF-16 code units; char slicing is close enough for a
/// display hint and never panics).
fn param_label(param: &serde_json::Value, signature_label: &str) -> String {
    match param.get("label") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(pair)) => {
            let start = pair
                .first()
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            let end = pair.get(1).and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
            signature_label
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect()
        }
        _ => String::new(),
    }
}

/// Flattens a rename result (`WorkspaceEdit`) — or null — into
/// [`RenameOut`]. Prefers `documentChanges` over `changes`; skips
/// file create/rename/delete operations and non-`file://` targets.
fn parse_rename(result: &serde_json::Value) -> RenameOut {
    let mut file_edits = Vec::new();

    if let Some(changes) = result
        .get("documentChanges")
        .and_then(serde_json::Value::as_array)
    {
        for change in changes {
            // A file create/rename/delete op carries `kind` and no `edits`.
            let Some(edits) = change.get("edits").and_then(serde_json::Value::as_array) else {
                continue;
            };
            let Some(uri) = change
                .get("textDocument")
                .and_then(|doc| doc.get("uri"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            file_edits.extend(file_edits_from(uri, edits));
        }
    } else if let Some(changes) = result.get("changes").and_then(serde_json::Value::as_object) {
        for (uri, edits) in changes {
            if let Some(edits) = edits.as_array() {
                file_edits.extend(file_edits_from(uri, edits));
            }
        }
    }

    RenameOut { file_edits }
}

fn file_edits_from(uri: &str, edits: &[serde_json::Value]) -> Option<FileEdits> {
    let path = uri_to_path(uri)?;
    let edits: Vec<TextEditOut> = edits.iter().filter_map(text_edit_from_json).collect();
    if edits.is_empty() {
        return None;
    }
    Some(FileEdits { path, edits })
}

fn text_edit_from_json(value: &serde_json::Value) -> Option<TextEditOut> {
    Some(TextEditOut {
        range: value.get("range").and_then(range_from_json)?,
        new_text: value.get("newText")?.as_str()?.to_owned(),
    })
}

/// Flattens a `workspace/symbol` result — `SymbolInformation[]` or
/// `WorkspaceSymbol[]` (whose `location` may be a bare `{uri}`), or
/// null — into [`WorkspaceSymbolsOut`], dropping non-`file://` entries.
fn parse_workspace_symbols(result: &serde_json::Value) -> WorkspaceSymbolsOut {
    let symbols = match result {
        serde_json::Value::Array(items) => items.iter().filter_map(symbol_from_json).collect(),
        _ => Vec::new(),
    };
    WorkspaceSymbolsOut { symbols }
}

fn symbol_from_json(value: &serde_json::Value) -> Option<SymbolOut> {
    let location = value.get("location")?;
    let uri = location.get("uri").and_then(serde_json::Value::as_str)?;
    let (line, col) = location
        .get("range")
        .and_then(range_from_json)
        .map_or((0, 0), |range| (range.start_line, range.start_col));
    Some(SymbolOut {
        name: str_field(value, "name")?,
        kind: value
            .get("kind")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u8,
        container: str_field(value, "containerName").filter(|text| !text.is_empty()),
        path: uri_to_path(uri)?,
        line,
        col,
    })
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// A documentation / markup value: a bare string, or `{ value, kind }`.
fn markup_to_string(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    value
        .get("value")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Percent-encodes an absolute path into a `file://` URI (RFC 3986
/// unreserved set plus `/` pass through). A space in the path — e.g.
/// `Game engine` — would otherwise make the URI unparseable.
fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char);
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// Reverses [`path_to_uri`]. `None` if `uri` isn't a `file://` URI or has
/// malformed percent-escapes / non-UTF-8 bytes.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut chars = rest.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hi = hex_value(chars.next()?)?;
            let lo = hex_value(chars.next()?)?;
            bytes.push(hi << 4 | lo);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_round_trips_a_path_with_spaces() {
        let path = Path::new("/Users/me/Game engine/src/main.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///Users/me/Game%20engine/src/main.rs");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn uri_to_path_rejects_non_file_uris() {
        assert_eq!(uri_to_path("http://example.com/x"), None);
        assert_eq!(uri_to_path("file:///bad/%zz"), None);
    }

    #[test]
    fn diagnostics_from_parses_a_publish_notification() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/proj/src/main.rs",
                "diagnostics": [{
                    "range": {
                        "start": { "line": 3, "character": 4 },
                        "end": { "line": 3, "character": 9 }
                    },
                    "severity": 1,
                    "code": "E0599",
                    "message": "no method named `frobnicate`"
                }, {
                    "range": {
                        "start": { "line": 10, "character": 0 },
                        "end": { "line": 10, "character": 7 }
                    },
                    "severity": 2,
                    "message": "unused import"
                }]
            }
        });

        let file = diagnostics_from(&message).expect("a publishDiagnostics message");
        assert_eq!(file.path, PathBuf::from("/tmp/proj/src/main.rs"));
        assert_eq!(file.items.len(), 2);
        assert_eq!(file.items[0].severity, Severity::Error);
        assert_eq!(file.items[0].code.as_deref(), Some("E0599"));
        assert_eq!(file.items[0].start_line, 3);
        assert_eq!(file.items[0].start_col, 4);
        assert_eq!(file.items[1].severity, Severity::Warning);
        assert_eq!(file.items[1].code, None);
    }

    #[test]
    fn diagnostics_from_ignores_other_messages() {
        let message = serde_json::json!({
            "jsonrpc": "2.0", "method": "window/logMessage",
            "params": { "type": 3, "message": "hi" }
        });
        assert!(diagnostics_from(&message).is_none());
    }

    #[test]
    fn framing_round_trips_a_message() {
        let message = serde_json::json!({ "jsonrpc": "2.0", "method": "ping", "params": {} });
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).expect("write");
        let mut reader = std::io::BufReader::new(buffer.as_slice());
        let read = read_message(&mut reader).expect("read").expect("a message");
        assert_eq!(read, message);
    }

    #[test]
    fn response_from_routes_by_pending_kind_and_consumes_the_id() {
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert(7, PendingKind::Hover);

        let message = serde_json::json!({
            "jsonrpc": "2.0", "id": 7,
            "result": { "contents": { "kind": "markdown", "value": "`i32`" } }
        });
        let response = response_from(&message, &pending).expect("a tracked response");
        assert_eq!(response.request_id, 7);
        match response.payload {
            LspPayload::Hover(hover) => assert_eq!(hover.markdown, "`i32`"),
            other => panic!("expected a hover payload, got {other:?}"),
        }
        // The id is consumed, so a duplicate reply is ignored.
        assert!(pending.lock().unwrap().is_empty());
        assert!(response_from(&message, &pending).is_none());
    }

    #[test]
    fn response_from_ignores_untracked_ids_and_notifications() {
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        // Reply to an id we never tracked (e.g. `initialize`).
        assert!(
            response_from(
                &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} }),
                &pending
            )
            .is_none()
        );
        // A notification (has `method`, no `id`).
        assert!(
            response_from(
                &serde_json::json!({ "jsonrpc": "2.0", "method": "x", "params": {} }),
                &pending
            )
            .is_none()
        );
    }

    #[test]
    fn parse_completion_takes_both_list_and_array_shapes() {
        let array = serde_json::json!([
            {
                "label": "push", "kind": 2, "detail": "fn(&mut self, value: T)",
                "insertText": "push(${1:value})$0", "insertTextFormat": 2,
                "sortText": "0001", "documentation": { "kind": "markdown", "value": "Appends." }
            },
            { "label": "len" }
        ]);
        let list = serde_json::json!({ "isIncomplete": true, "items": array });

        for value in [array, list] {
            let out = parse_completion(&value);
            assert_eq!(out.items.len(), 2);
            let push = &out.items[0];
            assert_eq!(push.label, "push");
            assert_eq!(push.kind, 2);
            assert!(push.is_snippet);
            assert_eq!(push.insert_text, "push(${1:value})$0");
            assert_eq!(push.detail.as_deref(), Some("fn(&mut self, value: T)"));
            assert_eq!(push.documentation.as_deref(), Some("Appends."));
            assert_eq!(push.sort_text.as_deref(), Some("0001"));
            // No `insertText`: falls back to the label, not a snippet.
            assert_eq!(out.items[1].insert_text, "len");
            assert!(!out.items[1].is_snippet);
        }

        assert!(parse_completion(&serde_json::Value::Null).items.is_empty());
    }

    #[test]
    fn parse_hover_joins_marked_string_arrays_with_code_fences() {
        let out = parse_hover(&serde_json::json!({
            "contents": [
                { "language": "rust", "value": "fn main()" },
                "The entry point."
            ],
            "range": {
                "start": { "line": 2, "character": 3 },
                "end": { "line": 2, "character": 7 }
            }
        }));
        assert_eq!(out.markdown, "```rust\nfn main()\n```\n\nThe entry point.");
        assert_eq!(
            out.range,
            Some(RangeOut {
                start_line: 2,
                start_col: 3,
                end_line: 2,
                end_col: 7
            })
        );

        // Bare string contents, no range.
        let plain = parse_hover(&serde_json::json!({ "contents": "just text" }));
        assert_eq!(plain.markdown, "just text");
        assert_eq!(plain.range, None);
    }

    #[test]
    fn parse_definition_handles_location_link_and_array_forms() {
        // Single plain Location.
        let single = parse_definition(&serde_json::json!({
            "uri": "file:///tmp/p/src/lib.rs",
            "range": {
                "start": { "line": 10, "character": 0 },
                "end": { "line": 10, "character": 5 }
            }
        }));
        assert_eq!(single.targets.len(), 1);
        assert_eq!(single.targets[0].path, PathBuf::from("/tmp/p/src/lib.rs"));
        assert_eq!(single.targets[0].start_line, 10);

        // Array of LocationLinks, plus one non-file entry that is dropped.
        let links = parse_definition(&serde_json::json!([
            {
                "targetUri": "file:///tmp/p/src/main.rs",
                "targetSelectionRange": {
                    "start": { "line": 1, "character": 4 },
                    "end": { "line": 1, "character": 8 }
                }
            },
            {
                "targetUri": "jar:///nope",
                "targetSelectionRange": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 }
                }
            }
        ]));
        assert_eq!(links.targets.len(), 1);
        assert_eq!(links.targets[0].path, PathBuf::from("/tmp/p/src/main.rs"));
        assert_eq!(links.targets[0].start_col, 4);

        assert!(
            parse_definition(&serde_json::Value::Null)
                .targets
                .is_empty()
        );
    }

    #[test]
    fn parse_references_keeps_file_locations_only() {
        let out = parse_references(&serde_json::json!([
            {
                "uri": "file:///tmp/p/src/a.rs",
                "range": {
                    "start": { "line": 2, "character": 4 },
                    "end": { "line": 2, "character": 9 }
                }
            },
            {
                "uri": "file:///tmp/p/src/b.rs",
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 3 }
                }
            },
            {
                "uri": "jar:///std/lib.rs",
                "range": {
                    "start": { "line": 1, "character": 1 },
                    "end": { "line": 1, "character": 2 }
                }
            }
        ]));
        assert_eq!(out.locations.len(), 2);
        assert_eq!(out.locations[0].path, PathBuf::from("/tmp/p/src/a.rs"));
        assert_eq!(out.locations[1].start_col, 0);
        assert!(
            parse_references(&serde_json::Value::Null)
                .locations
                .is_empty()
        );
    }

    #[test]
    fn parse_signature_help_resolves_string_and_offset_param_labels() {
        let out = parse_signature_help(&serde_json::json!({
            "signatures": [{
                "label": "fn push(&mut self, value: T)",
                "documentation": "Appends an element.",
                "parameters": [
                    { "label": "&mut self" },
                    { "label": [19, 27], "documentation": { "kind": "markdown", "value": "the value" } }
                ]
            }],
            "activeSignature": 0,
            "activeParameter": 1
        }));
        assert_eq!(out.signatures.len(), 1);
        assert_eq!(out.active_signature, 0);
        assert_eq!(out.active_parameter, Some(1));
        let sig = &out.signatures[0];
        assert_eq!(sig.documentation.as_deref(), Some("Appends an element."));
        assert_eq!(sig.parameters[0].label, "&mut self");
        // Offsets 19..27 into the label → "value: T".
        assert_eq!(sig.parameters[1].label, "value: T");
        assert_eq!(
            sig.parameters[1].documentation.as_deref(),
            Some("the value")
        );

        // Null result → empty, no active parameter.
        let empty = parse_signature_help(&serde_json::Value::Null);
        assert!(empty.signatures.is_empty());
        assert_eq!(empty.active_parameter, None);
    }

    #[test]
    fn response_from_routes_references_and_signature_help() {
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert(11, PendingKind::References);
        pending
            .lock()
            .unwrap()
            .insert(12, PendingKind::SignatureHelp);

        let refs = response_from(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 11, "result": [] }),
            &pending,
        )
        .expect("a references response");
        assert!(matches!(refs.payload, LspPayload::References(_)));

        let sig = response_from(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 12, "result": null }),
            &pending,
        )
        .expect("a signature-help response");
        assert!(matches!(sig.payload, LspPayload::SignatureHelp(_)));
    }

    #[test]
    fn response_from_routes_rename() {
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert(20, PendingKind::Rename);
        let rename = response_from(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 20, "result": null }),
            &pending,
        )
        .expect("a rename response");
        assert!(matches!(rename.payload, LspPayload::Rename(_)));
    }

    #[test]
    fn parse_workspace_symbols_handles_both_location_shapes() {
        let out = parse_workspace_symbols(&serde_json::json!([
            {
                "name": "helper", "kind": 12, "containerName": "app::util",
                "location": {
                    "uri": "file:///tmp/p/src/util.rs",
                    "range": {
                        "start": { "line": 4, "character": 3 },
                        "end": { "line": 4, "character": 9 }
                    }
                }
            },
            {
                "name": "Widget", "kind": 23,
                "location": { "uri": "file:///tmp/p/src/lib.rs" }
            },
            {
                "name": "external", "kind": 12,
                "location": { "uri": "jar:///std/lib.rs" }
            }
        ]));
        assert_eq!(out.symbols.len(), 2);
        assert_eq!(out.symbols[0].name, "helper");
        assert_eq!(out.symbols[0].kind, 12);
        assert_eq!(out.symbols[0].container.as_deref(), Some("app::util"));
        assert_eq!(out.symbols[0].path, PathBuf::from("/tmp/p/src/util.rs"));
        assert_eq!((out.symbols[0].line, out.symbols[0].col), (4, 3));
        // Bare `{uri}` location → 0,0 and no container.
        assert_eq!(out.symbols[1].name, "Widget");
        assert_eq!((out.symbols[1].line, out.symbols[1].col), (0, 0));
        assert_eq!(out.symbols[1].container, None);

        assert!(
            parse_workspace_symbols(&serde_json::Value::Null)
                .symbols
                .is_empty()
        );
    }

    #[test]
    fn response_from_routes_workspace_symbols() {
        let pending: Arc<Mutex<HashMap<i64, PendingKind>>> = Arc::new(Mutex::new(HashMap::new()));
        pending
            .lock()
            .unwrap()
            .insert(21, PendingKind::WorkspaceSymbol);
        let response = response_from(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 21, "result": [] }),
            &pending,
        )
        .expect("a workspace-symbols response");
        assert!(matches!(response.payload, LspPayload::WorkspaceSymbols(_)));
    }

    #[test]
    fn parse_rename_reads_both_workspace_edit_shapes() {
        let edit = |line: u32, start: u32, end: u32| {
            serde_json::json!({
                "range": {
                    "start": { "line": line, "character": start },
                    "end": { "line": line, "character": end }
                },
                "newText": "renamed"
            })
        };

        // `documentChanges` form; a CreateFile op and a non-file target
        // are both dropped.
        let doc_changes = parse_rename(&serde_json::json!({
            "documentChanges": [
                {
                    "textDocument": { "uri": "file:///tmp/p/src/a.rs", "version": 3 },
                    "edits": [edit(0, 3, 9), edit(4, 8, 14)]
                },
                { "kind": "create", "uri": "file:///tmp/p/src/new.rs" },
                {
                    "textDocument": { "uri": "jar:///std/lib.rs", "version": null },
                    "edits": [edit(1, 0, 2)]
                }
            ]
        }));
        assert_eq!(doc_changes.file_edits.len(), 1);
        assert_eq!(
            doc_changes.file_edits[0].path,
            PathBuf::from("/tmp/p/src/a.rs")
        );
        assert_eq!(doc_changes.file_edits[0].edits.len(), 2);
        assert_eq!(doc_changes.file_edits[0].edits[0].new_text, "renamed");
        assert_eq!(doc_changes.file_edits[0].edits[1].range.start_line, 4);

        // `changes` form.
        let changes = parse_rename(&serde_json::json!({
            "changes": { "file:///tmp/p/src/b.rs": [edit(2, 4, 10)] }
        }));
        assert_eq!(changes.file_edits.len(), 1);
        assert_eq!(changes.file_edits[0].path, PathBuf::from("/tmp/p/src/b.rs"));

        // Null / refused rename → empty.
        assert!(parse_rename(&serde_json::Value::Null).file_edits.is_empty());
    }

    #[test]
    fn server_request_reply_answers_the_ones_ra_waits_on() {
        let config = server_request_reply(&serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "workspace/configuration",
            "params": { "items": [{ "section": "rust-analyzer" }, { "section": "rust" }] }
        }))
        .expect("a reply");
        assert_eq!(config["result"], serde_json::json!([null, null]));

        let register = server_request_reply(&serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "client/registerCapability", "params": {}
        }))
        .expect("a reply");
        assert_eq!(register["result"], serde_json::Value::Null);

        // Unknown server request: declined with an error, not left hanging.
        let unknown = server_request_reply(&serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "window/showMessageRequest", "params": {}
        }))
        .expect("a reply");
        assert!(unknown.get("error").is_some());

        // A plain response is not a server request.
        assert!(
            server_request_reply(&serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} }))
                .is_none()
        );
    }

    #[test]
    #[ignore = "spawns rust-analyzer and runs cargo check; run locally with --ignored"]
    fn real_server_reports_a_diagnostic_then_shuts_down() {
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!("editor_lsp_it_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"it\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n",
        )
        .expect("Cargo.toml");
        let main_rs = root.join("src/main.rs");
        // Line 0: a helper fn to reference and to get signature help for.
        // Line 2: a type error for the diagnostic assertion.
        // Line 3: a call site (`helper(1)`).
        std::fs::write(
            &main_rs,
            "fn helper(value: i32) -> i32 { value }\n\
             fn main() {\n\
             \x20   let _x: i32 = \"not an int\";\n\
             \x20   let _y = helper(1);\n\
             }\n",
        )
        .expect("main.rs");

        let mut client = LspClient::start(&root).expect("rust-analyzer to start");
        let text = std::fs::read_to_string(&main_rs).expect("read main.rs");
        client.did_open(&main_rs, &text);

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut saw_error = false;
        while Instant::now() < deadline && !saw_error {
            for file in client.poll_diagnostics() {
                if file.path == main_rs && file.items.iter().any(|d| d.severity == Severity::Error)
                {
                    saw_error = true;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            saw_error,
            "expected an error diagnostic for the mismatched type"
        );

        // The server is warm now. Poll one request to completion.
        let wait_for = |client: &LspClient, id: i64, label: &str| -> LspPayload {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                for response in client.poll_responses() {
                    if response.request_id == id {
                        return response.payload;
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            panic!("timed out waiting for the {label} response");
        };

        // Completions where `helper` is being typed on line 3.
        let id = client.request_completion(&main_rs, 3, 13);
        match wait_for(&client, id, "completion") {
            LspPayload::Completion(out) => assert!(!out.items.is_empty(), "empty completion list"),
            other => panic!("expected a completion payload, got {other:?}"),
        }

        // Signature help inside the `helper(` call on line 3.
        let id = client.request_signature_help(&main_rs, 3, 20);
        match wait_for(&client, id, "signature help") {
            LspPayload::SignatureHelp(out) => {
                assert!(!out.signatures.is_empty(), "no signatures for helper()")
            }
            other => panic!("expected a signature-help payload, got {other:?}"),
        }

        // References to `helper`, its declaration on line 0 included.
        let id = client.request_references(&main_rs, 0, 5, true);
        match wait_for(&client, id, "references") {
            LspPayload::References(out) => {
                assert!(!out.locations.is_empty(), "no references to helper")
            }
            other => panic!("expected a references payload, got {other:?}"),
        }

        // Rename `helper` → `renamed`: expect edits for main.rs covering
        // at least the declaration and the call site.
        let id = client.request_rename(&main_rs, 0, 5, "renamed");
        match wait_for(&client, id, "rename") {
            LspPayload::Rename(out) => {
                let for_main = out
                    .file_edits
                    .iter()
                    .find(|entry| entry.path == main_rs)
                    .expect("edits for main.rs");
                assert!(
                    for_main.edits.len() >= 2,
                    "expected at least the decl + call-site edits"
                );
                assert!(for_main.edits.iter().all(|edit| edit.new_text == "renamed"));
            }
            other => panic!("expected a rename payload, got {other:?}"),
        }

        drop(client);
        let _ = std::fs::remove_dir_all(&root);
    }
}
