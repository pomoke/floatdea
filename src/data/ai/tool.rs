//! Provider-neutral tool system for AI boxes (plan_ai.md §9.8 / 阶段 2.5).
//!
//! FloatDea owns tool definitions, validation and execution. A model can only
//! request tools that exist in the registry, whose arguments pass the schema
//! and whose scope is already bounded by the conversation's sources. Tool
//! execution never touches the workspace, the file system or any store: it only
//! reads the bounded [`ToolContext`] captured at send time, and the only thing
//! it can produce is a [`SnippetProposal`] that still awaits user confirmation
//! (it cannot write a Markdown entity by itself).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::provider::CancelFlag;
use super::types::{SnippetProposal, SourceTarget};

/// Namespaced stable ids of the built-in tools (plan_ai.md §9.8).
pub const TOOL_LIST_SOURCES: &str = "core.list_sources";
pub const TOOL_READ_SOURCE: &str = "core.read_source";
pub const TOOL_SEARCH_SOURCES: &str = "core.search_sources";
pub const TOOL_CREATE_OUTPUT_PROPOSAL: &str = "core.create_output_proposal";

/// Maximum characters of a proposal body accepted from the model. Bounds the
/// sidecar payload a misbehaving model could otherwise flood.
pub const PROPOSAL_MAX_CHARS: usize = 60_000;

/// Side-effect category of a tool. `ReadOnly` tools have no write channel;
/// `Proposal` tools only return data awaiting user confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffect {
    ReadOnly,
    Proposal,
}

/// Where a tool comes from. MCP servers and plugins will add future sources;
/// they must go through the same registry/executor contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    Builtin,
}

/// Stable description of one tool exposed to models and used for validation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// Namespaced stable id, e.g. `core.read_source`.
    pub id: String,
    pub version: u32,
    /// Human-readable name.
    pub name: String,
    /// Description the model uses to decide when to call the tool.
    pub description: String,
    pub side_effect: ToolSideEffect,
    pub source: ToolSource,
    /// JSON Schema for the arguments object. Extra arguments are rejected when
    /// the schema sets `additionalProperties: false`.
    pub schema: Value,
    /// Maximum bytes of the result text fed back to the model.
    pub max_result_bytes: usize,
    /// Maximum calls of this tool per turn.
    pub max_calls_per_turn: u32,
}

/// A tool invocation requested by the model (provider-neutral).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Stable id used to correlate the tool response (assigned by the provider).
    pub call_id: String,
    /// The namespaced `ToolDef.id`, e.g. `core.read_source`.
    pub tool_id: String,
    /// JSON arguments as provided by the model.
    pub arguments: Value,
}

/// One source the current turn may read: the stable target plus the title and
/// bounded content captured at send time. Tools never resolve fresh content and
/// never see anything outside this list.
#[derive(Clone, Debug)]
pub struct BoundSource {
    /// 1-based source number assigned locally at send time; the model refers to
    /// sources by this number, never by `EntityId`.
    pub index: u32,
    pub target: SourceTarget,
    /// Title captured at send time (display only).
    pub title: String,
    /// Content already bounded/truncated by the conversation layer.
    pub content: String,
    /// FNV-1a hash of `content` at send time.
    pub content_hash: String,
}

/// The bounded read scope handed to tools for one turn. It contains only the
/// sources bound to the current conversation and selected for this turn.
#[derive(Clone, Debug, Default)]
pub struct ToolContext {
    pub sources: Vec<BoundSource>,
}

/// Lifecycle status of one tool invocation in a conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    #[default]
    Succeeded,
    Failed,
}

/// A persisted, visible receipt of one tool invocation (plan_ai.md §7.5). Tool
/// receipts are independent events in the conversation: they are never hidden
/// inside the assistant text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRecord {
    pub tool_id: String,
    pub status: ToolStatus,
    /// Short human-readable result summary, e.g. "read source #2 (title)".
    pub summary: String,
}

/// The outcome of executing one tool call.
#[derive(Clone, Debug)]
pub struct ToolResult {
    /// Bounded text fed back to the model.
    pub content: String,
    /// The visible receipt stored in the conversation.
    pub record: ToolRecord,
    /// Set only by `core.create_output_proposal` (a proposal, never a write).
    pub proposal: Option<SnippetProposal>,
}

/// Merges tool definitions from built-ins (and, later, MCP/plugins) and is the
/// only source of truth for which tools a model may call.
#[derive(Clone, Debug, Default)]
pub struct ToolRegistry {
    tools: Vec<ToolDef>,
}

impl ToolRegistry {
    pub fn new(defs: Vec<ToolDef>) -> Self {
        Self { tools: defs }
    }

    /// The first-phase built-in tools (plan_ai.md §9.8).
    pub fn builtins() -> Self {
        Self::new(builtin_tools())
    }

    pub fn definitions(&self) -> &[ToolDef] {
        &self.tools
    }

    pub fn def(&self, id: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|tool| tool.id == id)
    }
}

/// Executes one validated tool call against the bounded context. Returns a
/// receipt for the conversation and, for `core.create_output_proposal`, a
/// proposal that still requires user confirmation. Never writes the workspace.
pub fn execute_tool_call(
    call: &ToolCall,
    registry: &ToolRegistry,
    ctx: &ToolContext,
    cancel: &CancelFlag,
) -> ToolResult {
    if cancel.is_cancelled() {
        return failed(call, "cancelled before execution");
    }
    let Some(def) = registry.def(&call.tool_id) else {
        return failed(call, format!("unknown tool '{}'", call.tool_id));
    };
    if let Err(message) = validate_arguments(&def.schema, &call.arguments) {
        return failed(call, message);
    }
    match call.tool_id.as_str() {
        TOOL_LIST_SOURCES => list_sources(call, def, ctx),
        TOOL_READ_SOURCE => read_source(call, def, ctx),
        TOOL_SEARCH_SOURCES => search_sources(call, def, ctx),
        TOOL_CREATE_OUTPUT_PROPOSAL => create_output_proposal(call, def),
        _ => failed(call, "tool is registered but not executable"),
    }
}

// ---- tool implementations ----

/// `core.list_sources`: lists the bound sources the current turn may read
/// (number, title, kind, content hash). No content is returned.
fn list_sources(call: &ToolCall, def: &ToolDef, ctx: &ToolContext) -> ToolResult {
    if ctx.sources.is_empty() {
        return ok(
            call,
            "No sources are bound to this conversation.",
            "listed 0 bound sources",
        );
    }
    let mut lines = Vec::new();
    for source in &ctx.sources {
        let kind = match source.target {
            SourceTarget::Snippet(_) => "snippet",
            SourceTarget::Container(_) => "container",
            SourceTarget::ExternalFile(_) => "external_file",
        };
        lines.push(format!(
            "{}. [{}] ({kind}, content hash {})",
            source.index, source.title, source.content_hash
        ));
    }
    let content = truncate_chars(&lines.join("\n"), def.max_result_bytes);
    ok(
        call,
        content,
        format!("listed {} bound source(s)", ctx.sources.len()),
    )
}

/// `core.read_source`: returns the bounded content of one bound source by its
/// locally assigned number.
fn read_source(call: &ToolCall, def: &ToolDef, ctx: &ToolContext) -> ToolResult {
    let Some(number) = call
        .arguments
        .get("source")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    else {
        return failed(call, "missing 'source' (a 1-based source number)");
    };
    let Some(source) = ctx.sources.iter().find(|source| source.index == number) else {
        return failed(call, format!("source #{number} is not bound to this conversation"));
    };
    let content = truncate_chars(
        &format!("[{}] {}\n{}", source.index, source.title, source.content),
        def.max_result_bytes,
    );
    ok(
        call,
        content,
        format!("read source #{number} ({})", source.title),
    )
}

/// `core.search_sources`: case-insensitive substring search restricted to the
/// bound sources.
fn search_sources(call: &ToolCall, def: &ToolDef, ctx: &ToolContext) -> ToolResult {
    let Some(query) = call
        .arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
    else {
        return failed(call, "missing non-empty 'query'");
    };
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for source in &ctx.sources {
        if source.content.to_lowercase().contains(&needle) {
            hits.push(format!(
                "{}. [{}]\n{}",
                source.index,
                source.title,
                excerpt(&source.content, &needle)
            ));
        }
    }
    if hits.is_empty() {
        return ok(
            call,
            format!("No bound source contains '{}'.", query),
            format!("searched sources for '{}' (0 hits)", query),
        );
    }
    let content = truncate_chars(&hits.join("\n\n"), def.max_result_bytes);
    ok(
        call,
        content,
        format!("searched sources for '{}' ({} hit(s))", query, hits.len()),
    )
}

/// `core.create_output_proposal`: assembles a `CreateSnippetProposal` from the
/// model's title + full Markdown body. It accepts no `ContainerId`/`EntityId`/
/// path: the destination is bound by the UI to the current AI box and the new
/// `EntityId` is generated locally at commit time.
fn create_output_proposal(call: &ToolCall, def: &ToolDef) -> ToolResult {
    let Some(title) = call
        .arguments
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
    else {
        return failed(call, "missing non-empty 'title'");
    };
    let Some(content) = call
        .arguments
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
    else {
        return failed(call, "missing non-empty 'content'");
    };
    let content: String = content.chars().take(PROPOSAL_MAX_CHARS).collect();
    let proposal = SnippetProposal::new(title.to_owned(), content.clone());
    let reply = format!(
        "Proposal created for '{title}' ({} chars). The user will preview and confirm it before it becomes a Snippet.",
        content.chars().count()
    );
    ToolResult {
        content: truncate_chars(&reply, def.max_result_bytes),
        record: ToolRecord {
            tool_id: call.tool_id.clone(),
            status: ToolStatus::Succeeded,
            summary: format!("proposed new snippet: {title}"),
        },
        proposal: Some(proposal),
    }
}

// ---- validation helpers ----

/// Validates model-supplied arguments against the tool's JSON Schema: the
/// payload must be an object, all `required` fields present, types must match
/// and unknown keys are rejected when the schema forbids extra properties.
fn validate_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let Some(object) = arguments.as_object() else {
        return Err("tool arguments must be a JSON object".to_owned());
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required {
            if let Some(name) = field.as_str()
                && !object.contains_key(name)
            {
                return Err(format!("missing required argument '{name}'"));
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, value) in object {
            let Some(property) = properties.get(name) else {
                if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    return Err(format!("unexpected argument '{name}'"));
                }
                continue;
            };
            let expected = property.get("type").and_then(Value::as_str);
            let matches = match expected {
                Some("string") => value.is_string(),
                Some("integer") => value.is_i64() || value.is_u64(),
                Some("number") => value.is_number(),
                Some("boolean") => value.is_boolean(),
                Some("object") => value.is_object(),
                Some("array") => value.is_array(),
                _ => true,
            };
            if !matches {
                return Err(format!("argument '{name}' must be a {expected:?}"));
            }
        }
    }
    Ok(())
}

fn builtin_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            id: TOOL_LIST_SOURCES.to_owned(),
            version: 1,
            name: "List sources".to_owned(),
            description: "Lists the read-only sources bound to this conversation that the current turn may use (number, title, kind). Call this first to see what is available.".to_owned(),
            side_effect: ToolSideEffect::ReadOnly,
            source: ToolSource::Builtin,
            schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            max_result_bytes: 4000,
            max_calls_per_turn: 2,
        },
        ToolDef {
            id: TOOL_READ_SOURCE.to_owned(),
            version: 1,
            name: "Read source".to_owned(),
            description: "Reads the bounded content of one source by its 1-based number (as returned by core.list_sources).".to_owned(),
            side_effect: ToolSideEffect::ReadOnly,
            source: ToolSource::Builtin,
            schema: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "integer",
                        "description": "1-based source number from core.list_sources"
                    }
                },
                "required": ["source"],
                "additionalProperties": false
            }),
            max_result_bytes: 12_000,
            max_calls_per_turn: 4,
        },
        ToolDef {
            id: TOOL_SEARCH_SOURCES.to_owned(),
            version: 1,
            name: "Search sources".to_owned(),
            description: "Case-insensitive keyword search restricted to the bound sources; returns matching titles with short excerpts.".to_owned(),
            side_effect: ToolSideEffect::ReadOnly,
            source: ToolSource::Builtin,
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "keyword to find in the bound sources"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            max_result_bytes: 8000,
            max_calls_per_turn: 2,
        },
        ToolDef {
            id: TOOL_CREATE_OUTPUT_PROPOSAL.to_owned(),
            version: 1,
            name: "Create output proposal".to_owned(),
            description: "Creates a proposal for a brand-new Snippet (title + full Markdown body) in the current AI box. The user must preview and confirm before it is saved. Never used to modify, append to, replace or delete an existing snippet, and never accepts container/entity ids or file paths.".to_owned(),
            side_effect: ToolSideEffect::Proposal,
            source: ToolSource::Builtin,
            schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["title", "content"],
                "additionalProperties": false
            }),
            max_result_bytes: 2000,
            max_calls_per_turn: 2,
        },
    ]
}

fn ok(call: &ToolCall, content: impl Into<String>, summary: impl Into<String>) -> ToolResult {
    ToolResult {
        content: content.into(),
        record: ToolRecord {
            tool_id: call.tool_id.clone(),
            status: ToolStatus::Succeeded,
            summary: summary.into(),
        },
        proposal: None,
    }
}

fn failed(call: &ToolCall, message: impl Into<String>) -> ToolResult {
    ToolResult {
        content: format!("Error: {}", message.into()),
        record: ToolRecord {
            tool_id: call.tool_id.clone(),
            status: ToolStatus::Failed,
            summary: "failed".to_owned(),
        },
        proposal: None,
    }
}

/// Truncates `text` to at most `max_bytes` characters (byte-agnostic).
fn truncate_chars(text: &str, max_bytes: usize) -> String {
    text.chars().take(max_bytes).collect()
}

/// A short excerpt around the first occurrence of `needle` in `text`.
fn excerpt(text: &str, needle: &str) -> String {
    const EXCERPT_CHARS: usize = 160;
    let text = text.trim();
    let start = text.to_lowercase().find(needle).unwrap_or(0);
    let begin = start.saturating_sub(40);
    let slice: String = text.chars().skip(begin).take(EXCERPT_CHARS).collect();
    if begin > 0 {
        format!("…{slice}…")
    } else {
        slice
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ContainerId, EntityId};

    fn context() -> ToolContext {
        ToolContext {
            sources: vec![
                BoundSource {
                    index: 1,
                    target: SourceTarget::Snippet(EntityId::new()),
                    title: "Alpha".to_owned(),
                    content: "FloatDea is a local-first notes app.".to_owned(),
                    content_hash: "h1".to_owned(),
                },
                BoundSource {
                    index: 2,
                    target: SourceTarget::Container(ContainerId::new()),
                    title: "Beta".to_owned(),
                    content: "AI boxes keep their scope read-only.".to_owned(),
                    content_hash: "h2".to_owned(),
                },
            ],
        }
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::builtins()
    }

    fn call(tool_id: &str, arguments: Value) -> ToolCall {
        ToolCall {
            call_id: "call_1".to_owned(),
            tool_id: tool_id.to_owned(),
            arguments,
        }
    }

    #[test]
    fn builtins_contain_the_four_first_phase_tools() {
        let registry = registry();
        let ids: Vec<&str> = registry
            .definitions()
            .iter()
            .map(|def| def.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                TOOL_LIST_SOURCES,
                TOOL_READ_SOURCE,
                TOOL_SEARCH_SOURCES,
                TOOL_CREATE_OUTPUT_PROPOSAL
            ]
        );
        assert_eq!(
            registry.def(TOOL_CREATE_OUTPUT_PROPOSAL).unwrap().side_effect,
            ToolSideEffect::Proposal
        );
    }

    #[test]
    fn list_sources_returns_numbers_and_titles_without_content() {
        let cancel = CancelFlag::new();
        let result = execute_tool_call(&call(TOOL_LIST_SOURCES, json!({})), &registry(), &context(), &cancel);
        assert_eq!(result.record.status, ToolStatus::Succeeded);
        assert!(result.content.contains("1. [Alpha] (snippet"));
        assert!(result.content.contains("2. [Beta] (container"));
        assert!(!result.content.contains("local-first"));
        assert_eq!(result.record.summary, "listed 2 bound source(s)");
    }

    #[test]
    fn read_source_returns_bounded_content_of_the_requested_number() {
        let cancel = CancelFlag::new();
        let result = execute_tool_call(&call(TOOL_READ_SOURCE, json!({"source": 2})), &registry(), &context(), &cancel);
        assert_eq!(result.record.status, ToolStatus::Succeeded);
        assert!(result.content.contains("read-only"));
        assert!(!result.content.contains("local-first"));
        // Out-of-range numbers fail instead of expanding the scope.
        let out = execute_tool_call(&call(TOOL_READ_SOURCE, json!({"source": 9})), &registry(), &context(), &cancel);
        assert_eq!(out.record.status, ToolStatus::Failed);
    }

    #[test]
    fn search_sources_only_finds_bound_sources() {
        let cancel = CancelFlag::new();
        let result = execute_tool_call(&call(TOOL_SEARCH_SOURCES, json!({"query": "read-only"})), &registry(), &context(), &cancel);
        assert_eq!(result.record.status, ToolStatus::Succeeded);
        assert!(result.content.contains("[Beta]"));
        assert!(!result.content.contains("[Alpha]"));
        assert_eq!(result.record.summary, "searched sources for 'read-only' (1 hit(s))");
    }

    #[test]
    fn create_output_proposal_returns_a_proposal_without_writing() {
        let cancel = CancelFlag::new();
        let result = execute_tool_call(
            &call(
                TOOL_CREATE_OUTPUT_PROPOSAL,
                json!({"title": "Summary", "content": "# Summary\n\nBody"}),
            ),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(result.record.status, ToolStatus::Succeeded);
        let proposal = result.proposal.expect("a proposal is produced");
        assert_eq!(proposal.title, "Summary");
        assert!(proposal.content.contains("# Summary"));
        assert!(proposal.created.is_none());
        assert!(!proposal.rejected);
    }

    #[test]
    fn proposal_tool_rejects_ids_and_unknown_arguments() {
        let cancel = CancelFlag::new();
        let with_container = execute_tool_call(
            &call(
                TOOL_CREATE_OUTPUT_PROPOSAL,
                json!({"title": "T", "content": "Body", "container_id": "abc"}),
            ),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(with_container.record.status, ToolStatus::Failed);
        assert!(with_container.proposal.is_none());
        let missing_content = execute_tool_call(
            &call(TOOL_CREATE_OUTPUT_PROPOSAL, json!({"title": "T"})),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(missing_content.record.status, ToolStatus::Failed);
    }

    #[test]
    fn unknown_tools_and_malformed_arguments_fail_fast() {
        let cancel = CancelFlag::new();
        let unknown = execute_tool_call(
            &call("core.delete_everything", json!({})),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(unknown.record.status, ToolStatus::Failed);
        let not_an_object = execute_tool_call(
            &call(TOOL_LIST_SOURCES, json!([1, 2])),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(not_an_object.record.status, ToolStatus::Failed);
        let wrong_type = execute_tool_call(
            &call(TOOL_READ_SOURCE, json!({"source": "one"})),
            &registry(),
            &context(),
            &cancel,
        );
        assert_eq!(wrong_type.record.status, ToolStatus::Failed);
    }
}
