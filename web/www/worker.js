// Runs the wasm agent core off the main thread. Receives
// { prompt, config, enabledTools, newSession } messages and posts back
// { type: 'event' | 'done' | 'error', ... } messages as the agentic loop
// streams.
//
// `agent` is kept alive across messages (instead of created fresh each
// time) so a run with `newSession: false` continues the same conversation
// — the wasm-side WasmAgent carries its own message history internally.
import init, { WasmAgent } from "../pkg/cooper_web.js";
import { BUILTIN_TOOLS } from "./builtin-tools.js";

const ready = init();
let agent = null;

self.onmessage = async (message) => {
  const { prompt, config, enabledTools, newSession } = message.data;

  try {
    await ready;

    if (newSession || !agent) {
      agent = new WasmAgent(JSON.stringify(config));
      for (const name of enabledTools ?? []) {
        const tool = BUILTIN_TOOLS[name];
        if (!tool) continue;
        agent.register_tool(JSON.stringify(tool.schema), tool.execute);
      }
    }

    const resultJson = await agent.run_prompt(prompt, (eventJson) => {
      self.postMessage({ type: "event", event: JSON.parse(eventJson) });
    });

    self.postMessage({ type: "done", result: JSON.parse(resultJson) });
  } catch (err) {
    self.postMessage({ type: "error", error: String(err) });
  }
};
