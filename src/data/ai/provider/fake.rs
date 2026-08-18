//! Deterministic in-process provider used as the default and for tests. It
//! never touches the network, so the whole app works offline with AI "off".

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

use super::{
    AiError, AiErrorKind, CancelFlag, Capabilities, ChatProvider, ChatRequest, ChatRole,
    ChatToolCall, StreamEvent, StreamOutcome, TokenUsage,
};
use crate::data::ai::tool::{TOOL_CREATE_OUTPUT_PROPOSAL, TOOL_READ_SOURCE};

/// How a scripted fake provider behaves across the tool loop.
#[derive(Clone, Debug)]
enum FakeScript {
    None,
    /// Round 1 requests `core.create_output_proposal` with the canned title and
    /// body; the continuation round streams the canned final answer.
    Propose {
        title: String,
        content: String,
        answer: String,
    },
    /// Round 1 requests `core.read_source` for source #1; the continuation
    /// round streams the canned final answer.
    ReadSource { answer: String },
}

/// A scripted chat provider. The default instance answers deterministically
/// from the last user message; `canned` lets tests script chunks, delays,
/// failures and usage; `tool_*` constructors script a bounded tool round.
#[derive(Clone, Debug)]
pub struct FakeProvider {
    chunks: Vec<String>,
    /// Delay between chunks (tests keep this zero for speed).
    delay: std::time::Duration,
    /// Fail with `Protocol` after this many chunks (None = never fail).
    fail_after: Option<usize>,
    script: FakeScript,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            delay: std::time::Duration::ZERO,
            fail_after: None,
            script: FakeScript::None,
        }
    }
}

impl FakeProvider {
    /// A fake provider that replies deterministically to the last user message
    /// (streamed word by word).
    pub fn new() -> Self {
        Self::default()
    }

    /// A scripted fake provider for tests and offline demos.
    pub fn canned(chunks: Vec<&str>, delay_ms: u64, fail_after: Option<usize>) -> Self {
        Self {
            chunks: chunks.into_iter().map(str::to_owned).collect(),
            delay: std::time::Duration::from_millis(delay_ms),
            fail_after,
            script: FakeScript::None,
        }
    }

    /// A fake provider that exercises the bounded tool loop: the first call
    /// requests `core.create_output_proposal` with the given title/body, the
    /// continuation round answers with `answer`.
    pub fn tool_proposal(title: &str, content: &str, answer: &str) -> Self {
        Self {
            chunks: Vec::new(),
            delay: std::time::Duration::ZERO,
            fail_after: None,
            script: FakeScript::Propose {
                title: title.to_owned(),
                content: content.to_owned(),
                answer: answer.to_owned(),
            },
        }
    }

    /// A fake provider that first calls `core.read_source` for source #1 and
    /// answers with `answer` on the continuation round.
    pub fn tool_read_source(answer: &str) -> Self {
        Self {
            chunks: Vec::new(),
            delay: std::time::Duration::ZERO,
            fail_after: None,
            script: FakeScript::ReadSource {
                answer: answer.to_owned(),
            },
        }
    }

    fn reply_for(&self, request: &ChatRequest) -> String {
        let question = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .map(|message| message.content.trim())
            .filter(|content| !content.is_empty())
            .unwrap_or("(no question)");
        format!(
            "This is a reply from the Fake provider (no network).\n\n\
             You asked: {question}\n\n\
             Configure a real provider in Settings to enable model calls."
        )
    }
}

#[async_trait]
impl ChatProvider for FakeProvider {
    fn label(&self) -> String {
        "Fake".to_owned()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tool_calls: true,
        }
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
        events: mpsc::Sender<StreamEvent>,
        cancel: &CancelFlag,
    ) -> Result<StreamOutcome, AiError> {
        // Scripted tool calls only fire when the request actually carries tool
        // definitions (mimicking a real model that cannot call unknown tools).
        // The first call (no tool-role messages yet) emits the scripted tool
        // call; the continuation round streams the canned final answer.
        let continuation = request
            .messages
            .iter()
            .any(|message| message.role == ChatRole::Tool);
        let (full, tool_calls) = match (&self.script, request.tools.is_empty(), continuation) {
            (_, true, _) => (self.reply_for(&request), Vec::new()),
            (FakeScript::None, _, _) => (self.reply_for(&request), Vec::new()),
            (
                FakeScript::Propose {
                    title, content, ..
                },
                _,
                false,
            ) => (
                String::new(),
                vec![ChatToolCall {
                    call_id: "call_propose".to_owned(),
                    fn_name: TOOL_CREATE_OUTPUT_PROPOSAL.to_owned(),
                    arguments: json!({ "title": title, "content": content }),
                }],
            ),
            (FakeScript::ReadSource { .. }, _, false) => (
                String::new(),
                vec![ChatToolCall {
                    call_id: "call_read".to_owned(),
                    fn_name: TOOL_READ_SOURCE.to_owned(),
                    arguments: json!({ "source": 1 }),
                }],
            ),
            (FakeScript::Propose { answer, .. } | FakeScript::ReadSource { answer }, _, true) => {
                (answer.clone(), Vec::new())
            }
        };
        // Preserve the chunk boundaries when scripted; otherwise stream
        // word-by-word so the UI's delta merging is exercised. Tool-call rounds
        // stream nothing (the model only emitted a tool request).
        let chunks: Vec<String> = if tool_calls.is_empty() && !self.chunks.is_empty() {
            self.chunks.clone()
        } else if tool_calls.is_empty() {
            full.split(' ')
                .map(|word| format!("{word} "))
                .collect()
        } else {
            Vec::new()
        };
        let mut output = String::new();
        for (index, chunk) in chunks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(AiError::cancelled());
            }
            if self.fail_after.is_some_and(|limit| index >= limit) {
                return Err(AiError::new(
                    AiErrorKind::Protocol,
                    "scripted fake provider failure",
                ));
            }
            if self.delay > std::time::Duration::ZERO {
                tokio::time::sleep(self.delay).await;
            }
            output.push_str(chunk);
            if events.send(StreamEvent::Delta(chunk.clone())).await.is_err() {
                return Err(AiError::new(
                    AiErrorKind::Cancelled,
                    "event channel closed (turn discarded)",
                ));
            }
        }
        Ok(StreamOutcome {
            content: output,
            usage: TokenUsage {
                input_tokens: Some(12),
                output_tokens: Some(chunks.len() as u32),
            },
            tool_calls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::data::ai::ChatMessage;

    async fn collect(
        provider: &dyn ChatProvider,
        request: ChatRequest,
        cancel: &CancelFlag,
    ) -> (Vec<String>, Result<StreamOutcome, AiError>) {
        // Drain concurrently: a provider may produce more deltas than the
        // channel holds, so a non-reading consumer would deadlock on
        // backpressure.
        let (sender, mut receiver) = mpsc::channel(16);
        let (outcome, deltas) = tokio::join!(
            provider.stream_chat(request, sender, cancel),
            async {
                let mut deltas = Vec::new();
                while let Some(StreamEvent::Delta(delta)) = receiver.recv().await {
                    deltas.push(delta);
                }
                deltas
            }
        );
        (deltas, outcome)
    }

    #[tokio::test]
    async fn fake_provider_streams_deltas_and_returns_outcome() {
        let provider = FakeProvider::canned(vec!["hello ", "world"], 0, None);
        let cancel = CancelFlag::new();
        let request = ChatRequest::new(
            None,
            vec![ChatMessage::user("question?"), ChatMessage::user("hi")],
        );
        let (deltas, outcome) = collect(&provider, request, &cancel).await;
        assert_eq!(deltas, vec!["hello ", "world"]);
        let outcome = outcome.expect("no scripted failure");
        assert_eq!(outcome.content, "hello world");
        assert_eq!(outcome.usage.input_tokens, Some(12));
    }

    #[tokio::test]
    async fn fake_provider_default_answers_deterministically() {
        let provider = FakeProvider::new();
        let cancel = CancelFlag::new();
        let request = ChatRequest::new(None, vec![ChatMessage::user("What is 2+2?")]);
        let (_, outcome) = collect(&provider, request, &cancel).await;
        let content = outcome.expect("default fake never fails").content;
        assert!(content.contains("What is 2+2?"));
        assert!(content.contains("Fake provider"));
    }

    #[tokio::test]
    async fn fake_provider_honours_cancellation() {
        let provider = FakeProvider::canned(vec!["a", "b", "c", "d"], 0, None);
        let cancel = CancelFlag::new();
        cancel.cancel();
        let (deltas, outcome) = collect(&provider, ChatRequest::default(), &cancel).await;
        assert!(deltas.is_empty());
        assert_eq!(outcome.expect_err("cancelled").kind, AiErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn fake_provider_can_fail_mid_stream() {
        let provider = FakeProvider::canned(vec!["a", "b", "c"], 0, Some(2));
        let cancel = CancelFlag::new();
        let (_, outcome) = collect(&provider, ChatRequest::default(), &cancel).await;
        let error = outcome.expect_err("scripted failure after two chunks");
        assert_eq!(error.kind, AiErrorKind::Protocol);
    }

    #[tokio::test]
    async fn scripted_proposal_round_requests_the_tool_then_answers() {
        use crate::data::ai::ToolRegistry;
        let cancel = CancelFlag::new();
        let tools = ToolRegistry::builtins().definitions().to_vec();
        // First call: only a tool request, no text (scripted tool calls only
        // fire when the request actually carries tool definitions).
        let (deltas, first) = collect(
            &FakeProvider::tool_proposal("Draft", "# Draft\n\nBody", "Saved!"),
            ChatRequest::default().with_tools(tools.clone()),
            &cancel,
        )
        .await;
        let first = first.expect("tool round succeeds");
        assert!(deltas.is_empty(), "tool-call rounds stream no text");
        assert!(first.content.is_empty());
        assert_eq!(first.tool_calls.len(), 1);
        assert_eq!(first.tool_calls[0].fn_name, TOOL_CREATE_OUTPUT_PROPOSAL);

        // Continuation round (a tool-role message is present): final answer.
        let mut continuation = ChatRequest::default().with_tools(tools.clone());
        continuation.messages.push(ChatMessage::tool_result("call_propose", "ok"));
        let (_, second) = collect(
            &FakeProvider::tool_proposal("Draft", "# Draft\n\nBody", "Saved!"),
            continuation,
            &cancel,
        )
        .await;
        let second = second.expect("continuation round succeeds");
        assert!(second.tool_calls.is_empty());
        assert!(second.content.contains("Saved!"));
    }
}
