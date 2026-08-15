//! [`genai`](https://github.com/jeremychone/rust-genai) adapter (locked to the
//! `0.6.x` stable line per `plan_ai.md` §9). This is the only file that touches
//! genai types: the rest of FloatDea sees only the neutral `ChatProvider`
//! interface.

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::{
    AiError, AiErrorKind, CancelFlag, Capabilities, ChatProvider, ChatRequest, ChatRole,
    ProviderConfig, ProviderKind, StreamEvent, StreamOutcome, TokenUsage,
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

    fn genai_request(request: ChatRequest) -> genai::chat::ChatRequest {
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ChatRole::System => genai::chat::ChatRole::System,
                    ChatRole::User => genai::chat::ChatRole::User,
                    ChatRole::Assistant => genai::chat::ChatRole::Assistant,
                };
                genai::chat::ChatMessage::new(role, message.content)
            })
            .collect();
        genai::chat::ChatRequest {
            system: request.system,
            messages,
            tools: None,
            previous_response_id: None,
            store: None,
        }
    }
}

#[async_trait]
impl ChatProvider for GenaiAdapter {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true }
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
        events: mpsc::Sender<StreamEvent>,
        cancel: &CancelFlag,
    ) -> Result<StreamOutcome, AiError> {
        let options = genai::chat::ChatOptions::default().with_capture_usage(true);
        let mut response = self
            .client
            .exec_chat_stream(
                genai::ModelSpec::from_name(self.model.clone()),
                Self::genai_request(request),
                Some(&options),
            )
            .await
            .map_err(map_error)?;

        let mut content = String::new();
        let mut usage = TokenUsage::default();
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
                genai::chat::ChatStreamEvent::End(end) => {
                    if let Some(captured) = end.captured_usage {
                        usage = TokenUsage {
                            input_tokens: captured.prompt_tokens.and_then(|v| u32::try_from(v).ok()),
                            output_tokens: captured
                                .completion_tokens
                                .and_then(|v| u32::try_from(v).ok()),
                        };
                    }
                }
                // `Start`, reasoning and tool-call chunks are out of MVP scope:
                // the conversation layer ignores them and the UI shows text only.
                genai::chat::ChatStreamEvent::Start
                | genai::chat::ChatStreamEvent::ReasoningChunk(_)
                | genai::chat::ChatStreamEvent::ThoughtSignatureChunk(_)
                | genai::chat::ChatStreamEvent::ToolCallChunk(_) => {}
            }
        }
        Ok(StreamOutcome { content, usage })
    }
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
