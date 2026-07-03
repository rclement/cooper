use serde::Serialize;

use crate::fixture::FixtureResponse;

/// The over-the-wire shapes a real OpenAI-chat-completions-compatible server
/// sends per SSE `data:` line. Kept separate from anything the client parses
/// with — this crate plays the server, not the client.
#[derive(Serialize)]
struct ApiStreamChunk {
    id: String,
    choices: Vec<ApiStreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<ApiUsage>,
}

#[derive(Serialize)]
struct ApiStreamChoice {
    index: u64,
    delta: ApiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Default, Serialize)]
struct ApiStreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ApiStreamToolCallDelta>>,
}

#[derive(Serialize)]
struct ApiStreamToolCallDelta {
    index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    function: ApiStreamToolCallFunction,
}

#[derive(Serialize)]
struct ApiStreamToolCallFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

#[derive(Clone, Serialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<&crate::fixture::FixtureUsage> for ApiUsage {
    fn from(u: &crate::fixture::FixtureUsage) -> Self {
        ApiUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

/// Splits text into fixed-size chunks (by char count), so streaming a fixture
/// exercises the client's incremental SSE parsing instead of arriving in one
/// blob — while still reconstructing to the exact original text.
const CHUNK_SIZE: usize = 8;

fn chunk_str(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![];
    }
    chars
        .chunks(CHUNK_SIZE)
        .map(|c| c.iter().collect())
        .collect()
}

fn make_chunk(
    id: &str,
    delta: ApiStreamDelta,
    finish_reason: Option<String>,
    usage: Option<ApiUsage>,
) -> String {
    let chunk = ApiStreamChunk {
        id: id.to_string(),
        choices: vec![ApiStreamChoice {
            index: 0,
            delta,
            finish_reason,
        }],
        usage,
    };
    serde_json::to_string(&chunk).expect("ApiStreamChunk always serializes")
}

/// Builds the ordered sequence of SSE `data:` payloads for one fixture
/// response (not including the trailing `data: [DONE]` line).
pub fn build_sse_payloads(response: &FixtureResponse, id: &str) -> Vec<String> {
    let mut payloads = vec![make_chunk(
        id,
        ApiStreamDelta {
            role: Some("assistant".to_string()),
            ..Default::default()
        },
        None,
        None,
    )];

    if let Some(reasoning) = &response.reasoning {
        for piece in chunk_str(reasoning) {
            payloads.push(make_chunk(
                id,
                ApiStreamDelta {
                    reasoning: Some(piece),
                    ..Default::default()
                },
                None,
                None,
            ));
        }
    }

    if let Some(text) = &response.text {
        for piece in chunk_str(text) {
            payloads.push(make_chunk(
                id,
                ApiStreamDelta {
                    content: Some(piece),
                    ..Default::default()
                },
                None,
                None,
            ));
        }
    }

    for (index, tool_call) in response.tool_calls.iter().enumerate() {
        payloads.push(make_chunk(
            id,
            ApiStreamDelta {
                tool_calls: Some(vec![ApiStreamToolCallDelta {
                    index,
                    id: Some(tool_call.id.clone()),
                    function: ApiStreamToolCallFunction {
                        name: Some(tool_call.name.clone()),
                        arguments: None,
                    },
                }]),
                ..Default::default()
            },
            None,
            None,
        ));

        let arguments =
            serde_json::to_string(&tool_call.arguments).expect("arguments map always serializes");
        payloads.push(make_chunk(
            id,
            ApiStreamDelta {
                tool_calls: Some(vec![ApiStreamToolCallDelta {
                    index,
                    id: None,
                    function: ApiStreamToolCallFunction {
                        name: None,
                        arguments: Some(arguments),
                    },
                }]),
                ..Default::default()
            },
            None,
            None,
        ));
    }

    payloads.push(make_chunk(
        id,
        ApiStreamDelta::default(),
        Some(response.finish_reason().to_string()),
        response.usage.as_ref().map(ApiUsage::from),
    ));

    payloads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureToolCall, FixtureUsage};
    use std::collections::HashMap;

    fn parse_all(payloads: &[String]) -> Vec<serde_json::Value> {
        payloads
            .iter()
            .map(|p| serde_json::from_str(p).unwrap())
            .collect()
    }

    #[test]
    fn text_only_response_reconstructs_and_ends_with_stop() {
        let response = FixtureResponse {
            reasoning: None,
            text: Some("PONG".to_string()),
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };

        let payloads = build_sse_payloads(&response, "chatcmpl-1");
        let parsed = parse_all(&payloads);

        let reconstructed: String = parsed
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(reconstructed, "PONG");

        let last = parsed.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn long_text_is_split_into_multiple_chunks() {
        let response = FixtureResponse {
            reasoning: None,
            text: Some("this is a longer sentence than one chunk".to_string()),
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };

        let payloads = build_sse_payloads(&response, "chatcmpl-1");
        let parsed = parse_all(&payloads);

        let content_chunks: Vec<&str> = parsed
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert!(content_chunks.len() > 1);
        assert_eq!(
            content_chunks.concat(),
            "this is a longer sentence than one chunk"
        );
    }

    #[test]
    fn tool_call_streams_id_name_then_arguments_and_finish_reason() {
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

        let payloads = build_sse_payloads(&response, "chatcmpl-1");
        let parsed = parse_all(&payloads);

        let tool_call_chunks: Vec<&serde_json::Value> = parsed
            .iter()
            .filter(|c| !c["choices"][0]["delta"]["tool_calls"].is_null())
            .collect();
        assert_eq!(tool_call_chunks.len(), 2);
        assert_eq!(
            tool_call_chunks[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "call-1"
        );
        assert_eq!(
            tool_call_chunks[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "exec_cmd"
        );
        assert!(tool_call_chunks[1]["choices"][0]["delta"]["tool_calls"][0]["id"].is_null());

        let args_json =
            tool_call_chunks[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap();
        let args: HashMap<String, String> = serde_json::from_str(args_json).unwrap();
        assert_eq!(args.get("command"), Some(&"echo PONG".to_string()));

        let last = parsed.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn usage_is_attached_to_final_chunk() {
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

        let payloads = build_sse_payloads(&response, "chatcmpl-1");
        let parsed = parse_all(&payloads);
        let last = parsed.last().unwrap();

        assert_eq!(last["usage"]["prompt_tokens"], 10);
        assert_eq!(last["usage"]["completion_tokens"], 5);
        assert_eq!(last["usage"]["total_tokens"], 15);
    }
}
