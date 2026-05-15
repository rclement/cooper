# Tools

Tools give the agent the ability to take actions: read and write files, run commands, and call custom scripts. The agent decides when to call tools based on the conversation; you control which tools are available.

---

## Built-in tools

These tools ship with Cooper and are always available (unless restricted by `allowed_tools`).

| Tool | Description | Parameters |
|------|-------------|------------|
| `list_files` | List directory contents | `path` (optional, default `.`) |
| `read_file` | Read a file's contents | `path` (required) |
| `write_file` | Write content to a file | `path` (required), `content` (required) |
| `edit_file` | Replace the first occurrence of a string in a file | `path` (required), `old` (required), `new` (required) |
| `execute_command` | Run a shell command | `command` (required) |

---

## Custom tools

You can add custom tools as YAML files in a discovery directory. No compilation required — tools are shell commands with typed parameters.

### Discovery directories

Cooper looks in these directories, in order:

1. `.agents/tools/` — project-level tools (current working directory)
2. `~/.cooper/tools/` — global tools

Project tools take precedence over global tools with the same name. Custom tool names cannot shadow built-in tool names.

### File structure

Tools can be defined as a single file or as a bundle:

```
.agents/tools/
├── search-files.yml          # single-file tool
└── my-bundled-tool/
    ├── tool.yml              # definition (filename must be tool.yml)
    └── helper.sh             # supporting scripts accessible in the command
```

### Tool definition format

```yaml
name: fetch-url
description: Fetch a URL and return its content

parameters:
  url:
    type: string          # string | number | boolean
    required: true
    description: The URL to fetch
  user_agent:
    type: string
    required: false
    default: "agent-cooper"
    description: User-Agent header value

command: ["curl", "-sL", "-H", "User-Agent: ${user_agent}", "${url}"]
```

**Parameter fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `type` | yes | `string`, `number`, or `boolean` |
| `required` | yes | Whether the model must supply this parameter |
| `default` | no | Value used when the parameter is omitted |
| `description` | no | Shown to the model to help it use the tool correctly |

**Parameter substitution:** Use `${param_name}` anywhere in the command array to inject the value at runtime.

---

### Pipeline tools

For multi-step transforms, define `command` as a list of commands. Stdout from each step is piped into stdin of the next:

```yaml
name: fetch-md
description: Fetch a URL and convert the HTML to Markdown

parameters:
  url:
    type: string
    required: true

command:
  - ["curl", "-sL", "${url}"]
  - ["html2text", "--stdin"]
```

---

### Environment variables

Inject environment variables that are only visible at runtime (not exposed to the model):

```yaml
name: fetch-authenticated
description: Fetch a URL with API key authentication

parameters:
  url:
    type: string
    required: true

env:
  API_KEY: "${MY_SECRET_KEY}"   # resolved from OS environment
  TIMEOUT: "30"

command: ["curl", "-sL", "-H", "Authorization: Bearer ${API_KEY}", "${url}"]
```

The `env` block values support `${VAR}` expansion from the OS environment. The resolved values are set as environment variables for the subprocess but are never sent to the model.

---

## Running tools manually

You can call any tool directly without the model, useful for testing:

```bash
cooper tools run list_files
cooper tools run read_file --path src/main.rs
cooper tools run search-files --pattern "fn main" --glob "*.rs" --path src/
```

## Listing available tools

```bash
cooper tools list
```

This shows all tools (built-in + custom) that would be available in a session given the current configuration.

---

## Controlling tool access

See [Context — tool allowlist](context.md#tool-allowlist) for how to restrict which tools are available per session or project.

---

## Example: project search tool

A real-world example from `.agents/tools/search-files.yml`:

```yaml
name: search-files
description: Search for a pattern in files using grep

parameters:
  pattern:
    type: string
    required: true
    description: The text or regex pattern to search for
  path:
    type: string
    required: false
    default: "."
    description: Directory or file to search in
  glob:
    type: string
    required: false
    default: "*"
    description: Filename glob pattern to restrict search (e.g. "*.rs")

command: ["grep", "-rn", "--include=${glob}", "${pattern}", "${path}"]
```
