// Agent tool that renders a chart inline in the conversation timeline. Unlike
// the other tools, its `execute` does no real work — it only validates the
// call (via chart-common.js, shared with the renderer) and returns a plain
// confirmation string. The actual drawing happens on the main thread, in
// app.js, which renders straight from this same tool call's arguments as
// soon as the `tool_call` event arrives — see appendChartBlock there.
import { parseChartCall } from "./chart-common.js";

export const CHART_TOOLS = {
  render_chart: {
    schema: {
      name: "render_chart",
      description:
        "Render a bar, line, or scatter chart of tabular data directly in the conversation (visible to the user immediately, alongside your response). `data` is a JSON-encoded array of row objects — e.g. from a pandas DataFrame via df.to_json(orient='records'). `spec` is a JSON-encoded object: { type: 'bar'|'line'|'scatter', x: '<field>', y: '<field>' | ['<field>', ...], title?, x_label?, y_label? }. Up to 500 rows and 8 y-series.",
      parameters: {
        data: {
          type: "string",
          description: "JSON-encoded array of row objects, all sharing the same fields.",
          required: true,
        },
        spec: {
          type: "string",
          description:
            "JSON-encoded chart spec: { type, x, y, title?, x_label?, y_label? }.",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { rows, spec } = parseChartCall(argsJson);
      return `Rendered a ${spec.type} chart of ${rows.length} row(s): x=${spec.x}, y=${spec.y.join(", ")}.`;
    },
  },
};
