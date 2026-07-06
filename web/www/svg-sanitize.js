// Minimal allowlist-based SVG sanitizer. SVG is executable content — a
// <script>, an event-handler attribute, a SMIL <animate> rewriting an
// attribute, or a <foreignObject> full of HTML are all real script-injection
// vectors, and the source here is an LLM's own output: not actively
// hostile in the normal case, but a prompt-injected page fetched earlier in
// the session, or just an off-the-rails generation, can end up producing
// exactly this kind of markup. This is intentionally scoped to what a
// diagram/icon plausibly needs — not a general-purpose sanitizer like
// DOMPurify — and errs toward stripping the unfamiliar.
const DISALLOWED_TAGS = new Set([
  "script",
  "foreignobject",
  "iframe",
  "embed",
  "object",
  "link",
  "meta",
  "animate",
  "animatemotion",
  "animatetransform",
  "set",
  "style", // CSS `url(...)`/`@import` side channel; use presentation attrs or the `style` attribute instead
]);

function stripDangerous(el) {
  // Snapshot children before removing any — mutating while iterating a live
  // HTMLCollection skips siblings.
  for (const child of [...el.children]) {
    if (DISALLOWED_TAGS.has(child.tagName.toLowerCase())) {
      child.remove();
      continue;
    }
    stripDangerous(child);
  }

  for (const attr of [...el.attributes]) {
    const name = attr.name.toLowerCase();
    if (name.startsWith("on")) {
      el.removeAttribute(attr.name);
      continue;
    }
    if (name === "href" || name === "xlink:href") {
      const value = attr.value.trim();
      const isImage = el.tagName.toLowerCase() === "image";
      // <image> may only embed inline data — never fetch a remote or
      // javascript: URL. Everything else (<use>, <a>, ...) may only
      // reference a same-document fragment.
      const safe = isImage ? /^data:image\//i.test(value) : value.startsWith("#");
      if (!safe) el.removeAttribute(attr.name);
    }
  }
}

/// Parses `svgText`, strips script-execution vectors in place, and returns
/// the cleaned root `<svg>` element (still owned by a detached document —
/// the caller must `document.importNode(el, true)` before inserting it into
/// the page). Throws if the markup doesn't parse or isn't a single
/// `<svg>...</svg>` document.
export function sanitizeSvg(svgText) {
  const doc = new DOMParser().parseFromString(svgText, "image/svg+xml");
  if (doc.querySelector("parsererror")) {
    throw new Error("Invalid SVG markup.");
  }
  const root = doc.documentElement;
  if (!root || root.tagName.toLowerCase() !== "svg") {
    throw new Error("SVG markup must be a single <svg>...</svg> document.");
  }
  stripDangerous(root);
  return root;
}
