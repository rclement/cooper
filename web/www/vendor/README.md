# Vendored dependencies

Static ESM builds, one directory per package, copied in as-is (no npm/bundler
at runtime — this app has no build step). Each package's files stay
self-contained under `vendor/<pkg>/` so a version bump or a local patch never
spills across packages.

| Directory | Package | Version | License | Used for |
| --- | --- | --- | --- | --- |
| `marked/` | [marked](https://www.npmjs.com/package/marked) | 18.0.5 | MIT (`marked/LICENSE`) | Markdown (GFM) → HTML |
| `dompurify/` | [dompurify](https://www.npmjs.com/package/dompurify) | 3.4.11 | Apache-2.0 (`dompurify/LICENSE`) | Sanitizing that HTML before it's injected into the page |
| `wllama/` | [@wllama/wllama](https://www.npmjs.com/package/@wllama/wllama) | see `wllama/index.js` | MIT (`wllama/LICENCE`) | In-browser local model inference |
| `pyodide/` | [pyodide](https://www.npmjs.com/package/pyodide) | 0.29.4 | MPL-2.0 (`pyodide/LICENSE`) | `run_python` tool — in-browser Python interpreter |
| `duckdb-wasm/` | [@duckdb/duckdb-wasm](https://www.npmjs.com/package/@duckdb/duckdb-wasm) | 1.29.0 | MIT (`duckdb-wasm/LICENSE`) | `run_sql` tool — in-browser analytical database |

To re-vendor or bump a version: `npm pack <pkg>` into a scratch dir, extract,
copy the package's dependency-free ESM build (and its `LICENSE`) over the
matching `vendor/<pkg>/` directory, update the version/table above. If a
package ever needs a local patch, apply it directly inside its
`vendor/<pkg>/` directory and note the patch at the top of the file — it
won't affect any other vendored package.

## pyodide

Only the core interpreter is vendored (`pyodide.mjs`/`.asm.js`/`.asm.wasm`,
`python_stdlib.zip`, `pyodide-lock.json` — ~12 MB, copied as-is from the npm
package). Individual packages (numpy, pandas, etc.) aren't part of the npm
distribution at all — pyodide fetches those lazily, by design, only when
something actually imports them. `python-tool.js` points that lazy fetch at
jsDelivr's package repository via `packageBaseUrl`, kept separate from the
local `indexURL` used for the interpreter itself.

## duckdb-wasm

The npm build's browser ESM entrypoint imports `apache-arrow` as a bare
specifier rather than inlining it, so a plain copy isn't dependency-free
like the other vendored packages. `duckdb-browser.esm.js` here is instead
pre-bundled with esbuild against `apache-arrow` to fold it in:

```sh
npm pack @duckdb/duckdb-wasm@<version>  # extract, then in a scratch dir:
npm install apache-arrow@<matching semver from its package.json>
npx esbuild duckdb-browser.mjs --bundle --format=esm --outfile=duckdb-browser.esm.js
```

Only the `eh` (exception handling) and `coi` (cross-origin-isolated,
multi-threaded) wasm variants are vendored — not `mvp`. `cooper web` always
serves with COOP/COEP (see `src/web.rs`), so this app is always
cross-origin isolated and `coi`, a strict superset of `eh`, is always
preferred when supported. `eh` covers the realistic fallback cases: e2e
tests and any serving path other than `cooper web` (both leave
`crossOriginIsolated` false), plus Safari 15.2-16.3 (exception handling but
no SIMD). `mvp` would only additionally cover browsers with no WASM
exception-handling support at all (pre-2021 Chrome/Firefox/Safari) —
already unsupported by this app's SharedArrayBuffer-dependent local-model
story, so not worth vendoring for ~4 MB of extra coverage. (DuckDB-wasm's
compiled `selectBundle` does unconditionally read `bundles.mvp.mainModule`
in that last-resort branch regardless of which keys are supplied, so on one
of those ancient browsers this throws a "Cannot read properties of
undefined" instead of a clean error — still just a rejected promise the
tool-call pipeline catches normally, just a worse message for an
already-unsupported browser.) Re-vendor both `duckdb-eh.wasm` and
`duckdb-coi.wasm` plus their matching `duckdb-browser-*.worker.js` /
`duckdb-browser-coi.pthread.worker.js` files from `dist/`.

Assistant responses are untrusted model output. `marked` only parses
markdown into HTML; it does not defend against a model emitting raw
`<script>`/`onerror=`/`javascript:`-URL payloads. `DOMPurify` sanitizes the
parsed HTML before it's assigned via `innerHTML`, so vendoring `marked`
alone would not be safe — see `../markdown.js`.
