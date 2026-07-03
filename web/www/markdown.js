// Renders assistant responses (untrusted model output) as GFM markdown.
// `marked` only parses markdown -> HTML; it does not defend against a model
// emitting raw `<script>`/`onerror=`/`javascript:`-URL payloads, so the
// result is always run through DOMPurify before it's safe to assign via
// innerHTML. See vendor/README.md for where these come from.
import { marked } from "./vendor/marked/marked.esm.js";
import DOMPurify from "./vendor/dompurify/purify.es.mjs";

export function renderMarkdown(text) {
  const html = marked.parse(text);
  return DOMPurify.sanitize(html);
}
