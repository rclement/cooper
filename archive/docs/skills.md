# Skills

A skill is a named, reusable system prompt fragment. Activating a skill replaces the agent's behavior for the remainder of the session — turning a general-purpose assistant into a focused expert.

Cooper's skill format follows the [Agent Skills Specification](https://agentskills.io), which is the reference for the file layout, frontmatter fields, and discovery conventions described on this page.

---

## Discovery directories

Cooper looks for skills in:

1. `.agents/skills/` — project-level skills (current working directory)
2. `~/.cooper/skills/` — global skills

Project skills take precedence over global skills with the same name.

---

## Skill format

A skill is a Markdown file with an optional YAML frontmatter block.

```markdown
---
name: rust-reviewer
description: Reviews Rust code for idioms, safety, and simplification opportunities
---

You are a Rust code reviewer. When given a file or snippet to review, you:

1. Read the relevant file(s) using your tools before commenting.
2. Focus only on issues that matter: non-idiomatic patterns, unnecessary complexity,
   error handling gaps, and correctness concerns.
3. Ignore style trivia unless they obscure meaning.
4. Structure your feedback as a short list. Each item: one sentence naming the problem,
   one sentence proposing the fix. No padding.
5. If the code is solid, say so in one sentence and stop.
```

**Frontmatter fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `name` | no | Skill identifier. Falls back to the filename stem if omitted |
| `description` | no | One-line summary shown in `cooper skills list` and in the skill catalog notice sent to the model |

Everything after the `---` closing delimiter is the skill body — this becomes the injected system prompt content.

---

## File structure

Skills can be single files or bundles:

```
.agents/skills/
├── rust-reviewer.md          # single-file skill
└── deep-audit/
    ├── skill.md              # definition (filename must be skill.md)
    └── checklist.md          # resource file, accessible to the agent
```

In a bundled skill, Cooper lists the available resource files when the skill is activated, so the agent can read them with `read_file` if needed.

---

## Activating a skill

### At session start (CLI flag)

```bash
cooper prompt --skill rust-reviewer "Review src/agent.rs"
cooper chat --skill rust-reviewer
```

The skill body is injected before the session begins.

### During a session (dynamic activation)

In chat mode, ask the agent to activate a skill:

> "Activate the rust-reviewer skill"

Internally, the agent calls the `activate_skill` tool. Cooper replaces (or appends) a `<skill-instructions>` block in the system prompt. The change is immediate and persists for all subsequent turns in the session.

The model is made aware of available skills via a catalog notice in the system prompt that lists their names and descriptions.

---

## Controlling which skills are available

By default all discovered skills are available. You can restrict this per project:

```yaml
# cooper.yml
context:
  allowed_skills:
    - rust-reviewer
    - security-audit
```

Or via CLI:

```bash
cooper context skills allow rust-reviewer
cooper context skills deny experimental-skill
cooper context skills list
cooper context skills reset    # restore "all skills available"
```

Use `--global` to apply changes to `~/.cooper/settings.yml` instead.

---

## Listing available skills

```bash
cooper skills list
```

---

## Tips

- Keep skill bodies focused and imperative. The model follows instructions literally — avoid vague goals.
- A skill works best when it describes a specific *role* or *task mode*, not general advice.
- Use `description` in the frontmatter — it's what the model reads in the skill catalog to decide whether to activate the skill automatically.
- For complex skills that need reference material, use a bundled skill directory and put supporting files alongside `skill.md`.
