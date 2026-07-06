// Combined registry of every tool the agent can be given: network built-ins
// plus the Workspace (virtual filesystem) tools. context.js (the enable/
// disable UI) and worker.js (registration into the wasm agent) both read
// from this single map so adding a new tool module only means adding it here.
import { BUILTIN_TOOLS } from "./builtin-tools.js";
import { WORKSPACE_TOOLS } from "./workspace-tools.js";

export const ALL_TOOLS = { ...BUILTIN_TOOLS, ...WORKSPACE_TOOLS };
