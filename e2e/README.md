# cooper-e2e

Browser end-to-end tests for the web app: real headless Chromium (via
[`chromiumoxide`](https://docs.rs/chromiumoxide), talking directly to the
Chrome DevTools Protocol — no Node.js/Playwright anywhere in this crate's
dependency graph), the real wasm build, and the real `cooper-mock-server`
(run in-process, not as a subprocess) scripting deterministic responses. Not
a comprehensive suite — just the main features, as regression coverage.

## Prerequisites

```sh
wasm-pack build --target web --out-dir www/pkg  # from web/, produces web/www/pkg/
```

`cooper-mock-server` is run in-process via its library API, so no separate
build step is needed for it — `cargo test` builds it as a normal workspace
dependency. A Chrome or Chromium binary must be installed and discoverable
(chromiumoxide auto-detects common install locations); set
`CHROMIUM_EXECUTABLE` to point at a specific binary if auto-detection
doesn't find yours.

## Running

```sh
cargo test -p cooper-e2e -j 1
```

**`-j 1` is required**: each test launches its own Chrome instance, and
launching several concurrently is flaky (WebSocket handshake races). This
runs the 4 test binaries one after another instead of in parallel — normal
`--test-threads` doesn't help here since that only controls parallelism
*within* one binary, and each test file here has exactly one test.

Excluded from `cargo cov`/CI's default test job (same as `cooper-web`) since
it needs the wasm build and a real browser, not something the fast unit-test
loop should depend on.
