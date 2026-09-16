//! Auditable, provider-independent Studio tool schema.
//!
//! This crate only defines requests and results. Execution belongs to the
//! editor host, where every mutating request can use the existing command and
//! undo layer and later permission checks.

use std::path::{Path, PathBuf};

/// Initial read/edit/build tool surface shared by humans and AI.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ToolRequest {
    /// Return the current primary and secondary selection.
    GetSelection,
    /// Read a UTF-8 project file.
    ReadFile {
        /// Project-relative file path.
        path: PathBuf,
    },
    /// Search text files under the project root.
    SearchCode {
        /// Text to search for.
        query: String,
    },
    /// Propose a reviewable code patch; does not write anything.
    ProposePatch {
        /// Project-relative file path.
        path: PathBuf,
        /// Replacement file contents.
        replacement: String,
    },
    /// Create an entity through the editor command/undo layer.
    CreateEntity {
        /// Initial entity name.
        name: String,
    },
    /// Set a selected entity's transform through the editor command/undo layer.
    SetTransform {
        /// Stable entity id.
        entity: u64,
        /// World-space translation.
        translation: [f32; 3],
    },
    /// Compile the selected project configuration.
    CompileProject,
    /// Return current compiler/editor diagnostics.
    GetDiagnostics,
    /// Start the selected game configuration.
    RunGame,
    /// Stop the managed game process.
    StopGame,
    /// Return bounded output from the managed game process.
    GetRuntimeLogs,
}

/// Permission tier required by a tool action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// Read files and editor state inside the project.
    ReadProject,
    /// Modify source code after review.
    EditCode,
    /// Modify scene state through undoable commands.
    EditScene,
    /// Build or run the project.
    BuildRun,
    /// Execute shell commands (never granted by default).
    Shell,
    /// Access external network services (never granted by default).
    Network,
    /// Read secrets (never exposed to model context).
    Secrets,
}

impl ToolRequest {
    /// Required tier for this request.
    pub const fn permission(&self) -> Permission {
        match self {
            Self::GetSelection
            | Self::ReadFile { .. }
            | Self::SearchCode { .. }
            | Self::GetDiagnostics
            | Self::GetRuntimeLogs => Permission::ReadProject,
            Self::ProposePatch { .. } => Permission::EditCode,
            Self::CreateEntity { .. } | Self::SetTransform { .. } => Permission::EditScene,
            Self::CompileProject | Self::RunGame | Self::StopGame => Permission::BuildRun,
        }
    }
}

/// Explicit grants for one editor session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionPolicy {
    grants: [bool; 7],
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        let mut grants = [false; 7];
        grants[Permission::ReadProject as usize] = true;
        grants[Permission::BuildRun as usize] = true;
        Self { grants }
    }
}

impl PermissionPolicy {
    /// Grants one tier for the current session.
    pub fn allow(mut self, permission: Permission) -> Self {
        self.grants[permission as usize] = true;
        self
    }

    /// Returns whether a request is currently authorized.
    pub fn permits(&self, request: &ToolRequest) -> bool {
        self.grants[request.permission() as usize]
    }
}

/// Result envelope returned by a tool host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ToolResult {
    /// Structured JSON-safe data.
    Data(serde_json::Value),
    /// A proposed patch awaiting explicit approval.
    Patch {
        /// Project-relative file path.
        path: PathBuf,
        /// Proposed replacement file contents.
        replacement: String,
    },
    /// Human-readable success status.
    Accepted(String),
}

/// Tool validation failure before any side effect occurs.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    /// A path escaped the active project root.
    #[error("path escapes project root: {0}")]
    OutsideProject(PathBuf),
    /// Empty or oversized user input.
    #[error("invalid tool input: {0}")]
    InvalidInput(String),
}

/// Maximum search query size accepted from a tool caller.
pub const MAX_QUERY_BYTES: usize = 4 * 1024;
/// Maximum runtime log bytes returned to a tool caller.
pub const MAX_RUNTIME_LOG_BYTES: usize = 64 * 1024;
/// Maximum action records retained in one editor session.
pub const MAX_ACTIONS: usize = 256;
/// Maximum JSON-RPC frame accepted from an external tool client.
pub const MAX_RPC_FRAME_BYTES: usize = 256 * 1024;

/// JSON-RPC-style request envelope for MCP/automation adapters.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RpcRequest {
    /// Caller correlation id.
    pub id: u64,
    /// Tool method name.
    pub method: String,
    /// JSON arguments, validated by the host before execution.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// JSON-RPC-style response envelope.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RpcResponse {
    /// Correlation id from the request.
    pub id: u64,
    /// Successful result, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error object, if the host rejected the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Structured protocol error safe to show to a client.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RpcError {
    /// Stable error code.
    pub code: i32,
    /// Human-readable message.
    pub message: String,
}

/// Parses one bounded JSON-RPC frame.
pub fn parse_rpc_request(frame: &str) -> Result<RpcRequest, ToolError> {
    if frame.len() > MAX_RPC_FRAME_BYTES {
        return Err(ToolError::InvalidInput("RPC frame is too large".into()));
    }
    serde_json::from_str(frame)
        .map_err(|error| ToolError::InvalidInput(format!("invalid RPC request: {error}")))
}

/// Encodes one response and enforces the same frame bound.
pub fn encode_rpc_response(response: &RpcResponse) -> Result<String, ToolError> {
    let encoded = serde_json::to_string(response).map_err(|error| {
        ToolError::InvalidInput(format!("could not encode RPC response: {error}"))
    })?;
    if encoded.len() > MAX_RPC_FRAME_BYTES {
        return Err(ToolError::InvalidInput("RPC response is too large".into()));
    }
    Ok(encoded)
}

/// Lifecycle state of a planned tool action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ActionStatus {
    /// Waiting for approval or execution.
    Pending,
    /// Executed successfully.
    Completed,
    /// Refused or failed without applying a change.
    Rejected,
}

/// One human-readable, serializable action-log entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionEntry {
    /// Monotonic session-local id.
    pub id: u64,
    /// Tool request summary.
    pub action: String,
    /// Current lifecycle state.
    pub status: ActionStatus,
}

/// Bounded plan/action log shared by AI and automation hosts.
#[derive(Debug, Clone, Default)]
pub struct ActionLog {
    next_id: u64,
    entries: std::collections::VecDeque<ActionEntry>,
}

impl ActionLog {
    /// Adds a pending action and returns its id.
    pub fn plan(&mut self, action: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        if self.entries.len() == MAX_ACTIONS {
            self.entries.pop_front();
        }
        self.entries.push_back(ActionEntry {
            id,
            action: action.into(),
            status: ActionStatus::Pending,
        });
        id
    }

    /// Updates an existing action; unknown ids are ignored safely.
    pub fn set_status(&mut self, id: u64, status: ActionStatus) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.status = status;
        }
    }

    /// Current entries, oldest first.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &ActionEntry> {
        self.entries.iter()
    }
}

/// Host interface implemented by the editor binary's existing build/play
/// lifecycle. AI and human commands can use the same implementation.
pub trait BuildRunHost {
    /// Compile the active project/configuration.
    fn compile_project(&mut self) -> Result<ToolResult, ToolError>;
    /// Read current diagnostics.
    fn get_diagnostics(&self) -> Result<ToolResult, ToolError>;
    /// Start the managed game process.
    fn run_game(&mut self) -> Result<ToolResult, ToolError>;
    /// Stop the managed game process.
    fn stop_game(&mut self) -> Result<ToolResult, ToolError>;
    /// Read bounded runtime output.
    fn get_runtime_logs(&self) -> Result<ToolResult, ToolError>;
}

/// Dispatches build/run requests to the editor-owned host.
pub fn dispatch_build_run(
    host: &mut impl BuildRunHost,
    request: &ToolRequest,
) -> Result<ToolResult, ToolError> {
    match request {
        ToolRequest::CompileProject => host.compile_project(),
        ToolRequest::GetDiagnostics => host.get_diagnostics(),
        ToolRequest::RunGame => host.run_game(),
        ToolRequest::StopGame => host.stop_game(),
        ToolRequest::GetRuntimeLogs => host.get_runtime_logs(),
        _ => Err(ToolError::InvalidInput("request is not build/run".into())),
    }
}
/// Maximum source size accepted for one reviewable patch.
pub const MAX_PATCH_BYTES: usize = 512 * 1024;

/// A reviewable code edit. The original is retained so the UI can show a
/// diff and verify that the file did not change before approval.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PatchProposal {
    /// Project-relative target path.
    pub path: PathBuf,
    /// Bytes read when the proposal was created.
    pub original: String,
    /// Replacement bytes awaiting approval.
    pub replacement: String,
}

/// Failure while creating or applying a patch proposal.
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// Path or content failed tool validation.
    #[error("invalid patch: {0}")]
    Invalid(#[from] ToolError),
    /// Reading or writing the target failed.
    #[error("patch I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The file changed since the proposal was created.
    #[error("patch target changed since review began")]
    Stale,
    /// The caller attempted to write without explicit approval.
    #[error("patch requires explicit approval")]
    NotApproved,
}

/// Creates a bounded proposal from a project-relative file.
pub fn propose_patch(
    root: &Path,
    path: &Path,
    replacement: String,
) -> Result<PatchProposal, PatchError> {
    let scoped = scoped_path(root, path)?;
    if replacement.len() > MAX_PATCH_BYTES {
        return Err(PatchError::Invalid(ToolError::InvalidInput(
            "replacement is too large".into(),
        )));
    }
    let original = std::fs::read_to_string(&scoped)?;
    if original.len() > MAX_PATCH_BYTES {
        return Err(PatchError::Invalid(ToolError::InvalidInput(
            "source is too large".into(),
        )));
    }
    Ok(PatchProposal {
        path: path.to_owned(),
        original,
        replacement,
    })
}

/// Applies exactly the reviewed proposal after checking the file is unchanged.
pub fn apply_approved_patch(
    root: &Path,
    proposal: &PatchProposal,
    approved: bool,
) -> Result<(), PatchError> {
    if !approved {
        return Err(PatchError::NotApproved);
    }
    let scoped = scoped_path(root, &proposal.path)?;
    let current = std::fs::read_to_string(&scoped)?;
    if current != proposal.original {
        return Err(PatchError::Stale);
    }
    let temp = scoped.with_extension("vge-patch-tmp");
    std::fs::write(&temp, &proposal.replacement)?;
    std::fs::rename(temp, scoped)?;
    Ok(())
}

/// Resolves a project-relative path without allowing traversal or absolute
/// paths. The caller still decides whether the operation is read or write.
pub fn scoped_path(root: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(ToolError::OutsideProject(relative.to_owned()));
    }
    Ok(root.join(relative))
}

/// Validates a search query before scanning project files.
pub fn validate_query(query: &str) -> Result<(), ToolError> {
    if query.trim().is_empty() {
        return Err(ToolError::InvalidInput("query must not be empty".into()));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(ToolError::InvalidInput("query is too large".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_paths_reject_absolute_and_parent_traversal() {
        let root = Path::new("/project");
        assert!(scoped_path(root, Path::new("src/main.rs")).is_ok());
        assert!(matches!(
            scoped_path(root, Path::new("../secret")),
            Err(ToolError::OutsideProject(_))
        ));
        assert!(matches!(
            scoped_path(root, Path::new("/tmp/secret")),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn query_validation_is_bounded() {
        assert!(validate_query("Transform").is_ok());
        assert!(validate_query(" ").is_err());
        assert!(validate_query(&"x".repeat(MAX_QUERY_BYTES + 1)).is_err());
    }

    #[test]
    fn mutating_requests_are_serializable_audit_records() {
        let request = ToolRequest::SetTransform {
            entity: 7,
            translation: [1.0, 2.0, 3.0],
        };
        let encoded = ron::to_string(&request).expect("request serializes");
        assert!(encoded.contains("SetTransform"));
    }

    #[test]
    fn patch_requires_approval_and_rejects_stale_sources() {
        let root = std::env::temp_dir().join(format!("editor-tools-patch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let proposal = propose_patch(
            &root,
            Path::new("main.rs"),
            "fn main() { println!(\"ok\"); }\n".into(),
        )
        .unwrap();
        assert!(matches!(
            apply_approved_patch(&root, &proposal, false),
            Err(PatchError::NotApproved)
        ));
        std::fs::write(&path, "changed\n").unwrap();
        assert!(matches!(
            apply_approved_patch(&root, &proposal, true),
            Err(PatchError::Stale)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    struct MockBuildHost;

    impl BuildRunHost for MockBuildHost {
        fn compile_project(&mut self) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::Accepted("compiled".into()))
        }
        fn get_diagnostics(&self) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::Data(serde_json::json!([])))
        }
        fn run_game(&mut self) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::Accepted("running".into()))
        }
        fn stop_game(&mut self) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::Accepted("stopped".into()))
        }
        fn get_runtime_logs(&self) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::Accepted("log".into()))
        }
    }

    #[test]
    fn build_run_requests_share_one_host_dispatch() {
        let mut host = MockBuildHost;
        for request in [
            ToolRequest::CompileProject,
            ToolRequest::GetDiagnostics,
            ToolRequest::RunGame,
            ToolRequest::StopGame,
            ToolRequest::GetRuntimeLogs,
        ] {
            assert!(dispatch_build_run(&mut host, &request).is_ok());
        }
        assert!(dispatch_build_run(&mut host, &ToolRequest::GetSelection).is_err());
    }

    #[test]
    fn policy_allows_safe_reads_and_denies_mutation_by_default() {
        let policy = PermissionPolicy::default();
        assert!(policy.permits(&ToolRequest::GetSelection));
        assert!(policy.permits(&ToolRequest::CompileProject));
        assert!(!policy.permits(&ToolRequest::CreateEntity {
            name: "Cube".into()
        }));
        let approved = policy.allow(Permission::EditScene);
        assert!(approved.permits(&ToolRequest::CreateEntity {
            name: "Cube".into()
        }));
        assert!(!approved.grants[Permission::Network as usize]);
    }

    #[test]
    fn action_log_is_bounded_and_tracks_status() {
        let mut log = ActionLog::default();
        let first = log.plan("create_entity");
        log.set_status(first, ActionStatus::Completed);
        for index in 0..(MAX_ACTIONS + 2) {
            log.plan(format!("action-{index}"));
        }
        assert_eq!(log.entries().count(), MAX_ACTIONS);
        assert!(log.entries().all(|entry| entry.id != first));
    }

    #[test]
    fn rpc_frames_round_trip_and_reject_oversized_payloads() {
        let request = RpcRequest {
            id: 4,
            method: "get_selection".into(),
            params: serde_json::json!({}),
        };
        let frame = serde_json::to_string(&request).unwrap();
        assert_eq!(parse_rpc_request(&frame).unwrap(), request);
        assert!(parse_rpc_request(&"x".repeat(MAX_RPC_FRAME_BYTES + 1)).is_err());
        let response = RpcResponse {
            id: 4,
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        assert!(encode_rpc_response(&response).unwrap().contains("\"id\":4"));
    }
}
