// Shared between the render_chart tool (chart-tool.js, executes inside the
// agent Worker and only needs to validate) and the main-thread chart
// renderer (chart-render.js, which needs the same parsed {rows, spec} to
// actually draw). Keeping parsing/validation in one place means the error
// message the agent sees on a bad call is the same reason nothing rendered.

export const CHART_TYPES = ["bar", "line", "scatter"];
export const MAX_ROWS = 500;
export const MAX_SERIES = 8;

// Fixed-order categorical hues, validated (Machado-2009 CVD simulation,
// OKLCH lightness/chroma bands, contrast) against this app's dark chart
// surface (`--surface: #131417`) — see `scripts/validate_palette.js` in the
// dataviz skill. Never reorder per-chart and never generate a 9th color past
// this; fold extra series into "Other" or facet instead (see MAX_SERIES).
export const CATEGORICAL_COLORS = [
  "#3987e5", // 1 blue
  "#199e70", // 2 aqua
  "#c98500", // 3 yellow
  "#008300", // 4 green
  "#9085e9", // 5 violet
  "#e66767", // 6 red
  "#d55181", // 7 magenta
  "#d95926", // 8 orange
];

/// Parses and validates a render_chart tool call's raw JSON-string
/// arguments. Throws a descriptive `Error` (surfaced to the agent as the
/// tool result, and to the chart block as its inline error state) on any
/// malformed input.
export function parseChartCall(argsJson) {
  const { data, spec: specJson } = JSON.parse(argsJson);

  let rows;
  try {
    rows = JSON.parse(data);
  } catch {
    throw new Error("`data` must be a JSON-encoded array of row objects.");
  }
  if (!Array.isArray(rows) || rows.length === 0) {
    throw new Error("`data` must be a non-empty array of row objects.");
  }
  if (rows.length > MAX_ROWS) {
    throw new Error(
      `Too many rows (${rows.length}); aggregate or sample down to ${MAX_ROWS} or fewer before charting.`,
    );
  }

  let spec;
  try {
    spec = JSON.parse(specJson);
  } catch {
    throw new Error("`spec` must be a JSON-encoded object.");
  }
  if (!CHART_TYPES.includes(spec.type)) {
    throw new Error(`spec.type must be one of: ${CHART_TYPES.join(", ")}.`);
  }
  if (!spec.x || typeof spec.x !== "string") {
    throw new Error("spec.x must be the name of the field to use for the x axis.");
  }
  const yFields = Array.isArray(spec.y) ? spec.y : [spec.y];
  if (yFields.length === 0 || yFields.some((f) => typeof f !== "string" || !f)) {
    throw new Error("spec.y must be a field name or a non-empty array of field names.");
  }
  if (yFields.length > MAX_SERIES) {
    throw new Error(`spec.y supports at most ${MAX_SERIES} series.`);
  }
  for (const field of [spec.x, ...yFields]) {
    if (!(field in rows[0])) {
      throw new Error(`Field "${field}" is not present in the row data.`);
    }
  }

  return {
    rows,
    spec: {
      type: spec.type,
      x: spec.x,
      y: yFields,
      title: typeof spec.title === "string" ? spec.title : null,
      xLabel: typeof spec.x_label === "string" ? spec.x_label : spec.x,
      yLabel: typeof spec.y_label === "string" ? spec.y_label : yFields.join(" / "),
    },
  };
}
