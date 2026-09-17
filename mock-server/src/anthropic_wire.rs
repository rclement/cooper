use serde_json::{Value, json};

use crate::fixture::FixtureResponse;
use crate::wire::chunk_str;

/// Maps an OpenAI-style `finish_reason` (as produced by
/// `FixtureResponse::finish_reason`) onto the Anthropic Messages API's
/// `stop_reason` vocabulary.
fn stop_reason(finish_reason: &str) -> String {
    match finish_reason {
        "stop" => "end_turn".to_string(),
        "tool_calls" => "tool_use".to_string(),
        "length" => "max_tokens".to_string(),
        other => other.to_string(),
    }
}

fn event(name: &str, data: Value) -> (String, String) {
    (
        name.to_string(),
        serde_json::to_string(&data).expect("event payload always serializes"),
    )
}

/// Builds the ordered sequence of Anthropic Messages API SSE `(event, data)`
/// pairs for one fixture response. Unlike the OpenAI-compatible stream, every
/// Anthropic event carries an explicit `event:` name alongside its `data:`
/// line, and the stream ends with `message_stop` rather than a `[DONE]`
/// sentinel.
pub fn build_anthropic_sse_events(response: &FixtureResponse, id: &str) -> Vec<(String, String)> {
    let mut events = Vec::new();

    events.push(event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": "mock-model",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": response.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                    "output_tokens": 0,
                },
            },
        }),
    ));

    let mut index: u64 = 0;

    if let Some(reasoning) = &response.reasoning {
        events.push(event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "thinking", "thinking": "", "signature": ""},
            }),
        ));

        for piece in chunk_str(reasoning) {
            events.push(event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "thinking_delta", "thinking": piece},
                }),
            ));
        }

        events.push(event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "signature_delta", "signature": "mock-signature"},
            }),
        ));

        events.push(event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));

        index += 1;
    }

    if let Some(text) = &response.text {
        events.push(event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "text", "text": ""},
            }),
        ));

        for piece in chunk_str(text) {
            events.push(event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": piece},
                }),
            ));
        }

        events.push(event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));

        index += 1;
    }

    for tool_call in &response.tool_calls {
        events.push(event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_call.id,
                    "name": tool_call.name,
                    "input": {},
                },
            }),
        ));

        let arguments =
            serde_json::to_string(&tool_call.arguments).expect("arguments map always serializes");
        for piece in chunk_str(&arguments) {
            events.push(event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": piece},
                }),
            ));
        }

        events.push(event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));

        index += 1;
    }

    events.push(event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason(response.finish_reason()),
                "stop_sequence": null,
            },
            "usage": {
                "output_tokens": response.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
            },
        }),
    ));

    events.push(event("message_stop", json!({"type": "message_stop"})));

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureToolCall, FixtureUsage};
    use std::collections::HashMap;

    fn parse_all(events: &[(String, String)]) -> Vec<(String, Value)> {
        events
            .iter()
            .map(|(name, data)| (name.clone(), serde_json::from_str(data).unwrap()))
            .collect()
    }

    #[test]
    fn a_text_only_response_streams_as_one_text_block_and_ends_with_end_turn() {
        let response = FixtureResponse {
            reasoning: None,
            text: Some("PONG".to_string()),
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };

        let events = build_anthropic_sse_events(&response, "msg-1");
        let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        let parsed = parse_all(&events);
        assert_eq!(parsed[1].1["content_block"]["type"], "text");

        let reconstructed: String = parsed
            .iter()
            .filter(|(n, _)| n == "content_block_delta")
            .filter_map(|(_, v)| v["delta"]["text"].as_str())
            .collect();
        assert_eq!(reconstructed, "PONG");

        let message_delta = &parsed[parsed.len() - 2].1;
        assert_eq!(message_delta["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn reasoning_streams_as_a_thinking_block_before_the_text() {
        let response = FixtureResponse {
            reasoning: Some("thinking it over".to_string()),
            text: Some("PONG".to_string()),
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };

        let events = build_anthropic_sse_events(&response, "msg-1");
        let parsed = parse_all(&events);

        let block_starts: Vec<&(String, Value)> = parsed
            .iter()
            .filter(|(n, _)| n == "content_block_start")
            .collect();
        assert_eq!(block_starts.len(), 2);
        assert_eq!(block_starts[0].1["content_block"]["type"], "thinking");
        assert_eq!(block_starts[0].1["index"], 0);
        assert_eq!(block_starts[1].1["content_block"]["type"], "text");
        assert_eq!(block_starts[1].1["index"], 1);

        let thinking_deltas: Vec<&Value> = parsed
            .iter()
            .filter(|(n, v)| n == "content_block_delta" && v["index"] == 0)
            .map(|(_, v)| v)
            .collect();
        let reconstructed: String = thinking_deltas
            .iter()
            .filter_map(|v| v["delta"]["thinking"].as_str())
            .collect();
        assert_eq!(reconstructed, "thinking it over");
        assert_eq!(
            thinking_deltas.last().unwrap()["delta"]["signature"],
            "mock-signature"
        );
    }

    #[test]
    fn tool_calls_become_tool_use_blocks_whose_partial_json_reassembles_to_the_arguments() {
        let response = FixtureResponse {
            reasoning: None,
            text: None,
            tool_calls: vec![FixtureToolCall {
                id: "call-1".to_string(),
                name: "exec_cmd".to_string(),
                arguments: HashMap::from([("command".to_string(), "echo PONG".to_string())]),
            }],
            finish_reason: None,
            usage: None,
        };

        let events = build_anthropic_sse_events(&response, "msg-1");
        let parsed = parse_all(&events);

        let start = &parsed
            .iter()
            .find(|(n, _)| n == "content_block_start")
            .unwrap()
            .1;
        assert_eq!(start["content_block"]["type"], "tool_use");
        assert_eq!(start["content_block"]["id"], "call-1");
        assert_eq!(start["content_block"]["name"], "exec_cmd");

        let partial_json: String = parsed
            .iter()
            .filter(|(n, _)| n == "content_block_delta")
            .filter_map(|(_, v)| v["delta"]["partial_json"].as_str())
            .collect();
        let args: HashMap<String, String> = serde_json::from_str(&partial_json).unwrap();
        assert_eq!(args.get("command"), Some(&"echo PONG".to_string()));

        let message_delta = &parsed.iter().find(|(n, _)| n == "message_delta").unwrap().1;
        assert_eq!(message_delta["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn usage_is_split_between_message_start_input_tokens_and_message_delta_output_tokens() {
        let response = FixtureResponse {
            reasoning: None,
            text: Some("hi".to_string()),
            tool_calls: vec![],
            finish_reason: None,
            usage: Some(FixtureUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };

        let events = build_anthropic_sse_events(&response, "msg-1");
        let parsed = parse_all(&events);

        let message_start = &parsed[0].1;
        assert_eq!(message_start["message"]["usage"]["input_tokens"], 10);

        let message_delta = &parsed.iter().find(|(n, _)| n == "message_delta").unwrap().1;
        assert_eq!(message_delta["usage"]["output_tokens"], 5);
    }

    #[test]
    fn no_reasoning_no_text_no_tool_calls_still_ends_cleanly() {
        let response = FixtureResponse {
            reasoning: None,
            text: None,
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };

        let events = build_anthropic_sse_events(&response, "msg-1");
        let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(
            names,
            vec!["message_start", "message_delta", "message_stop"]
        );
    }
}
