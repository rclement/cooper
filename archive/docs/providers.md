# Providers

Cooper is model-agnostic. A *provider* is a named connection to a model server. You can configure as many providers as you like and switch between them with `--provider`.

---

## Supported API types

| API type | Value in config | Compatible with |
|----------|----------------|-----------------|
| OpenAI Chat Completions | `openai-completions` | Ollama, OpenRouter, OpenAI, any OpenAI-compatible server |
| Anthropic Messages | `anthropic-messages` | Anthropic API directly |

---

## Configuration

Providers are defined in `~/.cooper/settings.yml` (global) or `cooper.yml` (project). Both files are merged at runtime.

```yaml
providers:
  ollama:
    base_url: "http://localhost:11434/v1"
    api: "openai-completions"
    api_key: "ollama"          # Ollama ignores the key but one must be present
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

  openrouter:
    base_url: "https://openrouter.ai/api/v1"
    api: "openai-completions"
    api_key: "${OPENROUTER_API_KEY}"
    models:
      - id: "meta-llama/llama-3.1-70b-instruct"
      - id: "deepseek/deepseek-r1"

default_provider: "ollama"
default_model: "qwen3:0.6b"
```

**Provider fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `base_url` | yes | API endpoint root URL |
| `api` | yes | `openai-completions` or `anthropic-messages` |
| `api_key` | no | API key; supports `${ENV_VAR}` expansion |
| `models` | yes | List of model IDs available for this provider |

---

## Model selection

Cooper selects a model in this priority order:

1. `--model <ID>` CLI flag
2. `default_model` in project config
3. `default_model` in global config
4. First model listed under the selected provider
5. Error if none of the above resolves

---

## CLI management

```bash
# List all configured providers
cooper providers list

# Add a provider interactively
cooper providers add

# Add a provider with flags
cooper providers add \
  --name my-server \
  --base-url "http://localhost:8080/v1" \
  --api openai-completions \
  --model my-model \
  --project

# Remove a provider
cooper providers remove my-server

# Update a field
cooper providers set ollama --base-url "http://new-host:11434/v1"

# Manage models
cooper providers models list ollama
cooper providers models add ollama phi3:mini
cooper providers models remove ollama phi3:mini
```

---

## Specifying a provider at runtime

```bash
cooper chat --provider anthropic --model claude-opus-4-7
cooper prompt --provider ollama --model llama3.1 "Explain this code"
```

---

## Notes on specific providers

**Ollama** — The API key field is required by Cooper's config schema but Ollama ignores it. Pass any non-empty string (e.g. `"ollama"`).

**Models with extended thinking** — The `openai-completions` provider parses `<think>` blocks emitted by models such as DeepSeek-R1. Thinking content is displayed separately from the final response.

**Inline tool calls** — Some open-weight models (Qwen, Hermes) emit tool calls as JSON embedded in the response text rather than via the structured API field. Cooper detects and parses these automatically when using the `openai-completions` adapter.
