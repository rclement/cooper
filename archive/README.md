# Cooper

> *...damn good agent*

Cooper is a model-agnostic AI agent CLI. Point it at any OpenAI-compatible or Anthropic-compatible API — local models via Ollama, cloud models, or anything in between — and get an interactive assistant that can read your files, run commands, and follow project-specific instructions.

## Quick start

```bash
# Interactive chat session
cooper chat

# One-shot prompt
cooper prompt "Summarize the current codebase"

# Use a named skill
cooper prompt --skill reviewer "Review the current branch"
```

## Features

**Model agnostic** — connects to any OpenAI Chat Completions or Anthropic Messages API. Works with Ollama (local), OpenRouter, Anthropic, or any compatible endpoint.

**Project-aware context** — drop an `AGENTS.md` in your project root and Cooper picks it up automatically as agent instructions ([agents.md spec](https://agents.md/)). Add files to context via `cooper.yml`.

**Built-in tools** — the agent can list and read files, write and edit files, and run shell commands out of the box.

**Custom tools via YAML** — extend the toolset with simple YAML definitions. A tool is just a name, parameters, and a shell command. No code required. Tools live in `.agents/tools/` (project) or `~/.cooper/tools/` (global).

**Skills** — reusable system prompt fragments that change the agent's behavior for a task. Load them on demand (`--skill <name>`) or let the agent activate them mid-session. Skills follow the [Agent Skills Specification](https://agentskills.io) and live in `.agents/skills/` (project) or `~/.cooper/skills/` (global).

**Fine-grained context control** — decide exactly which files, tools, and skills are available per project via `cooper.yml` or the `context` subcommand.

**Session logging** — every session is saved to `~/.cooper/sessions/` as JSONL, including full message history, token usage, and timing.

**Global + project configuration** — global defaults in `~/.cooper/settings.yml`, project overrides in `cooper.yml`. All settings are mergeable.

## Configuration at a glance

**Global** (`~/.cooper/settings.yml`):
```yaml
system_prompt: "You are a helpful AI assistant"
default_provider: "ollama"
default_model: "qwen3:0.6b"

providers:
  ollama:
    base_url: "http://localhost:11434/v1"
    api: "openai-completions"
    api_key: "ollama"
    models:
      - id: "qwen3:0.6b"
```

**Project** (`cooper.yml` — overrides global):
```yaml
system_prompt: "You are a senior Rust engineer."

context:
  files: [README.md, CONTRIBUTING.md]
  allowed_tools: [list_files, read_file, search-files]
  allowed_skills: [rust-reviewer]
```

## Documentation

| Topic | Description |
|-------|-------------|
| [CLI reference](docs/cli.md) | All commands, subcommands, and flags |
| [Configuration](docs/configuration.md) | Global and project settings, all options |
| [Context & agent instructions](docs/context.md) | How context is assembled and how to control it |
| [Tools](docs/tools.md) | Built-in tools and how to add custom ones |
| [Skills](docs/skills.md) | Creating and activating skills |
| [Providers](docs/providers.md) | Connecting to model providers |
| [Sessions](docs/sessions.md) | Session storage and inspection |
