# Context & Agent Instructions

This page explains exactly how Cooper assembles the system prompt before each session, and how to control what goes into it.

---

## How the system prompt is built

Cooper assembles the final system prompt in this order every time a session starts:

```
1. Base system prompt        (from config or --system-prompt flag)
2. Skill catalog notice      (if any skills are available)
3. Agent instructions        (from AGENTS.md or configured file)
4. Context files             (from context.files in config)
```

Each layer is appended to the previous one. The assembled prompt is sent to the model as the `system` message and remains fixed for the lifetime of the session — except for skill injections (see below).

### 1. Base system prompt

The literal `system_prompt` string from configuration, or the value passed with `--system-prompt`. Default: `"You are a helpful AI assistant"`.

### 2. Skill catalog notice

If any skills are available (after allowlist filtering), Cooper appends a brief notice listing them by name and description, and informs the model that it can call the `activate_skill` tool to load one. This is how skills are made discoverable without pre-loading all of them.

### 3. Agent instructions

Cooper follows the [agents.md](https://agents.md/) specification for agent instructions files.

If `AGENTS.md` exists in the current directory (and loading is not disabled), its content is injected wrapped in XML-style tags:

```
<agent-instructions>
...content of AGENTS.md...
</agent-instructions>
```

Use this file for project-specific instructions: coding standards, conventions, what the project does, how to run tests, etc.

### 4. Context files

Each file listed in `context.files` is read and appended under a `<context>` block:

```
<context>
<file path="README.md">
...file contents...
</file>
<file path="docs/architecture.md">
...file contents...
</file>
</context>
```

Files are loaded once at session start. Changes to files during a session are not picked up until the next session.

---

## Skill injection mid-session

When the model calls `activate_skill <name>`, Cooper replaces (or appends) a `<skill-instructions>` block in the system prompt with the skill's body. This happens in-place, so the skill instructions are active for all subsequent turns in the same session.

This means you can start a generic session and then ask the agent to "activate the rust-reviewer skill" and it will apply from that point forward, without restarting.

---

## Controlling what is loaded

### Agent instructions

```bash
# Disable AGENTS.md for this invocation
cooper chat --no-agent-instructions

# Use a different file
cooper chat --agent-instructions CLAUDE.md

# Permanently disable in project config
cooper context agent-instructions disable
```

In `cooper.yml`:
```yaml
context:
  agent_instructions: false          # disable
  agent_instructions: "CLAUDE.md"   # custom file
```

### Context files

Files to inject are declared in config:

```yaml
context:
  files:
    - README.md
    - docs/architecture.md
    - CONTRIBUTING.md
```

Or managed via CLI:

```bash
cooper context files add README.md
cooper context files list
cooper context files remove CONTRIBUTING.md
cooper context files clear
```

### Tool allowlist

By default all tools (built-in + custom) are available. To restrict:

```yaml
context:
  allowed_tools:
    - list_files
    - read_file
    - search-files
    # execute_command is NOT listed → agent cannot run shell commands
```

`null` (or omitting the key) means all tools are allowed. An empty list `[]` means no tools at all.

```bash
cooper context tools allow read_file
cooper context tools deny execute_command
cooper context tools reset    # back to "allow all"
```

### Skill allowlist

Same pattern for skills:

```yaml
context:
  allowed_skills:
    - rust-reviewer
```

```bash
cooper context skills allow rust-reviewer
cooper context skills deny experimental-skill
cooper context skills reset
```

---

## Viewing the resolved context

To see exactly what system prompt and tools would be used for a session:

```bash
cooper context show
```

---

## Tips

- Keep `AGENTS.md` focused on *what the project is* and *how to work in it*. See the [agents.md spec](https://agents.md/) for recommended conventions. Avoid duplicating general instructions that belong in the system prompt.
- Use `context.files` for files the agent needs to read often (architecture docs, conventions, API contracts) so they are always in context without the agent having to call `read_file` first.
- Restrict `allowed_tools` in `cooper.yml` when you want to prevent the agent from modifying files or running commands — useful for read-only review tasks.
- Restrict `allowed_skills` when you have many skills globally but only want a subset relevant to the current project.
