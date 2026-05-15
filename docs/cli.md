# CLI Reference

Complete reference for all `cooper` commands and flags.

---

## Global flags

These flags apply to the `chat` and `prompt` commands.

| Flag | Description |
|------|-------------|
| `--system-prompt <TEXT>` | Override the system prompt for this session |
| `--provider <NAME>` | Use a specific provider (must be defined in config) |
| `--model <ID>` | Use a specific model ID |
| `--no-agent-instructions` | Do not load `AGENTS.md` (or configured instructions file) |
| `--agent-instructions <FILE>` | Load a custom file as agent instructions instead of `AGENTS.md` |

---

## `cooper chat`

Start an interactive multi-turn chat session.

```bash
cooper chat
cooper chat --provider anthropic --model claude-opus-4-7
cooper chat --no-agent-instructions
cooper chat --agent-instructions CLAUDE.md
```

The session runs an agentic loop: the model can call tools, receive results, and continue until it produces a final text response. Up to 20 tool-call turns are allowed per user message.

Press `Ctrl+D` or `Ctrl+C` to exit.

---

## `cooper prompt`

Run a single prompt non-interactively and exit.

```bash
cooper prompt "Summarize the current codebase"
cooper prompt --skill rust-reviewer "Review src/main.rs"
cooper prompt --provider ollama --model qwen3:0.6b "What is 2+2?"
```

| Flag | Description |
|------|-------------|
| `--skill <NAME>` | Activate a skill before running the prompt |

All global flags from `chat` also apply.

---

## `cooper config`

Manage Cooper's configuration. Use `--project` to write to `./cooper.yml` instead of `~/.cooper/settings.yml`.

### `cooper config show`

Print resolved configuration (merged global + project).

```bash
cooper config show
cooper config show --project   # show project-level config only
```

### `cooper config set`

Set a configuration value.

```bash
cooper config set system-prompt "You are a senior engineer."
cooper config set default-provider ollama
cooper config set default-model qwen3:0.6b

# Write to project config instead of global
cooper config set default-model llama3.1 --project
```

**Keys**: `system-prompt`, `default-provider`, `default-model`

### `cooper config unset`

Remove a configuration value, reverting to the inherited default.

```bash
cooper config unset system-prompt
cooper config unset default-provider --project
```

---

## `cooper providers`

Manage model providers.

### `cooper providers list`

List all configured providers and their models.

```bash
cooper providers list
```

### `cooper providers add`

Add a new provider interactively or via flags.

```bash
cooper providers add
cooper providers add \
  --name my-provider \
  --base-url "https://api.example.com/v1" \
  --api openai-completions \
  --model gpt-4o \
  --api-key "sk-..." \
  --project
```

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Provider identifier |
| `--base-url <URL>` | API base URL |
| `--api <TYPE>` | API type: `openai-completions` or `anthropic-messages` |
| `--model <ID>` | Add a model (repeatable) |
| `--api-key <KEY>` | Optional API key (supports `${ENV_VAR}` syntax) |
| `--project` | Write to project config |

### `cooper providers remove`

Remove a provider by name.

```bash
cooper providers remove my-provider
cooper providers remove my-provider --project
```

### `cooper providers edit`

Open the provider's config in `$EDITOR`.

```bash
cooper providers edit ollama
```

### `cooper providers set`

Update a specific field on an existing provider.

```bash
cooper providers set ollama --base-url "http://localhost:11434/v1"
cooper providers set ollama --api-key "${MY_KEY}"
```

### `cooper providers models list`

List models for a provider.

```bash
cooper providers models list ollama
```

### `cooper providers models add`

Add a model to a provider.

```bash
cooper providers models add ollama qwen3:0.6b
cooper providers models add ollama llama3.1 --project
```

### `cooper providers models remove`

Remove a model from a provider.

```bash
cooper providers models remove ollama qwen3:0.6b
```

---

## `cooper context`

Inspect and manage context settings.

### `cooper context show`

Display the fully resolved context for the current session (system prompt, agent instructions state, loaded files, available tools, available skills).

```bash
cooper context show
```

### `cooper context agent-instructions`

```bash
cooper context agent-instructions enable
cooper context agent-instructions disable
cooper context agent-instructions set-file CLAUDE.md

# Target global config
cooper context agent-instructions disable --global
```

### `cooper context files`

Manage files that are automatically injected into context.

```bash
cooper context files list
cooper context files add README.md
cooper context files add docs/architecture.md
cooper context files remove README.md
cooper context files clear
```

Use `--global` to operate on `~/.cooper/settings.yml`.

### `cooper context tools`

Control which tools the agent can use.

```bash
cooper context tools list
cooper context tools allow read_file
cooper context tools deny execute_command
cooper context tools reset             # allow all tools (default)
```

Use `--global` for global scope.

### `cooper context skills`

Control which skills are available.

```bash
cooper context skills list
cooper context skills allow rust-reviewer
cooper context skills deny untrusted-skill
cooper context skills reset            # allow all skills (default)
```

Use `--global` for global scope.

---

## `cooper tools`

### `cooper tools list`

List all available tools (built-in + custom).

```bash
cooper tools list
```

### `cooper tools run`

Execute a tool directly without involving a model.

```bash
cooper tools run list_files
cooper tools run read_file --path src/main.rs
cooper tools run search-files --pattern "fn main" --glob "*.rs"
```

Parameter names match what is defined in the tool's YAML definition. Pass each parameter as `--<name> <value>`.

---

## `cooper skills`

### `cooper skills list`

List all available skills (project + global).

```bash
cooper skills list
```
