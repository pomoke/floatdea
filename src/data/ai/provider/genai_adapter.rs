//! [`genai`](https://github.com/jeremychone/rust-genai) adapter (locked to the
//! `0.6.x` stable line per `plan_ai.md` §9). This is the only file that touches
//! genai types: the rest of FloatDea sees only the neutral `ChatProvider`
//! interface.

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::{
    AiError, AiErrorKind, CancelFlag, Capabilities, ChatProvider, ChatRequest, ChatRole,
    ChatToolCall, ProviderConfig, ProviderKind, StreamEvent, StreamOutcome, TokenUsage,
};

/// A chat provider backed by a `genai::Client` (OpenAI-compatible endpoints,
/// Ollama, and other adapters genai supports). The client is configured from
/// [`ProviderConfig`]; an optional custom endpoint/auth overrides genai's
/// defaults for that provider family.
pub struct GenaiAdapter {
    client: genai::Client,
    /// The full model spec passed to genai (e.g. "ollama/llama3.2" or the
    /// plain model name for OpenAI-compatible endpoints).
    model: String,
    /// Human-readable label for the UI (provider family + model).
    label: String,
}

impl GenaiAdapter {
    pub fn new(config: &ProviderConfig) -> Result<Self, AiError> {
        let adapter_kind = match config.kind {
            ProviderKind::OpenAiCompatible => genai::adapter::AdapterKind::OpenAI,
            ProviderKind::Ollama => genai::adapter::AdapterKind::Ollama,
            ProviderKind::Fake => {
                return Err(AiError::new(
                    AiErrorKind::UnsupportedCapability,
                    "the fake provider is in-process and has no genai adapter",
                ))
            }
        };
        let model = config.model.trim().to_owned();
        if model.is_empty() {
            return Err(AiError::new(
                AiErrorKind::Protocol,
                "a model name is required for remote providers",
            ));
        }
        let base_url = config.base_url.clone().filter(|url| !url.trim().is_empty());
        let api_key = config.api_key.clone().filter(|key| !key.trim().is_empty());

        let mut builder = genai::Client::builder().with_adapter_kind(adapter_kind);
        if let Some(api_key) = api_key.clone() {
            builder = builder.with_auth_resolver_fn(
                move |_model: genai::ModelIden| -> genai::resolver::Result<Option<genai::resolver::AuthData>> {
                    Ok(Some(genai::resolver::AuthData::from_single(api_key.clone())))
                },
            );
        }
        if let Some(base_url) = base_url {
            builder = builder.with_service_target_resolver_fn(
                move |mut target: genai::ServiceTarget| -> genai::resolver::Result<genai::ServiceTarget> {
                    target.endpoint = genai::resolver::Endpoint::from_owned(base_url.clone());
                    Ok(target)
                },
            );
        }

        Ok(Self {
            client: builder.build(),
            model,
            label: format!("{} · {}", config.kind.label(), config.model),
        })
    }

    /// Builds the genai request and the wire-name mapping for one round.
    ///
    /// Namespaced tool ids such as `core.list_sources` do not match the pattern
    /// OpenAI-compatible endpoints require for `tools[].function.name`
    /// (`^[a-zA-Z0-9_-]+$`), so the adapter sanitizes them on the wire
    /// (`.` → `_`) and keeps a `wire name → canonical id` map to translate the
    /// model's tool calls back to the FloatDea `ToolId` before returning them.
    /// The neutral layer never sees the wire names.
    fn genai_request(
        request: ChatRequest,
    ) -> (genai::chat::ChatRequest, BTreeMap<String, String>) {
        // Canonical ids → wire names (built before the messages so assistant
        // tool-call messages reuse the same encoding).
        let mut canonical_to_wire: BTreeMap<String, String> = BTreeMap::new();
        let mut wire_to_canonical: BTreeMap<String, String> = BTreeMap::new();
        if !request.tools.is_empty() {
            for tool in &request.tools {
                let wire = wire_tool_name(&tool.id);
                canonical_to_wire.insert(tool.id.clone(), wire.clone());
                wire_to_canonical.insert(wire, tool.id.clone());
            }
        }
        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(
                request
                    .tools
                    .into_iter()
                    .map(|tool| {
                        let wire = canonical_to_wire
                            .get(&tool.id)
                            .cloned()
                            .unwrap_or_else(|| wire_tool_name(&tool.id));
                        genai::chat::Tool::new(genai::chat::ToolName::Custom(wire))
                            .with_description(tool.description)
                            .with_schema(tool.schema)
                    })
                    .collect(),
            )
        };
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ChatRole::System => genai::chat::ChatRole::System,
                    ChatRole::User => genai::chat::ChatRole::User,
                    ChatRole::Assistant => genai::chat::ChatRole::Assistant,
                    ChatRole::Tool => genai::chat::ChatRole::Tool,
                };
                // Assistant tool-call messages carry the tool requests
                // (re-encoded to the wire names); tool messages carry one
                // bounded result per call.
                if !message.tool_calls.is_empty() {
                    let calls: Vec<genai::chat::ToolCall> = message
                        .tool_calls
                        .into_iter()
                        .map(|call| genai::chat::ToolCall {
                            call_id: call.call_id,
                            fn_name: canonical_to_wire
                                .get(&call.fn_name)
                                .cloned()
                                .unwrap_or_else(|| wire_tool_name(&call.fn_name)),
                            fn_arguments: call.arguments,
                            thought_signatures: None,
                        })
                        .collect();
                    return genai::chat::ChatMessage::assistant(
                        genai::chat::MessageContent::from_tool_calls(calls),
                    );
                }
                if let Some(call_id) = message.tool_call_id {
                    return genai::chat::ChatMessage::tool(
                        genai::chat::MessageContent::from_tool_responses(vec![
                            genai::chat::ToolResponse::new(call_id, message.content),
                        ]),
                    );
                }
                genai::chat::ChatMessage::new(role, message.content)
            })
            .collect();
        (
            genai::chat::ChatRequest {
                system: request.system,
                messages,
                tools,
                previous_response_id: None,
                store: None,
            },
            wire_to_canonical,
        )
    }
}

#[async_trait]
impl ChatProvider for GenaiAdapter {
    fn label(&self) -> String {
        self.label.clone()
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
        // Capture usage, concatenated text and fully aggregated tool calls so
        // the terminal `StreamEnd` carries the complete round outcome.
        let options = genai::chat::ChatOptions::default()
            .with_capture_usage(true)
            .with_capture_content(true)
            .with_capture_tool_calls(true);
        let (genai_request, wire_to_canonical) = Self::genai_request(request);
        let mut response = self
            .client
            .exec_chat_stream(
                genai::ModelSpec::from_name(self.model.clone()),
                genai_request,
                Some(&options),
            )
            .await
            .map_err(map_error)?;

        let mut content = String::new();
        let mut usage = TokenUsage::default();
        let mut tool_calls = Vec::new();
        while let Some(event) = response.stream.next().await {
            if cancel.is_cancelled() {
                return Err(AiError::cancelled());
            }
            match event.map_err(map_error)? {
                genai::chat::ChatStreamEvent::Chunk(chunk) => {
                    content.push_str(&chunk.content);
                    if events.send(StreamEvent::Delta(chunk.content)).await.is_err() {
                        // The turn was discarded (conversation deleted, window
                        // closed, app shutting down): stop producing output.
                        return Err(AiError::cancelled());
                    }
                }
                genai::chat::ChatStreamEvent::End(mut end) => {
                    if let Some(captured) = end.captured_usage.take() {
                        usage = TokenUsage {
                            input_tokens: captured.prompt_tokens.and_then(|v| u32::try_from(v).ok()),
                            output_tokens: captured
                                .completion_tokens
                                .and_then(|v| u32::try_from(v).ok()),
                        };
                    }
                    // Tool calls are aggregated by genai's inter-stream; map the
                    // fully assembled calls to the neutral representation,
                    // restoring the canonical namespaced id from the wire name.
                    if let Some(calls) = end.captured_into_tool_calls() {
                        tool_calls = calls
                            .into_iter()
                            .map(|call| ChatToolCall {
                                call_id: call.call_id,
                                fn_name: wire_to_canonical
                                    .get(&call.fn_name)
                                    .cloned()
                                    .unwrap_or(call.fn_name),
                                arguments: call.fn_arguments,
                            })
                            .collect();
                    }
                }
                // Per-chunk tool deltas are aggregated at `End`; reasoning and
                // thought-signature chunks are not part of the MVP surface.
                genai::chat::ChatStreamEvent::Start
                | genai::chat::ChatStreamEvent::ReasoningChunk(_)
                | genai::chat::ChatStreamEvent::ThoughtSignatureChunk(_)
                | genai::chat::ChatStreamEvent::ToolCallChunk(_) => {}
            }
        }
        Ok(StreamOutcome {
            content,
            usage,
            tool_calls,
        })
    }
}

/// Sanitizes a namespaced tool id for OpenAI-compatible wire transport:
/// `core.list_sources` → `core_list_sources` (the pattern for
/// `tools[].function.name` is `^[a-zA-Z0-9_-]+$`).
fn wire_tool_name(id: &str) -> String {
    id.replace('.', "_")
}

/// Maps any `genai::Error` to the stable application error categories.
fn map_error(error: genai::Error) -> AiError {
    use genai::Error as E;
    let kind = match &error {
        E::WebAdapterCall { webc_error, .. } => match webc_error {
            genai::webc::Error::ResponseFailedStatus { status, .. } => match status.as_u16() {
                401 | 403 => AiErrorKind::Authentication,
                408 => AiErrorKind::Timeout,
                429 => AiErrorKind::RateLimited,
                400 => AiErrorKind::ContextTooLarge,
                500..=599 => AiErrorKind::ProviderUnavailable,
                _ => AiErrorKind::Protocol,
            },
            genai::webc::Error::Reqwest(error) => {
                if error.is_timeout() {
                    AiErrorKind::Timeout
                } else if error.is_connect() || error.is_request() {
                    AiErrorKind::ProviderUnavailable
                } else {
                    AiErrorKind::Protocol
                }
            }
            _ => AiErrorKind::Protocol,
        },
        E::NoAuthData { .. } | E::NoAuthResolver { .. } | E::RequiresApiKey { .. } => {
            AiErrorKind::Authentication
        }
        _ => AiErrorKind::Protocol,
    };
    AiError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_tool_name_sanitizes_namespaced_ids() {
        assert_eq!(wire_tool_name("core.list_sources"), "core_list_sources");
        assert_eq!(wire_tool_name("core.read_source"), "core_read_source");
        assert_eq!(
            wire_tool_name("core.search_sources"),
            "core_search_sources"
        );
        assert_eq!(
            wire_tool_name("core.create_output_proposal"),
            "core_create_output_proposal"
        );
        // The sanitized name must satisfy the OpenAI-compatible
        // `tools[].function.name` pattern `^[a-zA-Z0-9_-]+$`.
        for name in ["core_list_sources", "core_read_source"] {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name} matches the function-name pattern"
            );
        }
    }
}
