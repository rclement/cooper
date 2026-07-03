// Runs the wasm agent core off the main thread. Receives
// { prompt, config, enabledTools, newSession, restoreHistory } messages and
// posts back { type: 'event' | 'done' | 'error', ... } messages as the
// agentic loop streams.
//
// `agent` is kept alive across messages (instead of created fresh each
// time) so a run with `newSession: false` continues the same conversation
// — the wasm-side WasmAgent carries its own message history internally.
// `restoreHistory` (a JSON string previously produced by
// `agent.export_history()`) seeds that history when resuming a session
// persisted from a *previous* page load, where no in-memory agent exists.
import init, { WasmAgent } from "../pkg/cooper_web.js";
import { BUILTIN_TOOLS } from "./builtin-tools.js";

const ready = init();
let agent = null;

self.onmessage = async (message) => {
  const { prompt, config, enabledTools, newSession, restoreHistory } = message.data;

  try {
    await ready;

    if (newSession || !agent) {
      // Cleared up front (not just reassigned below) so that if
      // construction throws, the catch block below doesn't attribute the
      // failure to — and export history from — the *previous* session's
      // agent instead of this one.
      agent = null;
      agent = new WasmAgent(JSON.stringify(config));
      for (const name of enabledTools ?? []) {
        const tool = BUILTIN_TOOLS[name];
        if (!tool) continue;
        agent.register_tool(JSON.stringify(tool.schema), tool.execute);
      }
      if (restoreHistory) {
        agent.import_history(restoreHistory);
      }
    }

    const resultJson = await agent.run_prompt(prompt, (eventJson) => {
      self.postMessage({ type: "event", event: JSON.parse(eventJson) });
    });

    self.postMessage({
      type: "done",
      result: JSON.parse(resultJson),
      history: agent.export_history(),
    });
  } catch (err) {
    self.postMessage({
      type: "error",
      error: String(err),
      history: agent?.export_history(),
    });
  }
};
