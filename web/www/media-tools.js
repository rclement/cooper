// Agent tools that display media inline in the conversation — the same
// tool_call-renders-immediately pattern as render_chart (see chart-tool.js):
// execute() here only validates, since the actual drawing happens on the
// main thread from the tool_call's own arguments (see media-render.js and
// appendImageBlock/appendSvgBlock in app.js).
import { readFileBlob, isImageName } from "./workspace-fs.js";

// A cheap structural check only — NOT the real sanitizer. `execute()` runs
// inside the agent Worker, which has no `DOMParser` (it's a Window-only API
// per spec: https://w3c.github.io/DOM-Parsing/#dom-domparser), so the actual
// parse-and-strip in svg-sanitize.js can only run on the main thread, in
// media-render.js. This just catches "that's not SVG at all" early enough to
// give the agent a useful error instead of silently rendering nothing.
function looksLikeSvg(text) {
  return /^\s*<svg[\s>]/i.test(text) && /<\/svg>\s*$/i.test(text.trim());
}

export const MEDIA_TOOLS = {
  show_image: {
    schema: {
      name: "show_image",
      description:
        "Display an image file from the workspace inline in the conversation — e.g. a chart saved via matplotlib, or a photo you were asked to inspect. Supports png, jpg, jpeg, gif, webp, bmp, svg.",
      parameters: {
        path: {
          type: "string",
          description: "Image file path relative to the workspace root.",
          required: true,
        },
        caption: {
          type: "string",
          description: "Optional caption shown above the image.",
          required: false,
        },
      },
    },
    async execute(argsJson) {
      const { path } = JSON.parse(argsJson);
      if (!isImageName(path)) {
        throw new Error(
          `"${path}" doesn't look like a supported image file (png, jpg, jpeg, gif, webp, bmp, svg).`,
        );
      }
      const blob = await readFileBlob(path); // throws a descriptive error if missing
      return `Displayed "${path}" (${blob.size.toLocaleString()} bytes).`;
    },
  },

  show_svg: {
    schema: {
      name: "show_svg",
      description:
        "Render raw SVG markup inline in the conversation — for a diagram, icon, or simple illustration you compose directly, not tied to a file. Scripts, event handlers, and remote references are stripped before display.",
      parameters: {
        svg: {
          type: "string",
          description: "A full <svg>...</svg> document.",
          required: true,
        },
        caption: {
          type: "string",
          description: "Optional caption shown above the SVG.",
          required: false,
        },
      },
    },
    async execute(argsJson) {
      const { svg } = JSON.parse(argsJson);
      if (!looksLikeSvg(svg)) {
        throw new Error("`svg` must be a single <svg>...</svg> document.");
      }
      return "Rendered SVG.";
    },
  },
};
