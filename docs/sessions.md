# Sessions

Every Cooper session is automatically saved to disk. This lets you revisit past conversations, audit tool calls, and inspect token usage.

---

## Storage location

```
~/.cooper/sessions/<project-slug>/<session-id>.jsonl
```

- **project-slug** — the current working directory path with `/` replaced by `-`  
  Example: `/home/alice/projects/myapp` → `home-alice-projects-myapp`
- **session-id** — a UUID v4 generated at session start

Each session is a single `.jsonl` file (one JSON object per line).

---

## File format

Sessions are stored in JSONL format. Each line is a JSON object of one of three types:

### Session header (first line)

```json
{
  "type": "session",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "provider": "ollama",
  "model": "qwen3:0.6b",
  "project": "/home/alice/projects/myapp",
  "started_at": "2025-11-15T10:23:01Z"
}
```

### Request entry

Recorded before each model call. Contains the full message history sent to the model, including the system prompt.

```json
{
  "type": "request",
  "messages": [
    { "role": "system", "content": "You are a helpful AI assistant" },
    { "role": "user", "content": "Summarize the codebase" }
  ]
}
```

### Response entry

Recorded after each model response, including timing and token usage.

```json
{
  "type": "response",
  "thinking": null,
  "message": {
    "role": "assistant",
    "content": "The codebase is a Rust CLI tool that..."
  },
  "duration_ms": 1423,
  "usage": {
    "prompt_tokens": 312,
    "completion_tokens": 87,
    "total_tokens": 399
  }
}
```

The `thinking` field contains extended reasoning content when the model emits `<think>` blocks (DeepSeek-R1, etc.).

---

## Multi-turn sessions

A multi-turn chat session produces interleaved request/response entries — one pair per user message. The `request` entry always contains the *full* accumulated message history at that point, so you can reconstruct the exact context that was sent to the model for any turn.

---

## Tool calls in sessions

Tool calls appear inside the assistant message's `tool_calls` array. Tool results are recorded as messages with `role: "tool"` in the next request entry. This means you can trace the full tool execution chain by reading the request/response pairs in sequence.

---

## Inspecting sessions

Sessions are plain JSONL — any tool that reads JSON works:

```bash
# View a session
cat ~/.cooper/sessions/home-alice-projects-myapp/*.jsonl

# Pretty-print
cat session.jsonl | jq .

# Show only responses with token usage
cat session.jsonl | jq 'select(.type == "response") | .usage'

# Show all tool calls made during a session
cat session.jsonl | jq 'select(.type == "response") | .message.tool_calls[]?'

# Total tokens across all turns
cat session.jsonl | jq '[select(.type == "response") | .usage.total_tokens] | add'
```
