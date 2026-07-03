# Vendored dependencies

Static ESM builds, one directory per package, copied in as-is (no npm/bundler
at runtime — this app has no build step). Each package's files stay
self-contained under `vendor/<pkg>/` so a version bump or a local patch never
spills across packages.

| Directory | Package | Version | License | Used for |
| --- | --- | --- | --- | --- |
| `marked/` | [marked](https://www.npmjs.com/package/marked) | 18.0.5 | MIT (`marked/LICENSE`) | Markdown (GFM) → HTML |
| `dompurify/` | [dompurify](https://www.npmjs.com/package/dompurify) | 3.4.11 | Apache-2.0 (`dompurify/LICENSE`) | Sanitizing that HTML before it's injected into the page |

To re-vendor or bump a version: `npm pack <pkg>` into a scratch dir, extract,
copy the package's dependency-free ESM build (and its `LICENSE`) over the
matching `vendor/<pkg>/` directory, update the version/table above. If a
package ever needs a local patch, apply it directly inside its
`vendor/<pkg>/` directory and note the patch at the top of the file — it
won't affect any other vendored package.

Assistant responses are untrusted model output. `marked` only parses
markdown into HTML; it does not defend against a model emitting raw
`<script>`/`onerror=`/`javascript:`-URL payloads. `DOMPurify` sanitizes the
parsed HTML before it's assigned via `innerHTML`, so vendoring `marked`
alone would not be safe — see `../markdown.js`.
