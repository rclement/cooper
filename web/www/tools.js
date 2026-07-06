// Combined registry of every tool the agent can be given: network built-ins
// plus the Workspace (virtual filesystem) tools. context.js (the enable/
// disable UI) and worker.js (registration into the wasm agent) both read
// from this single map so adding a new tool module only means adding it here.
import { BUILTIN_TOOLS } from "./builtin-tools.js";
import { WORKSPACE_TOOLS } from "./workspace-tools.js";
import { PYTHON_TOOLS } from "./python-tool.js";
import { DUCKDB_TOOLS } from "./duckdb-tool.js";
import { CHART_TOOLS } from "./chart-tool.js";
import { MEDIA_TOOLS } from "./media-tools.js";

export const ALL_TOOLS = {
  ...BUILTIN_TOOLS,
  ...WORKSPACE_TOOLS,
  ...PYTHON_TOOLS,
  ...DUCKDB_TOOLS,
  ...CHART_TOOLS,
  ...MEDIA_TOOLS,
};
