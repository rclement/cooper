// Runs the wasm agent core off the main thread. Receives
// { prompt, config, enabledTools } messages and posts back
// { type: 'event' | 'done' | 'error', ... } messages as the agentic loop
// streams.
import init, { WasmAgent } from "../pkg/cooper_web.js";
import { BUILTIN_TOOLS } from "./builtin-tools.js";

const ready = init();

self.onmessage = async (message) => {
  const { prompt, config, enabledTools } = message.data;

  try {
    await ready;
    const agent = new WasmAgent(JSON.stringify(config));

    for (const name of enabledTools ?? []) {
      const tool = BUILTIN_TOOLS[name];
      if (!tool) continue;
      agent.register_tool(JSON.stringify(tool.schema), tool.execute);
    }

    const resultJson = await agent.run_prompt(prompt, (eventJson) => {
      self.postMessage({ type: "event", event: JSON.parse(eventJson) });
    });

    self.postMessage({ type: "done", result: JSON.parse(resultJson) });
  } catch (err) {
    self.postMessage({ type: "error", error: String(err) });
  }
};
