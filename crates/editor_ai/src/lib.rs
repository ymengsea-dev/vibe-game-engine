//! Provider-neutral AI boundary for RustyEngine Studio.
//!
//! This crate deliberately contains no HTTP client and never stores a key in
//! project files. Providers receive a short-lived credential from a
//! [`CredentialStore`] when the editor starts a request.

#[cfg(target_os = "macos")]
use std::process::Command;

/// Intent mode constraining the assistant's available tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AiMode {
    /// Explain and inspect without mutation.
    #[default]
    Ask,
    /// Focus on source files and reviewable patches.
    Code,
    /// Focus on scene entities and transforms.
    Scene,
    /// Focus on diagnostics, logs, and profiling.
    Debug,
    /// Focus on compilation and managed play lifecycle.
    Build,
}

impl AiMode {
    /// Stable toolbar order.
    pub const ALL: [Self; 5] = [Self::Ask, Self::Code, Self::Scene, Self::Debug, Self::Build];

    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Code => "Code",
            Self::Scene => "Scene",
            Self::Debug => "Debug",
            Self::Build => "Build",
        }
    }
}

/// Supported provider families. Adding a provider does not change editor UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProviderKind {
    /// OpenAI-compatible chat/completions endpoint.
    OpenAi,
    /// Anthropic Messages API.
    Anthropic,
    /// A user-configured OpenAI-compatible endpoint.
    Compatible,
}

/// Non-secret provider configuration safe to persist in editor settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig {
    /// Provider family used to select the request adapter.
    pub kind: ProviderKind,
    /// Model identifier selected by the user.
    pub model: String,
    /// Optional custom endpoint for [`ProviderKind::Compatible`].
    pub endpoint: Option<String>,
}

/// Role of a message in a chat transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChatRole {
    /// Provider/system instruction.
    System,
    /// Human-authored prompt.
    User,
    /// Assistant response.
    Assistant,
}

/// One bounded message sent to a provider.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    /// Speaker role.
    pub role: ChatRole,
    /// UTF-8 message content.
    pub content: String,
}

/// Project material explicitly selected for a request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectContext {
    /// Relative project path and its text excerpt.
    pub files: Vec<(String, String)>,
}

/// Deterministic, bounded text index for project-aware context lookup.
#[derive(Debug, Clone, Default)]
pub struct ProjectIndex {
    files: Vec<(String, String)>,
}

impl ProjectIndex {
    /// Maximum indexed files and total source bytes.
    pub const MAX_FILES: usize = 4096;
    /// Total source bytes retained by the index.
    pub const MAX_BYTES: usize = 8 * 1024 * 1024;

    /// Indexes common source/configuration files below `root`.
    pub fn build(root: &std::path::Path) -> Self {
        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            out: &mut Vec<(String, String)>,
            bytes: &mut usize,
        ) {
            if out.len() >= ProjectIndex::MAX_FILES || *bytes >= ProjectIndex::MAX_BYTES {
                return;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || matches!(name.as_ref(), "target" | "assets") {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out, bytes);
                    continue;
                }
                let text_like = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "rs" | "toml" | "ron" | "wgsl" | "glsl" | "md" | "json" | "txt"
                        )
                    });
                if !text_like {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if bytes.saturating_add(content.len()) > ProjectIndex::MAX_BYTES {
                    break;
                }
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                *bytes += content.len();
                out.push((relative.to_string_lossy().replace('\\', "/"), content));
                if out.len() >= ProjectIndex::MAX_FILES {
                    break;
                }
            }
        }
        let mut files = Vec::new();
        let mut bytes = 0;
        walk(root, root, &mut files, &mut bytes);
        Self { files }
    }

    /// Returns indexed files whose content contains `query`.
    pub fn search(&self, query: &str) -> Vec<&str> {
        self.files
            .iter()
            .filter(|(_, content)| content.contains(query))
            .map(|(path, _)| path.as_str())
            .collect()
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }
    /// Whether the index contains no files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl ProjectContext {
    /// Maximum context bytes accepted from one request.
    pub const MAX_BYTES: usize = 256 * 1024;

    /// Builds context while preserving file boundaries and a hard byte cap.
    pub fn from_files<I, P, C>(files: I) -> Self
    where
        I: IntoIterator<Item = (P, C)>,
        P: Into<String>,
        C: Into<String>,
    {
        let mut total: usize = 0;
        let mut selected = Vec::new();
        for (path, content) in files {
            let path = path.into();
            let content = content.into();
            let cost = path.len().saturating_add(content.len());
            if total.saturating_add(cost) > Self::MAX_BYTES {
                break;
            }
            total += cost;
            selected.push((path, content));
        }
        Self { files: selected }
    }
}

/// Input to a streaming chat request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    /// Provider/model selection.
    pub provider: ProviderConfig,
    /// Conversation, oldest message first.
    pub messages: Vec<ChatMessage>,
    /// Explicit project excerpts available to the provider.
    pub context: ProjectContext,
}

/// A piece of a streamed assistant response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatChunk {
    /// Text appended to the assistant response.
    Text(String),
    /// Provider indicated the response is complete.
    Finished,
}

/// Bounded conversation state suitable for an editor panel.
#[derive(Debug, Clone, Default)]
pub struct ChatSession {
    messages: Vec<ChatMessage>,
    assistant_draft: String,
}

impl ChatSession {
    /// Maximum retained transcript bytes.
    pub const MAX_TRANSCRIPT_BYTES: usize = 128 * 1024;

    /// Starts a user turn and clears any unfinished assistant draft.
    pub fn submit_user(&mut self, content: impl Into<String>) {
        self.assistant_draft.clear();
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: content.into(),
        });
        self.trim();
    }

    /// Accepts one streamed provider chunk.
    pub fn accept(&mut self, chunk: ChatChunk) {
        match chunk {
            ChatChunk::Text(text) => self.assistant_draft.push_str(&text),
            ChatChunk::Finished => {
                if !self.assistant_draft.is_empty() {
                    self.messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: std::mem::take(&mut self.assistant_draft),
                    });
                    self.trim();
                }
            }
        }
        if self.assistant_draft.len() > Self::MAX_TRANSCRIPT_BYTES {
            self.assistant_draft.truncate(Self::MAX_TRANSCRIPT_BYTES);
        }
    }

    /// Completed messages in chronological order.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Current assistant text while a response is streaming.
    pub fn assistant_draft(&self) -> &str {
        &self.assistant_draft
    }

    fn trim(&mut self) {
        let mut total: usize = self
            .messages
            .iter()
            .map(|message| message.content.len())
            .sum();
        while total > Self::MAX_TRANSCRIPT_BYTES && !self.messages.is_empty() {
            total = total.saturating_sub(self.messages[0].content.len());
            self.messages.remove(0);
        }
    }
}

/// Provider-independent streaming interface.
pub trait ChatProvider {
    /// Streams chunks synchronously; a real adapter may perform I/O on its
    /// own worker thread, while the editor remains responsive.
    fn stream_chat(
        &mut self,
        request: &ChatRequest,
        emit: &mut dyn FnMut(ChatChunk),
    ) -> Result<(), CredentialError>;
}

/// Credential lookup failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    /// No credential was found for the requested service.
    #[error("no credential configured for {0}")]
    Missing(String),
    /// The platform credential helper could not be invoked.
    #[error("credential helper failed: {0}")]
    Helper(String),
}

/// Narrow boundary around OS-backed secret storage.
pub trait CredentialStore {
    /// Reads a secret for `service`, without persisting it in the project.
    fn read(&self, service: &str) -> Result<String, CredentialError>;
}

/// Uses the native macOS Keychain command-line interface.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsCredentialStore;

impl CredentialStore for OsCredentialStore {
    fn read(&self, service: &str) -> Result<String, CredentialError> {
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("security")
                .args(["find-generic-password", "-s", service, "-w"])
                .output()
                .map_err(|error| CredentialError::Helper(error.to_string()))?;
            if output.status.success() {
                let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !value.is_empty() {
                    return Ok(value);
                }
            }
        }
        Err(CredentialError::Missing(service.to_owned()))
    }
}

/// In-memory store used by tests and embedding applications.
#[derive(Debug, Default, Clone)]
pub struct MemoryCredentialStore(std::collections::BTreeMap<String, String>);

impl MemoryCredentialStore {
    /// Adds or replaces a test credential.
    pub fn insert(&mut self, service: impl Into<String>, secret: impl Into<String>) {
        self.0.insert(service.into(), secret.into());
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn read(&self, service: &str) -> Result<String, CredentialError> {
        self.0
            .get(service)
            .cloned()
            .ok_or_else(|| CredentialError::Missing(service.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_round_trips_without_a_secret() {
        let config = ProviderConfig {
            kind: ProviderKind::OpenAi,
            model: "gpt-5".into(),
            endpoint: None,
        };
        let encoded = ron::to_string(&config).expect("config serializes");
        assert!(!encoded.contains("key"));
        assert_eq!(ron::from_str::<ProviderConfig>(&encoded).unwrap(), config);
    }

    #[test]
    fn memory_store_is_explicit_and_missing_is_safe() {
        let mut store = MemoryCredentialStore::default();
        store.insert("openai", "test-secret");
        assert_eq!(store.read("openai").unwrap(), "test-secret");
        assert_eq!(
            store.read("missing"),
            Err(CredentialError::Missing("missing".into()))
        );
    }

    #[test]
    fn project_context_stops_at_a_hard_byte_limit() {
        let context = ProjectContext::from_files([
            ("src/a.rs".to_owned(), "a".repeat(ProjectContext::MAX_BYTES)),
            ("src/b.rs".to_owned(), "never included".to_owned()),
        ]);
        assert_eq!(context.files.len(), 0);
    }

    #[test]
    fn chat_request_keeps_messages_and_context_separate() {
        let request = ChatRequest {
            provider: ProviderConfig {
                kind: ProviderKind::Compatible,
                model: "local".into(),
                endpoint: Some("http://localhost".into()),
            },
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "hello".into(),
            }],
            context: ProjectContext::from_files([("src/lib.rs", "pub struct Player;")]),
        };
        assert_eq!(request.messages[0].role, ChatRole::User);
        assert_eq!(request.context.files[0].0, "src/lib.rs");
    }

    #[test]
    fn chat_session_accumulates_streamed_chunks_and_bounds_history() {
        let mut session = ChatSession::default();
        session.submit_user("hello");
        session.accept(ChatChunk::Text("hel".into()));
        session.accept(ChatChunk::Text("lo".into()));
        assert_eq!(session.assistant_draft(), "hello");
        session.accept(ChatChunk::Finished);
        assert_eq!(session.messages().len(), 2);
        assert_eq!(session.messages()[1].role, ChatRole::Assistant);

        session.submit_user("x".repeat(ChatSession::MAX_TRANSCRIPT_BYTES + 1));
        assert!(
            session
                .messages()
                .iter()
                .map(|m| m.content.len())
                .sum::<usize>()
                <= ChatSession::MAX_TRANSCRIPT_BYTES
        );
    }

    #[test]
    fn ai_modes_have_unique_labels_and_a_safe_default() {
        assert_eq!(AiMode::default(), AiMode::Ask);
        let labels: Vec<_> = AiMode::ALL.iter().map(|mode| mode.label()).collect();
        assert_eq!(labels.len(), 5);
        assert!(labels.iter().all(|label| !label.is_empty()));
        assert!(labels.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn project_index_is_deterministic_and_skips_build_assets() {
        let root = std::env::temp_dir().join(format!("editor-ai-index-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub struct Player;\n").unwrap();
        std::fs::write(root.join("target/generated.rs"), "Player\n").unwrap();
        let index = ProjectIndex::build(&root);
        assert_eq!(index.len(), 1);
        assert_eq!(index.search("Player"), vec!["src/lib.rs"]);
        let _ = std::fs::remove_dir_all(root);
    }
}
