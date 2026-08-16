//! Provider-neutral AI model interface.
//!
//! Everything downstream (UI, Conversation sidecar, workspace) only sees the
//! types in this module. Concrete SDK types (`genai`, provider HTTP errors,
//! tool protocols) are confined to the adapters below and never leak into
//! storage or the UI. A provider only receives the bounded request built by
//! the conversation layer: it has no access to the workspace, the file system
//! or arbitrary network tools.

mod fake;
mod genai_adapter;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use fake::FakeProvider;
pub use genai_adapter::GenaiAdapter;

/// Which provider family is configured. The UI and the conversation layer only
/// ever depend on this neutral enum; each family maps to one `ChatProvider`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Deterministic in-process provider used for tests and as the default
    /// when no remote service is configured. Never makes network requests.
    #[default]
    Fake,
    /// OpenAI-compatible chat endpoint (OpenAI, DeepSeek, Groq, …).
    OpenAiCompatible,
    /// Ollama local model service.
    Ollama,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Fake => "Fake",
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
            ProviderKind::Ollama => "Ollama",
        }
    }
}

/// Stable error categories mapped from any concrete provider failure.
/// Provider-specific detail is preserved in the message, never in the category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiErrorKind {
    /// AI is disabled; no network request is allowed.
    Disabled,
    /// Missing/invalid credentials.
    Authentication,
    /// Rate limited by the provider.
    RateLimited,
    /// The request timed out.
    Timeout,
    /// The context exceeds the provider's limit.
    ContextTooLarge,
    /// The provider service is unreachable (offline, DNS, connection refused).
    ProviderUnavailable,
    /// The provider does not support the requested capability.
    UnsupportedCapability,
    /// Protocol-level failure (bad payload, unexpected response).
    Protocol,
    /// The turn was cancelled by the user or the app.
    Cancelled,
}

/// A stable provider error with a human-readable detail message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiError {
    pub kind: AiErrorKind,
    pub message: String,
}

impl AiError {
    pub fn new(kind: AiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self::new(AiErrorKind::Cancelled, "the turn was cancelled")
    }

    pub fn disabled() -> Self {
        Self::new(AiErrorKind::Disabled, "AI is disabled")
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", error_kind_label(self.kind), self.message)
    }
}

impl std::error::Error for AiError {}

fn error_kind_label(kind: AiErrorKind) -> &'static str {
    match kind {
        AiErrorKind::Disabled => "AI disabled",
        AiErrorKind::Authentication => "authentication failed",
        AiErrorKind::RateLimited => "rate limited",
        AiErrorKind::Timeout => "timed out",
        AiErrorKind::ContextTooLarge => "context too large",
        AiErrorKind::ProviderUnavailable => "provider unavailable",
        AiErrorKind::UnsupportedCapability => "unsupported capability",
        AiErrorKind::Protocol => "protocol error",
        AiErrorKind::Cancelled => "cancelled",
    }
}

/// Role of a message inside a bounded chat request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// One message of a bounded chat request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ChatRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }
}

/// The bounded request the conversation layer builds for one turn. It contains
/// only the conversation history and the sources bound to the current turn —
/// never the whole workspace.
#[derive(Clone, Debug, Default)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn new(system: Option<String>, messages: Vec<ChatMessage>) -> Self {
        Self { system, messages }
    }
}

/// Token usage reported by a provider when available. Missing values stay
/// `None`; the UI must never guess costs from absent usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// A single streaming text delta delivered through the events channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Delta(String),
}

/// The completed result of a streaming turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamOutcome {
    pub content: String,
    pub usage: TokenUsage,
}

/// Per-provider capability matrix. MVP only needs streaming; missing features
/// must degrade explicitly in the UI instead of being faked.
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub streaming: bool,
}

/// Shared cancellation flag for one turn task. Setting it tells the provider
/// to stop producing deltas; the worker additionally aborts the underlying
/// task to free the connection promptly.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Provider-neutral chat interface. Implementations may be in-process (fake)
/// or talk to a remote/local model service.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Human-readable provider label (e.g. "Fake" or "Ollama · llama3.2").
    fn label(&self) -> String;

    fn capabilities(&self) -> Capabilities;

    /// Runs one bounded chat request, streaming text deltas into `events` and
    /// returning the completed outcome. Must yield `Done`-equivalent state
    /// exactly once (via the returned `StreamOutcome`) and must stop producing
    /// deltas once `cancel` is set.
    async fn stream_chat(
        &self,
        request: ChatRequest,
        events: mpsc::Sender<StreamEvent>,
        cancel: &CancelFlag,
    ) -> Result<StreamOutcome, AiError>;
}

/// Configuration used to build the concrete provider for the current settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Model name as shown to the user (e.g. "gpt-4o-mini", "llama3.2").
    pub model: String,
    /// Optional custom base URL for OpenAI-compatible endpoints.
    pub base_url: Option<String>,
    /// Optional API key. When absent, the adapter falls back to environment
    /// variables / provider defaults.
    pub api_key: Option<String>,
}

/// Builds the provider matching `config`. Never makes network requests.
pub fn build_provider(config: &ProviderConfig) -> Result<Box<dyn ChatProvider>, AiError> {
    match config.kind {
        ProviderKind::Fake => Ok(Box::new(FakeProvider::default())),
        ProviderKind::OpenAiCompatible | ProviderKind::Ollama => {
            Ok(Box::new(GenaiAdapter::new(config)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fake_provider_requires_no_network() {
        let provider = build_provider(&ProviderConfig {
            kind: ProviderKind::Fake,
            ..ProviderConfig::default()
        })
        .expect("fake provider builds");
        assert!(provider.capabilities().streaming);
        assert!(provider.label().contains("Fake"));
    }
}
