# Configuration

Cooper uses two configuration files that are merged at runtime, with the project file taking precedence over the global one.

| Scope | Path | Purpose |
|-------|------|---------|
| Global | `~/.cooper/settings.yml` | Personal defaults, provider credentials |
| Project | `./cooper.yml` (current directory) | Per-project overrides |

---

## Full schema

```yaml
# ──────────────────────────────────────────────
# Agent behaviour
# ──────────────────────────────────────────────

# System prompt sent to the model on every session.
# Project value overrides global value.
system_prompt: "You are a helpful AI assistant"

# ──────────────────────────────────────────────
# Default provider / model
# ──────────────────────────────────────────────

# Name of the provider to use when none is specified on the CLI.
default_provider: "ollama"

# Model ID to use when none is specified on the CLI.
default_model: "qwen3:0.6b"

# ──────────────────────────────────────────────
# Providers
# ──────────────────────────────────────────────

providers:
  # Key is the provider name referenced by default_provider / --provider
  ollama:
    base_url: "http://localhost:11434/v1"
    api: "openai-completions"   # or "anthropic-messages"
    api_key: "ollama"           # optional; supports ${ENV_VAR}
    models:
      - id: "qwen3:0.6b"
      - id: "llama3.1"

  anthropic:
    base_url: "https://api.anthropic.com"
    api: "anthropic-messages"
    api_key: "${ANTHROPIC_API_KEY}"
    models:
      - id: "claude-sonnet-4-6"
      - id: "claude-opus-4-7"

# ──────────────────────────────────────────────
# Context
# ──────────────────────────────────────────────

context:
  # Agent instructions file injected into the system prompt.
  # true (default) = load AGENTS.md from cwd
  # false          = disable
  # "filename.md"  = load a specific file
  agent_instructions: true

  # Files whose contents are injected into the system prompt at session start.
  files:
    - README.md
    - docs/architecture.md

  # Allowlist of tool names the agent may call.
  # null (default) = all tools available
  # []             = no tools
  # ["name", ...]  = only listed tools
  allowed_tools:
    - list_files
    - read_file
    - search-files

  # Allowlist of skill names that can be activated.
  # null (default) = all skills available
  # []             = no skills
  # ["name", ...]  = only listed skills
  allowed_skills:
    - rust-reviewer
```

---

## Merge rules

When both global and project configs exist, they are merged as follows:

| Setting | Merge behaviour |
|---------|----------------|
| `system_prompt` | Project wins; falls back to global |
| `default_provider` | Project wins; falls back to global |
| `default_model` | Project wins; falls back to global |
| `providers` | Union — both sets are available; project entry wins on name collision |
| `context.agent_instructions` | Project wins; falls back to global |
| `context.files` | Project wins; falls back to global |
| `context.allowed_tools` | Project wins; falls back to global |
| `context.allowed_skills` | Project wins; falls back to global |

---

## Environment variable expansion

Any config value that contains `${VAR_NAME}` is expanded from the environment at load time. This is most useful for API keys:

```yaml
providers:
  openrouter:
    api_key: "${OPENROUTER_API_KEY}"
```

Unexpanded variables (missing env var) are left as-is and may cause authentication failures at runtime.

---

## CLI management

The `cooper config` subcommand provides a programmatic interface:

```bash
# View resolved config
cooper config show

# Set global default model
cooper config set default-model qwen3:0.6b

# Set project system prompt
cooper config set system-prompt "You are a Rust expert." --project

# Remove a project-level override
cooper config unset default-provider --project
```

See [CLI reference — config](cli.md#cooper-config) for the full command listing.
