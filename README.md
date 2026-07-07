# Cooper

> *...damn good agent*

A minimal Rust AI agent harness with tool-calling support: a native CLI, and a
client-side web app built from the same core.

## Build & Run

```bash
cargo build --release          # produces target/release/cooper
cargo run -- prompt "your message here"
```

## Usage

```bash
cooper prompt "<text>" [-p provider] [-m model] [-i agent_instructions.md] [-c context_file ...]
cooper chat [-r session_id]    # interactive multi-turn conversation
cooper sessions list|show <id> # saved chat sessions
cooper web [-P port] [-d dir]  # serve the browser app (see below)
```

- `-p, --provider` — provider name (defaults to config)
- `-m, --model` — model name (defaults to config)
- `-i, --agent-instructions` — file with extra agent instructions
- `-c, --context-file` — one or more files added as context

## Web app

`cooper web` serves the browser app from `web/` with the headers it needs:
COOP/COEP for cross-origin isolation (multi-threaded wllama inference via
`SharedArrayBuffer`), `Cache-Control: no-store` so reloads pick up fresh wasm
builds, and a same-origin git CORS proxy at `/git-proxy` so workspace cloning
doesn't depend on cors.isomorphic-git.org.

The wasm package (`web/pkg/`) is built automatically by [build.rs](build.rs)
whenever `web/` or `core/` sources change, as part of `cargo build`/`cargo run` on the CLI — install [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)
and it's picked up with no extra step:

```bash
cargo run -- web   # http://127.0.0.1:8080/
```

If `wasm-pack` isn't installed, the build step is skipped with a warning
(the native CLI doesn't need it) and `cooper web` will tell you to run
`wasm-pack build --target web web/` manually. Set `COOPER_SKIP_WASM_BUILD=1`
to skip it even when `wasm-pack` is present (e.g. in CI).

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

- [src/](src/) — the `cooper` CLI: args, config, sessions, native tools, `cooper web` server
- [build.rs](build.rs) — builds `web/pkg` via `wasm-pack` ahead of `cooper build`/`run`
- [core/](core/) — target-agnostic agent core (loop, providers, tool traits), shared by CLI and web
- [web/](web/) — client-side web app: wasm bindings ([web/src/](web/src/)) and browser UI ([web/www/](web/www/))
- [mock-server/](mock-server/) — canned OpenAI-compatible SSE server for deterministic testing
- [e2e/](e2e/) — browser end-to-end tests for the web app (headless Chromium)
