pub mod anthropic_messages;
pub mod openai_completions;

use crate::types::{ApiType, Message, OutputChunk, ToolCall};
use anyhow::Result;
use serde_json::Value;

// ── ThinkParser ───────────────────────────────────────────────────────────────

/// Parses a stream of content strings, splitting off `<think>…</think>` blocks.
/// Handles tags that arrive split across multiple chunks.
#[derive(Default)]
pub struct ThinkParser {
    in_think: bool,
    partial_tag: String,
}

impl ThinkParser {
    pub fn feed(&mut self, text: &str) -> Vec<OutputChunk> {
        let mut chunks: Vec<OutputChunk> = Vec::new();
        let mut buf = String::new();

        let mut emit = |in_think: bool, s: String| {
            if s.is_empty() {
                return;
            }
            if in_think {
                chunks.push(OutputChunk::Thinking { text: s });
            } else {
                chunks.push(OutputChunk::Content { text: s });
            }
        };

        for ch in text.chars() {
            if !self.partial_tag.is_empty() {
                self.partial_tag.push(ch);
                if ch == '>' {
                    let tag = std::mem::take(&mut self.partial_tag);
                    match tag.as_str() {
                        "<think>" => {
                            emit(self.in_think, std::mem::take(&mut buf));
                            self.in_think = true;
                        }
                        "</think>" => {
                            emit(self.in_think, std::mem::take(&mut buf));
                            self.in_think = false;
                        }
                        other => {
                            buf.push_str(other);
                        }
                    }
                }
            } else if ch == '<' {
                self.partial_tag.push('<');
            } else {
                buf.push(ch);
            }
        }

        emit(self.in_think, buf);
        chunks
    }

    pub fn flush(&mut self) -> Vec<OutputChunk> {
        let tag = std::mem::take(&mut self.partial_tag);
        if tag.is_empty() {
            return vec![];
        }
        if self.in_think {
            vec![OutputChunk::Thinking { text: tag }]
        } else {
            vec![OutputChunk::Content { text: tag }]
        }
    }
}

// ── Inline tool call parser ───────────────────────────────────────────────────

/// Parses text-based tool call formats that some open-source models emit as
/// content instead of using the structured `tool_calls` API field.
///
/// Handles:
/// - `<function=NAME><parameter=K>V</parameter>…</function>` (Qwen/Hermes XML)
/// - `<tool_call>{"name":"…","arguments":{…}}</tool_call>` (Hermes JSON)
pub fn parse_inline_tool_calls(content: &str) -> Option<Vec<ToolCall>> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut rest = content;

    loop {
        if let Some(offset) = rest.find("<function=") {
            let tail = &rest[offset..];
            let name_close = tail.find('>')?;
            let func_name = tail["<function=".len()..name_close].trim().to_string();

            let body_start = name_close + 1;
            let func_end = tail[body_start..].find("</function>")?;
            let func_body = &tail[body_start..body_start + func_end];

            let mut args = serde_json::Map::new();
            let mut pb = func_body;
            while let Some(pidx) = pb.find("<parameter=") {
                let pt = &pb[pidx..];
                let pclose = pt.find('>')?;
                let pname = pt["<parameter=".len()..pclose].trim().to_string();
                let val_start = pclose + 1;
                let val_end = pt[val_start..].find("</parameter>")?;
                let pval = pt[val_start..val_start + val_end].trim().to_string();
                args.insert(pname, serde_json::Value::String(pval));
                pb = &pt[val_start + val_end + "</parameter>".len()..];
            }

            tool_calls.push(ToolCall {
                id: format!("tc_{}", tool_calls.len()),
                name: func_name,
                arguments: serde_json::to_string(&serde_json::Value::Object(args))
                    .unwrap_or_else(|_| "{}".to_string()),
            });

            rest = &tail[body_start + func_end + "</function>".len()..];
            let rt = rest.trim_start();
            if rt.starts_with("</tool_call>") {
                let skip = rest.len() - rt.len() + "</tool_call>".len();
                rest = &rest[skip..];
            }
        } else if let Some(offset) = rest.find("<tool_call>") {
            let tail = &rest[offset + "<tool_call>".len()..];
            let end = tail.find("</tool_call>")?;
            let json_str = tail[..end].trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(name) = val["name"].as_str() {
                    let arguments = val
                        .get("arguments")
                        .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(ToolCall {
                        id: format!("tc_{}", tool_calls.len()),
                        name: name.to_string(),
                        arguments,
                    });
                }
            }
            rest = &tail[end + "</tool_call>".len()..];
        } else {
            break;
        }
    }

    if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ThinkParser helpers ───────────────────────────────────────────────────

    fn contents(chunks: &[OutputChunk]) -> Vec<String> {
        chunks.iter().filter_map(|c| {
            if let OutputChunk::Content { text } = c { Some(text.clone()) } else { None }
        }).collect()
    }

    fn thinkings(chunks: &[OutputChunk]) -> Vec<String> {
        chunks.iter().filter_map(|c| {
            if let OutputChunk::Thinking { text } = c { Some(text.clone()) } else { None }
        }).collect()
    }

    // ── ThinkParser ───────────────────────────────────────────────────────────

    #[test]
    fn think_parser_plain_content() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("hello world");
        assert_eq!(contents(&chunks), vec!["hello world"]);
        assert!(thinkings(&chunks).is_empty());
    }

    #[test]
    fn think_parser_empty_input() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn think_parser_full_think_block() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("<think>internal</think>answer");
        assert_eq!(thinkings(&chunks), vec!["internal"]);
        assert_eq!(contents(&chunks), vec!["answer"]);
    }

    #[test]
    fn think_parser_content_before_and_after_think() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("before<think>inside</think>after");
        assert_eq!(contents(&chunks), vec!["before", "after"]);
        assert_eq!(thinkings(&chunks), vec!["inside"]);
    }

    #[test]
    fn think_parser_tag_split_across_chunks() {
        let mut p = ThinkParser::default();
        let c1 = p.feed("<thi");
        assert!(c1.is_empty(), "incomplete tag must not emit yet");
        let c2 = p.feed("nk>content</think>");
        assert_eq!(thinkings(&c2), vec!["content"]);
        assert!(contents(&c2).is_empty());
    }

    #[test]
    fn think_parser_unknown_tag_emitted_as_content() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("<div>hello</div>");
        let joined: String = contents(&chunks).concat();
        assert_eq!(joined, "<div>hello</div>");
    }

    #[test]
    fn think_parser_flush_empty() {
        let mut p = ThinkParser::default();
        let _ = p.feed("plain");
        let chunks = p.flush();
        assert!(chunks.is_empty());
    }

    #[test]
    fn think_parser_flush_partial_tag_outside_think() {
        let mut p = ThinkParser::default();
        let _ = p.feed("text<");
        let chunks = p.flush();
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], OutputChunk::Content { text } if text == "<"));
    }

    #[test]
    fn think_parser_flush_partial_tag_inside_think() {
        let mut p = ThinkParser::default();
        let _ = p.feed("<think>inside<");
        let chunks = p.flush();
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], OutputChunk::Thinking { text } if text == "<"));
    }

    #[test]
    fn think_parser_multiple_think_blocks() {
        let mut p = ThinkParser::default();
        let chunks = p.feed("<think>a</think>x<think>b</think>y");
        assert_eq!(thinkings(&chunks), vec!["a", "b"]);
        assert_eq!(contents(&chunks), vec!["x", "y"]);
    }

    // ── parse_inline_tool_calls ───────────────────────────────────────────────

    #[test]
    fn inline_none_when_no_marker() {
        assert!(parse_inline_tool_calls("no tools here").is_none());
        assert!(parse_inline_tool_calls("").is_none());
    }

    #[test]
    fn inline_function_xml_single() {
        let input = r#"<function=search><parameter=query>rust lang</parameter></function>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(args["query"], "rust lang");
    }

    #[test]
    fn inline_function_xml_no_parameters() {
        let input = r#"<function=ping></function>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ping");
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn inline_function_xml_multiple_params() {
        let input = r#"<function=write><parameter=path>foo.txt</parameter><parameter=content>hello</parameter></function>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(args["path"], "foo.txt");
        assert_eq!(args["content"], "hello");
    }

    #[test]
    fn inline_tool_call_json_format() {
        let input = r#"<tool_call>{"name":"run","arguments":{"cmd":"ls"}}</tool_call>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "run");
        let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(args["cmd"], "ls");
    }

    #[test]
    fn inline_tool_call_json_no_arguments_field() {
        let input = r#"<tool_call>{"name":"ping"}</tool_call>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn inline_tool_call_json_missing_name_skipped() {
        let input = r#"<tool_call>{"foo":"bar"}</tool_call>"#;
        assert!(parse_inline_tool_calls(input).is_none());
    }

    #[test]
    fn inline_tool_call_json_malformed_json_skipped() {
        let input = r#"<tool_call>not json</tool_call>"#;
        assert!(parse_inline_tool_calls(input).is_none());
    }

    #[test]
    fn inline_multiple_function_xml_calls() {
        let input = concat!(
            r#"<function=a><parameter=x>1</parameter></function>"#,
            r#"<function=b><parameter=y>2</parameter></function>"#,
        );
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
        assert_eq!(calls[0].id, "tc_0");
        assert_eq!(calls[1].id, "tc_1");
    }

    #[test]
    fn inline_ids_are_unique_per_call() {
        let input = r#"<tool_call>{"name":"a"}</tool_call><tool_call>{"name":"b"}</tool_call>"#;
        let calls = parse_inline_tool_calls(input).unwrap();
        assert_ne!(calls[0].id, calls[1].id);
    }
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

pub async fn call(
    api_type: &ApiType,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<Message>,
    tools: &[Value],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<(Message, Option<crate::types::Usage>)> {
    match api_type {
        ApiType::OpenaiCompletions => {
            openai_completions::call(base_url, api_key, model, messages, tools, on_chunk).await
        }
        ApiType::AnthropicMessages => {
            anthropic_messages::call(base_url, api_key, model, messages, tools, on_chunk).await
        }
    }
}
