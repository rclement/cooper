// Main-thread rendering for show_image / show_svg — the drawing half of
// media-tools.js's validate-only execute(). Reads straight from the same
// OPFS the Workspace view browses (see workspace-fs.js's header comment on
// why that needs no message-passing bridge to the Worker).
import { readFileBlob } from "./workspace-fs.js";
import { sanitizeSvg } from "./svg-sanitize.js";

function appendCaption(container, caption) {
  if (!caption) return;
  const el = document.createElement("div");
  el.className = "media-caption";
  el.textContent = caption;
  container.appendChild(el);
}

export async function renderImage(container, path, caption) {
  appendCaption(container, caption);
  const blob = await readFileBlob(path);
  const url = URL.createObjectURL(blob);
  const img = document.createElement("img");
  img.className = "media-image";
  img.src = url;
  img.alt = path;
  // Safe to revoke once the browser has decoded the bytes for this <img> —
  // the object URL doesn't need to stay alive after that for a plain <img>.
  img.addEventListener("load", () => URL.revokeObjectURL(url), { once: true });
  container.appendChild(img);
}

export function renderSvg(container, svgText, caption) {
  appendCaption(container, caption);
  const clean = sanitizeSvg(svgText); // throws on invalid/empty markup
  clean.classList.add("media-svg");
  container.appendChild(document.importNode(clean, true));
}
