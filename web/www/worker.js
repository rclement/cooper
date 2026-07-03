// Runs the wasm agent core off the main thread. Receives { prompt, config }
// messages and posts back { type: 'event' | 'done' | 'error', ... } messages
// as the agentic loop streams.
import init, { WasmAgent } from "../pkg/cooper_web.js";

const ready = init();

self.onmessage = async (message) => {
  const { prompt, config } = message.data;

  try {
    await ready;
    const agent = new WasmAgent(JSON.stringify(config));

    const resultJson = await agent.run_prompt(prompt, (eventJson) => {
      self.postMessage({ type: "event", event: JSON.parse(eventJson) });
    });

    self.postMessage({ type: "done", result: JSON.parse(resultJson) });
  } catch (err) {
    self.postMessage({ type: "error", error: String(err) });
  }
};
