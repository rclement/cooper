# Cooper

> *...damn good agent*

A minimal Rust AI agent CLI harness with tool-calling support.

## Build & Run

```bash
cargo build --release
cargo run -- prompt "your message here"
```

## Usage

```bash
cooper prompt "<text>" [-p provider] [-m model] [-i agent_instructions.md] [-c context_file ...]
```

- `-p, --provider` — provider name (defaults to config)
- `-m, --model` — model name (defaults to config)
- `-i, --agent-instructions` — file with extra agent instructions
- `-c, --context-file` — one or more files added as context

## Configuration

Requires `~/.cooper/settings.yml` with `default_provider`, `default_model`, and a `providers` map (type, base URL, API key, models). See [src/config.rs](src/config.rs) for the schema.

```yaml
default_provider: openai
default_model: gpt-4o-mini

providers:
  openai:
    provider_type: openai-completions
    base_url: https://api.openai.com/v1
    api_key: sk-...
    models:
      - id: gpt-4o-mini
      - id: gpt-4o
```

Currently supported provider type: `openai-completions`.

## Built-in Tools

- `list_files`
- `read_file`
- `exec_cmd`

## Layout

- [src/main.rs](src/main.rs) — entrypoint
- [src/cli.rs](src/cli.rs) — CLI args and `prompt` command
- [src/agent.rs](src/agent.rs) — agent loop / streaming
- [src/config.rs](src/config.rs) — settings loading
- [src/providers/](src/providers/) — LLM provider implementations
- [src/tools.rs](src/tools.rs) — built-in tools
