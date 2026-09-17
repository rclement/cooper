//! Over-the-wire shapes of the Anthropic Messages API, and the translation
//! between them and Cooper's provider-neutral `Message`.
//!
//! The Messages API differs from OpenAI chat completions in three ways that
//! shape this module:
//!
//! - The system prompt is a top-level `system` field, not a message.
//! - Every message body is a list of typed *content blocks* (`text`,
//!   `thinking`, `tool_use`, `tool_result`, …) instead of a flat string plus
//!   side fields. Tool results travel inside a `user` message.
//! - The stream is a sequence of named events (`message_start`,
//!   `content_block_delta`, `message_delta`, …) that build up those blocks
//!   one at a time, rather than one delta shape repeated until `[DONE]`.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::agent::{AgentEventsHandler, AgentMessageChunk, FinishReason, Message, ToolCall, Usage};
use crate::tools::{ToolParameterTypeSchema, ToolSchema};

// ---------------------------------------------------------------------------
// Request side: what we send
// ---------------------------------------------------------------------------

/// One block of an Anthropic message body.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// A message body is either blocks we build from the shared `Message`
/// fields, or the provider's own blocks replayed exactly as they were
/// received (see `Message::Assistant::provider_content`).
#[derive(Serialize)]
#[serde(untagged)]
enum ApiMessageContent {
    Blocks(Vec<ApiContentBlock>),
    Verbatim(serde_json::Value),
}

#[derive(Serialize)]
pub struct ApiMessage {
    role: String,
    content: ApiMessageContent,
}

impl ApiMessage {
    fn user_text(text: &str) -> Self {
        ApiMessage {
            role: "user".to_string(),
            content: ApiMessageContent::Blocks(vec![ApiContentBlock::Text {
                text: text.to_string(),
            }]),
        }
    }

    fn assistant_blocks(blocks: Vec<ApiContentBlock>) -> Self {
        ApiMessage {
            role: "assistant".to_string(),
            content: ApiMessageContent::Blocks(blocks),
        }
    }

    fn assistant_verbatim(content: serde_json::Value) -> Self {
        ApiMessage {
            role: "assistant".to_string(),
            content: ApiMessageContent::Verbatim(content),
        }
    }

    fn tool_results(blocks: Vec<ApiContentBlock>) -> Self {
        ApiMessage {
            role: "user".to_string(),
            content: ApiMessageContent::Blocks(blocks),
        }
    }
}

fn tool_result_block(call_id: &str, result: &Result<String, String>) -> ApiContentBlock {
    match result {
        Ok(output) => ApiContentBlock::ToolResult {
            tool_use_id: call_id.to_string(),
            content: output.clone(),
            is_error: false,
        },
        Err(error) => ApiContentBlock::ToolResult {
            tool_use_id: call_id.to_string(),
            content: error.clone(),
            is_error: true,
        },
    }
}

/// The system prompt and message list of a request, split the way the
/// Messages API wants them.
pub struct ApiConversation {
    pub system: Option<String>,
    pub messages: Vec<ApiMessage>,
}

impl ApiConversation {
    /// Lays out Cooper's history for the Messages API.
    ///
    /// - `System` messages leave the list and become the `system` field.
    /// - `User` text becomes a `user` message with one text block.
    /// - An `Assistant` reply that carries `provider_content` is replayed
    ///   verbatim, so signed thinking blocks survive the round trip. One
    ///   produced elsewhere is rebuilt from its text and tool calls; its
    ///   reasoning is left out, since the API only accepts thinking blocks
    ///   it signed itself.
    /// - Consecutive `Tool` results are gathered into a single `user`
    ///   message: the API wants every result of a parallel tool-use turn
    ///   in one message, and rejects two `user` messages in a row.
    pub fn from_messages(messages: &[Message]) -> Self {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut api_messages: Vec<ApiMessage> = Vec::new();
        let mut pending_tool_results: Vec<ApiContentBlock> = Vec::new();

        let flush_tool_results = |pending: &mut Vec<ApiContentBlock>, out: &mut Vec<ApiMessage>| {
            if !pending.is_empty() {
                out.push(ApiMessage::tool_results(std::mem::take(pending)));
            }
        };

        for message in messages {
            match message {
                Message::Tool {
                    call_id, result, ..
                } => {
                    pending_tool_results.push(tool_result_block(call_id, result));
                    continue;
                }
                _ => flush_tool_results(&mut pending_tool_results, &mut api_messages),
            }

            match message {
                Message::System(text) => system_parts.push(text),
                Message::User(text) => api_messages.push(ApiMessage::user_text(text)),
                Message::Assistant {
                    text,
                    tool_calls,
                    provider_content: Some(content),
                    ..
                } if content.as_array().is_some_and(|blocks| !blocks.is_empty()) => {
                    // Silence the unused-binding lints on the shared view:
                    // the verbatim blocks already contain the same text and
                    // tool calls.
                    let _ = (text, tool_calls);
                    api_messages.push(ApiMessage::assistant_verbatim(content.clone()));
                }
                Message::Assistant {
                    text, tool_calls, ..
                } => {
                    let mut blocks = Vec::new();
                    if let Some(text) = text
                        && !text.trim().is_empty()
                    {
                        blocks.push(ApiContentBlock::Text { text: text.clone() });
                    }
                    for tool_call in tool_calls {
                        blocks.push(ApiContentBlock::ToolUse {
                            id: tool_call.id.clone(),
                            name: tool_call.name.clone(),
                            input: serde_json::to_value(&tool_call.arguments)
                                .unwrap_or(serde_json::Value::Object(Default::default())),
                        });
                    }
                    // The API rejects an assistant message with no blocks;
                    // a reply that said nothing is simply not replayed.
                    if !blocks.is_empty() {
                        api_messages.push(ApiMessage::assistant_blocks(blocks));
                    }
                }
                Message::Tool { .. } => unreachable!("tool results are gathered above"),
            }
        }
        flush_tool_results(&mut pending_tool_results, &mut api_messages);

        ApiConversation {
            system: if system_parts.is_empty() {
                None
            } else {
                Some(system_parts.join("\n\n"))
            },
            messages: api_messages,
        }
    }
}

fn get_tool_param_type(param_type: &ToolParameterTypeSchema) -> &'static str {
    match param_type {
        ToolParameterTypeSchema::String => "string",
        ToolParameterTypeSchema::Number => "number",
        ToolParameterTypeSchema::Boolean => "boolean",
    }
}

#[derive(Serialize)]
struct ApiToolParamProperty {
    #[serde(rename = "type")]
    param_type: String,
    description: String,
}

#[derive(Serialize)]
struct ApiToolInputSchema {
    #[serde(rename = "type")]
    object_type: String,
    properties: HashMap<String, ApiToolParamProperty>,
    required: Vec<String>,
}

/// A tool definition. Same JSON-schema payload as OpenAI's, under the
/// Messages API's `input_schema` name and without the `function` wrapper.
#[derive(Serialize)]
pub struct ApiTool {
    name: String,
    description: String,
    input_schema: ApiToolInputSchema,
}

impl From<&ToolSchema> for ApiTool {
    fn from(t: &ToolSchema) -> Self {
        ApiTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: ApiToolInputSchema {
                object_type: "object".to_string(),
                properties: t
                    .parameters
                    .iter()
                    .map(|(name, param)| {
                        (
                            name.clone(),
                            ApiToolParamProperty {
                                param_type: get_tool_param_type(&param.param_type).to_string(),
                                description: param.description.clone(),
                            },
                        )
                    })
                    .collect(),
                required: t
                    .parameters
                    .iter()
                    .filter(|(_, param)| param.required)
                    .map(|(name, _)| name.clone())
                    .collect(),
            },
        }
    }
}

#[derive(Serialize)]
pub struct ApiMessagesRequest {
    pub model: String,
    /// Required by the API: the hard ceiling on generated tokens.
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ApiTool>,
    pub stream: bool,
}

// ---------------------------------------------------------------------------
// Response side: what we receive, one SSE event at a time
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ApiUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
pub struct ApiMessageStart {
    #[serde(default)]
    usage: Option<ApiUsage>,
}

/// The opening of a content block: its kind, plus whatever the API sends
/// up front (a tool call's id and name arrive here, its arguments later).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiContentBlockStart {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
pub struct ApiMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
pub struct ApiError {
    #[serde(default, rename = "type")]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// One event of the stream, as found in a `data:` line. The `event:` line
/// that precedes it in SSE repeats the same `type`, so it can be ignored.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiStreamEvent {
    MessageStart {
        message: ApiMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: ApiContentBlockStart,
    },
    ContentBlockDelta {
        index: usize,
        delta: ApiContentBlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: ApiMessageDelta,
        #[serde(default)]
        usage: Option<ApiUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: ApiError,
    },
    #[serde(other)]
    Unsupported,
}

// ---------------------------------------------------------------------------
// Accumulating the stream into one assistant message
// ---------------------------------------------------------------------------

/// A content block under construction, keyed by its stream index.
enum BlockInProgress {
    Text(String),
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    ToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
    Unsupported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamPhase {
    Reasoning,
    Response,
}

/// Folds stream events into the final `Message::Assistant`, forwarding
/// text and reasoning to the handler as they arrive.
#[derive(Default)]
pub struct MessagesStreamAccumulator {
    blocks: BTreeMap<usize, BlockInProgress>,
    text_buf: String,
    reasoning_buf: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    stop_reason: Option<String>,
    current_phase: Option<(StreamPhase, Instant)>,
    reasoning_ms: Option<u64>,
    response_ms: Option<u64>,
}

impl MessagesStreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    fn enter_phase(&mut self, phase: StreamPhase) {
        match self.current_phase {
            Some((current, _)) if current == phase => {} // already in it
            _ => {
                self.close_current_phase();
                self.current_phase = Some((phase, Instant::now()));
            }
        }
    }

    fn close_current_phase(&mut self) {
        if let Some((phase, started_at)) = self.current_phase.take() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            match phase {
                StreamPhase::Reasoning => self.reasoning_ms = Some(elapsed_ms),
                StreamPhase::Response => self.response_ms = Some(elapsed_ms),
            }
        }
    }

    fn push_text(&mut self, text: &str, handler: &dyn AgentEventsHandler) {
        // Like the OpenAI accumulator: a whitespace-only delta before any
        // real text is a formatting artifact, not a response — dropping it
        // keeps `text` at `None` for tool-only turns. Once real text is
        // underway, whitespace is kept so paragraphs stay separated.
        if text.is_empty() || (self.text_buf.is_empty() && text.trim().is_empty()) {
            return;
        }
        self.enter_phase(StreamPhase::Response);
        self.text_buf.push_str(text);
        handler.on_chunk(&AgentMessageChunk {
            text: Some(text.to_string()),
            reasoning: None,
        });
    }

    fn push_reasoning(&mut self, thinking: &str, handler: &dyn AgentEventsHandler) {
        if thinking.is_empty() {
            return;
        }
        self.enter_phase(StreamPhase::Reasoning);
        self.reasoning_buf.push_str(thinking);
        handler.on_chunk(&AgentMessageChunk {
            text: None,
            reasoning: Some(thinking.to_string()),
        });
    }

    /// Applies one event. An `error` event is the API aborting the reply
    /// mid-stream, and surfaces as the returned error.
    pub fn push(
        &mut self,
        event: ApiStreamEvent,
        handler: &dyn AgentEventsHandler,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match event {
            ApiStreamEvent::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.input_tokens = usage.input_tokens;
                    self.output_tokens = usage.output_tokens;
                }
            }
            ApiStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let block = match content_block {
                    ApiContentBlockStart::Text { text } => {
                        self.push_text(&text, handler);
                        BlockInProgress::Text(text)
                    }
                    ApiContentBlockStart::Thinking {
                        thinking,
                        signature,
                    } => {
                        self.push_reasoning(&thinking, handler);
                        BlockInProgress::Thinking {
                            thinking,
                            signature,
                        }
                    }
                    ApiContentBlockStart::RedactedThinking { data } => {
                        BlockInProgress::RedactedThinking(data)
                    }
                    ApiContentBlockStart::ToolUse { id, name } => BlockInProgress::ToolUse {
                        id,
                        name,
                        partial_json: String::new(),
                    },
                    ApiContentBlockStart::Unsupported => BlockInProgress::Unsupported,
                };
                self.blocks.insert(index, block);
            }
            ApiStreamEvent::ContentBlockDelta { index, delta } => match delta {
                ApiContentBlockDelta::TextDelta { text } => {
                    self.push_text(&text, handler);
                    if let Some(BlockInProgress::Text(buf)) = self.blocks.get_mut(&index) {
                        buf.push_str(&text);
                    }
                }
                ApiContentBlockDelta::ThinkingDelta { thinking } => {
                    self.push_reasoning(&thinking, handler);
                    if let Some(BlockInProgress::Thinking { thinking: buf, .. }) =
                        self.blocks.get_mut(&index)
                    {
                        buf.push_str(&thinking);
                    }
                }
                ApiContentBlockDelta::SignatureDelta { signature } => {
                    if let Some(BlockInProgress::Thinking {
                        signature: slot, ..
                    }) = self.blocks.get_mut(&index)
                    {
                        *slot = Some(signature);
                    }
                }
                ApiContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some(BlockInProgress::ToolUse {
                        partial_json: buf, ..
                    }) = self.blocks.get_mut(&index)
                    {
                        buf.push_str(&partial_json);
                    }
                }
                ApiContentBlockDelta::Unsupported => {}
            },
            ApiStreamEvent::ContentBlockStop { .. } => {}
            ApiStreamEvent::MessageDelta { delta, usage } => {
                if let Some(stop_reason) = delta.stop_reason {
                    self.stop_reason = Some(stop_reason);
                }
                if let Some(usage) = usage {
                    if usage.input_tokens.is_some() {
                        self.input_tokens = usage.input_tokens;
                    }
                    if usage.output_tokens.is_some() {
                        self.output_tokens = usage.output_tokens;
                    }
                }
            }
            ApiStreamEvent::Error { error } => {
                return Err(format!(
                    "anthropic api error ({}): {}",
                    error.error_type.unwrap_or_else(|| "unknown".to_string()),
                    error.message.unwrap_or_else(|| "no message".to_string())
                )
                .into());
            }
            ApiStreamEvent::MessageStop | ApiStreamEvent::Ping | ApiStreamEvent::Unsupported => {}
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
        self.close_current_phase();

        let mut tool_calls = Vec::new();
        let mut verbatim_blocks = Vec::new();

        for block in self.blocks.into_values() {
            match block {
                BlockInProgress::Text(text) => {
                    if !text.trim().is_empty() {
                        verbatim_blocks.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                }
                BlockInProgress::Thinking {
                    thinking,
                    signature,
                } => {
                    // Only a signed thinking block can be replayed; an
                    // unsigned one would be rejected on the next turn.
                    if let Some(signature) = signature {
                        verbatim_blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": signature,
                        }));
                    }
                }
                BlockInProgress::RedactedThinking(data) => {
                    verbatim_blocks
                        .push(serde_json::json!({ "type": "redacted_thinking", "data": data }));
                }
                BlockInProgress::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input = parse_tool_input(&partial_json)?;
                    verbatim_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }));
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: tool_arguments_as_strings(&input)?,
                    });
                }
                BlockInProgress::Unsupported => {}
            }
        }

        let usage = match (self.input_tokens, self.output_tokens) {
            (None, None) => None,
            (input, output) => {
                let prompt_tokens = input.unwrap_or(0);
                let completion_tokens = output.unwrap_or(0);
                Some(Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens + completion_tokens,
                })
            }
        };

        let message = Message::Assistant {
            text: if self.text_buf.is_empty() {
                None
            } else {
                Some(self.text_buf)
            },
            reasoning: if self.reasoning_buf.is_empty() {
                None
            } else {
                Some(self.reasoning_buf)
            },
            tool_calls,
            reasoning_duration_ms: self.reasoning_ms,
            response_duration_ms: self.response_ms,
            usage,
            at_ms: None,
            provider_content: if verbatim_blocks.is_empty() {
                None
            } else {
                Some(serde_json::Value::Array(verbatim_blocks))
            },
        };

        let finish_reason = match self.stop_reason.as_deref() {
            Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
            Some("tool_use") => FinishReason::ToolCalls,
            Some("max_tokens") => FinishReason::Length,
            Some(other) => FinishReason::Unknown(other.to_string()),
            None => FinishReason::Unknown("none".to_string()),
        };

        Ok((message, finish_reason))
    }
}

/// A tool call's arguments arrive as fragments of one JSON object. A call
/// with no arguments may stream no fragments at all, which reads as `{}`.
fn parse_tool_input(partial_json: &str) -> Result<serde_json::Value, serde_json::Error> {
    if partial_json.trim().is_empty() {
        Ok(serde_json::Value::Object(Default::default()))
    } else {
        serde_json::from_str(partial_json)
    }
}

/// Cooper's tools take every argument as a string. Claude honors the
/// declared parameter types, so a `number` parameter arrives as a JSON
/// number: scalars are rendered back to text rather than rejected. Nested
/// objects and arrays have no string form a tool could use, so they are
/// still an error.
fn tool_arguments_as_strings(
    input: &serde_json::Value,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let object = input
        .as_object()
        .ok_or("tool call input is not a JSON object")?;
    object
        .iter()
        .map(|(name, value)| {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => {
                    return Err(
                        format!("tool argument '{name}' is not a scalar value: {other}").into(),
                    );
                }
            };
            Ok((name.clone(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolParameterSchema;

    fn to_json<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    // --- request layout -------------------------------------------------

    #[test]
    fn system_prompt_leaves_the_message_list_and_becomes_the_system_field() {
        let conversation = ApiConversation::from_messages(&[
            Message::System("be brief".to_string()),
            Message::User("hi".to_string()),
        ]);

        assert_eq!(conversation.system.as_deref(), Some("be brief"));
        assert_eq!(
            to_json(&conversation.messages),
            serde_json::json!([
                {"role": "user", "content": [{"type": "text", "text": "hi"}]}
            ])
        );
    }

    #[test]
    fn assistant_reply_from_another_provider_is_rebuilt_from_text_and_tool_calls() {
        let reply = Message::assistant(
            Some("checking".to_string()),
            Some("unsigned reasoning".to_string()),
            vec![ToolCall {
                id: "toolu_1".to_string(),
                name: "read_file".to_string(),
                arguments: HashMap::from([("path".to_string(), "a.txt".to_string())]),
            }],
        );

        let conversation = ApiConversation::from_messages(&[reply]);

        assert_eq!(
            to_json(&conversation.messages),
            serde_json::json!([{
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.txt"}}
                ]
            }])
        );
    }

    #[test]
    fn assistant_reply_from_anthropic_is_replayed_verbatim_with_its_signed_thinking() {
        let verbatim = serde_json::json!([
            {"type": "thinking", "thinking": "hmm", "signature": "sig-1"},
            {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.txt"}}
        ]);
        let mut reply = Message::assistant(None, Some("hmm".to_string()), vec![]);
        if let Message::Assistant {
            provider_content, ..
        } = &mut reply
        {
            *provider_content = Some(verbatim.clone());
        }

        let conversation = ApiConversation::from_messages(&[reply]);

        assert_eq!(to_json(&conversation.messages)[0]["content"], verbatim);
    }

    #[test]
    fn results_of_parallel_tool_calls_share_one_user_message() {
        let conversation = ApiConversation::from_messages(&[
            Message::Tool {
                call_id: "toolu_1".to_string(),
                result: Ok("first".to_string()),
                duration_ms: None,
                at_ms: None,
            },
            Message::Tool {
                call_id: "toolu_2".to_string(),
                result: Err("boom".to_string()),
                duration_ms: None,
                at_ms: None,
            },
            Message::User("thanks".to_string()),
        ]);

        assert_eq!(
            to_json(&conversation.messages),
            serde_json::json!([
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "first", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_2", "content": "boom", "is_error": true}
                ]},
                {"role": "user", "content": [{"type": "text", "text": "thanks"}]}
            ])
        );
    }

    #[test]
    fn an_assistant_reply_that_said_nothing_is_not_replayed() {
        let conversation =
            ApiConversation::from_messages(&[Message::assistant(None, None, vec![])]);

        assert!(conversation.messages.is_empty());
    }

    #[test]
    fn tool_schema_becomes_an_input_schema_definition() {
        let schema = ToolSchema {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            parameters: HashMap::from([
                (
                    "city".to_string(),
                    ToolParameterSchema {
                        param_type: ToolParameterTypeSchema::String,
                        description: "City name".to_string(),
                        required: true,
                    },
                ),
                (
                    "days".to_string(),
                    ToolParameterSchema {
                        param_type: ToolParameterTypeSchema::Number,
                        description: "Forecast days".to_string(),
                        required: false,
                    },
                ),
            ]),
        };

        let value = to_json(&ApiTool::from(&schema));

        assert_eq!(value["name"], "get_weather");
        assert_eq!(value["description"], "Get the weather");
        assert_eq!(value["input_schema"]["type"], "object");
        assert_eq!(
            value["input_schema"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(
            value["input_schema"]["properties"]["days"]["type"],
            "number"
        );
        assert_eq!(
            value["input_schema"]["required"],
            serde_json::json!(["city"])
        );
    }

    #[test]
    fn request_omits_system_and_tools_when_there_are_none() {
        let request = ApiMessagesRequest {
            model: "m".to_string(),
            max_tokens: 10,
            system: None,
            messages: vec![],
            tools: vec![],
            stream: true,
        };

        let value = to_json(&request);

        assert!(value.get("system").is_none());
        assert!(value.get("tools").is_none());
        assert_eq!(value["max_tokens"], 10);
        assert_eq!(value["stream"], true);
    }

    // --- stream accumulation -------------------------------------------

    struct NullHandler;

    impl AgentEventsHandler for NullHandler {
        fn on_chunk(&self, _chunk: &AgentMessageChunk) {}
    }

    fn event(json: &str) -> ApiStreamEvent {
        serde_json::from_str(json).unwrap()
    }

    fn accumulate(events: &[&str]) -> MessagesStreamAccumulator {
        let mut acc = MessagesStreamAccumulator::new();
        for json in events {
            acc.push(event(json), &NullHandler).unwrap();
        }
        acc
    }

    #[test]
    fn text_blocks_add_up_to_the_reply_and_usage_combines_both_directions() {
        let acc = accumulate(&[
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ]);

        let (message, finish_reason) = acc.finish().unwrap();

        assert!(matches!(finish_reason, FinishReason::Stop));
        match message {
            Message::Assistant {
                text,
                usage,
                provider_content,
                ..
            } => {
                assert_eq!(text.as_deref(), Some("Hello"));
                let usage = usage.expect("usage");
                assert_eq!(usage.prompt_tokens, 10);
                assert_eq!(usage.completion_tokens, 5);
                assert_eq!(usage.total_tokens, 15);
                assert_eq!(
                    provider_content,
                    Some(serde_json::json!([{"type": "text", "text": "Hello"}]))
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn tool_use_block_reassembles_its_arguments_from_json_fragments() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"pa"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ris\", \"days\": 3}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        ]);

        let (message, finish_reason) = acc.finish().unwrap();

        assert!(matches!(finish_reason, FinishReason::ToolCalls));
        match message {
            Message::Assistant {
                text, tool_calls, ..
            } => {
                assert_eq!(text, None);
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "toolu_1");
                assert_eq!(tool_calls[0].name, "get_weather");
                assert_eq!(
                    tool_calls[0].arguments,
                    HashMap::from([
                        ("city".to_string(), "paris".to_string()),
                        ("days".to_string(), "3".to_string()),
                    ])
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn tool_call_without_arguments_streams_no_fragments_and_still_parses() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"list_files","input":{}}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant { tool_calls, .. } => {
                assert!(tool_calls[0].arguments.is_empty());
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn nested_tool_arguments_are_rejected_since_tools_only_take_strings() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"t","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"items\": [1, 2]}"}}"#,
        ]);

        assert!(acc.finish().is_err());
    }

    #[test]
    fn signed_thinking_is_kept_for_replay_and_shown_as_reasoning() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me see"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-1"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant {
                reasoning,
                provider_content,
                ..
            } => {
                assert_eq!(reasoning.as_deref(), Some("let me see"));
                assert_eq!(
                    provider_content,
                    Some(serde_json::json!([
                        {"type": "thinking", "thinking": "let me see", "signature": "sig-1"},
                        {"type": "text", "text": "done"}
                    ]))
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn redacted_thinking_is_kept_for_replay_but_never_shown() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"opaque"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant {
                reasoning,
                provider_content,
                ..
            } => {
                assert_eq!(reasoning, None);
                assert_eq!(
                    provider_content,
                    Some(serde_json::json!([{"type": "redacted_thinking", "data": "opaque"}]))
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn unsigned_thinking_is_shown_but_not_replayed() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"draft"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant {
                reasoning,
                provider_content,
                ..
            } => {
                assert_eq!(reasoning.as_deref(), Some("draft"));
                assert_eq!(provider_content, None);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn reasoning_and_response_phases_are_each_timed() {
        let mut acc = MessagesStreamAccumulator::new();
        acc.push(
            event(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thinking"}}"#,
            ),
            &NullHandler,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        acc.push(
            event(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}"#,
            ),
            &NullHandler,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant {
                reasoning_duration_ms,
                response_duration_ms,
                ..
            } => {
                assert!(reasoning_duration_ms.expect("reasoning timed") >= 10);
                assert!(response_duration_ms.expect("response timed") >= 10);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn whitespace_before_a_tool_call_is_not_mistaken_for_a_reply() {
        let acc = accumulate(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"\n"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"t","input":{}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant {
                text,
                provider_content,
                ..
            } => {
                assert_eq!(text, None);
                // Nor is it replayed: the API rejects whitespace-only text.
                assert_eq!(provider_content.unwrap().as_array().unwrap().len(), 1);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn stop_reasons_map_to_cooper_finish_reasons() {
        let finish = |stop_reason: &str| {
            let acc = accumulate(&[&format!(
                r#"{{"type":"message_delta","delta":{{"stop_reason":"{stop_reason}"}}}}"#
            )]);
            acc.finish().unwrap().1
        };

        assert!(matches!(finish("end_turn"), FinishReason::Stop));
        assert!(matches!(finish("stop_sequence"), FinishReason::Stop));
        assert!(matches!(finish("tool_use"), FinishReason::ToolCalls));
        assert!(matches!(finish("max_tokens"), FinishReason::Length));
        match finish("refusal") {
            FinishReason::Unknown(s) => assert_eq!(s, "refusal"),
            _ => panic!("expected unknown finish reason"),
        }
        match MessagesStreamAccumulator::new().finish().unwrap().1 {
            FinishReason::Unknown(s) => assert_eq!(s, "none"),
            _ => panic!("expected unknown finish reason"),
        }
    }

    #[test]
    fn an_error_event_aborts_the_reply_with_the_api_message() {
        let mut acc = MessagesStreamAccumulator::new();

        let result = acc.push(
            event(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            &NullHandler,
        );

        assert_eq!(
            result.unwrap_err().to_string(),
            "anthropic api error (overloaded_error): Overloaded"
        );
    }

    #[test]
    fn unfamiliar_events_and_blocks_are_ignored_rather_than_fatal() {
        let acc = accumulate(&[
            r#"{"type":"ping"}"#,
            r#"{"type":"some_future_event","payload":1}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"x"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":"ok"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ]);

        let (message, _) = acc.finish().unwrap();

        match message {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("ok")),
            _ => panic!("expected assistant message"),
        }
    }
}
