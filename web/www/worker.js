// Runs the wasm agent core off the main thread. Receives
// { prompt, config, enabledTools, restoreHistory, gitAuth, workingDir }
// messages and posts back
// { type: 'event' | 'done' | 'error', ... } messages as the agentic loop
// streams.
//
// A fresh WasmAgent is built for every run; continuing a session works by
// seeding it with `restoreHistory` (a JSON string previously produced by
// `agent.export_history()`, persisted by the main thread after each turn).
// `agent` is still module-level only so the error path can export whatever
// history the failed run produced.
//
// When `config.provider_type === "local"`, inference also happens right
// here: wllama (llama.cpp compiled to wasm, including llama-server's
// OpenAI-compatible chat layer) loads the GGUF at `config.model_url` —
// downloading it on first use, cached in OPFS afterwards — and the agent's
// completions are routed to it through `set_completion_bridge` instead of
// going out over HTTP. Model download/load progress is reported with
// { type: 'model_progress', loaded, total } and
// { type: 'model_status', status: 'loading' | 'ready' } messages.
import init, { WasmAgent } from "./pkg/cooper_web.js";
import { ALL_TOOLS } from "./tools.js";
import { setGitAuth } from "./workspace-fs.js";
import { setWorkingDir } from "./workspace-tools.js";
import { Wllama } from "./vendor/wllama/index.js";

// wllama's bundle resolves relative asset paths against `document.baseURI`,
// which doesn't exist inside a worker — give it just enough of a stand-in
// (it reads nothing else off `document`). Runs before any wllama call; the
// import above only defines the helper, it doesn't invoke it.
if (typeof document === "undefined") {
  self.document = { baseURI: self.location.href };
}

const ready = init();
let agent = null;

// Armed at the start of each local run; a { type: "stop" } message aborts
// the in-flight completion. wllama surfaces the abort between streamed
// chunks, so it takes effect immediately while decoding but only at the
// first token when still prefilling (the main thread escalates to
// terminating the whole worker if this isn't enough).
let activeAbort = null;

// One wllama instance for the worker's lifetime; reloading only when the
// requested model changes (loadModelFromUrl may be called repeatedly on the
// same instance). Created lazily so the OpenAI-provider path never pays for
// instantiating the wllama wasm module.
let wllama = null;
let loadedModelUrl = null;

async function ensureLocalModel(modelUrl) {
  if (wllama && loadedModelUrl === modelUrl) return;
  self.postMessage({ type: "model_status", status: "loading" });
  // A Wllama instance can't load a second model ("Module is already
  // initialized"), so switching models — or retrying after a failed load —
  // means tearing the old instance down and starting from a fresh one.
  if (wllama) {
    try {
      await wllama.exit();
    } catch {
      // best-effort: a half-initialized instance may throw on exit
    }
    wllama = null;
    loadedModelUrl = null;
  }
  wllama = new Wllama({
    default: new URL("./vendor/wllama/wasm/wllama.wasm", import.meta.url).href,
  });
  try {
    await wllama.loadModelFromUrl(modelUrl, {
      // wllama's default context is a tiny 1024 tokens — far too small for
      // an agent turn carrying a system prompt, tool schemas, and context
      // files. The KV-cache cost at 16k is modest for the sub-1B models in
      // the catalog.
      n_ctx: 16384,
      progressCallback: ({ loaded, total }) => {
        self.postMessage({ type: "model_progress", loaded, total });
      },
    });
  } catch (err) {
    // Don't keep the broken instance around: the next attempt must start
    // from scratch instead of tripping over "already initialized".
    wllama = null;
    loadedModelUrl = null;
    throw err;
  }
  loadedModelUrl = modelUrl;
  self.postMessage({ type: "model_status", status: "ready" });
}

// The completion bridge contract expected by WasmAgent.set_completion_bridge:
// called with an OpenAI chat.completions request (JSON string), must invoke
// `onChunk` with each streamed chat.completion.chunk as a JSON string, and
// resolve once the stream ends. wllama's createChatCompletion speaks exactly
// this shape on both sides, so this is a thin adapter.
async function localCompletion(requestJson, onChunk) {
  const request = JSON.parse(requestJson);
  const signal = activeAbort?.signal;
  // Fires on every completion round — including the ones after tool results
  // — so the UI can show that the model is prefilling/decoding rather than
  // sitting silent until the first token (which on CPU can take a while).
  self.postMessage({ type: "model_status", status: "generating" });
  try {
    const chunks = await wllama.createChatCompletion({
      messages: request.messages,
      // An empty tools array would still trigger tool-call formatting in some
      // chat templates; omit it entirely when no tools are registered.
      tools: request.tools?.length ? request.tools : undefined,
      stream: true,
      // Reuse the KV cache across rounds: without this, every tool-call
      // round re-prefills the entire conversation from token zero, which
      // dominates latency on CPU. (Forwarded verbatim to the underlying
      // llama-server request handler.)
      cache_prompt: true,
      abortSignal: signal,
    });
    for await (const chunk of chunks) {
      onChunk(JSON.stringify(chunk));
    }
  } catch (err) {
    // A user-requested stop isn't an error: return normally so the agent
    // loop completes the turn with whatever partial output was streamed
    // (and the conversation history stays continuable).
    if (signal?.aborted) return;
    throw err;
  }
}

self.onmessage = async (message) => {
  if (message.data?.type === "stop") {
    activeAbort?.abort();
    return;
  }

  const { prompt, config, enabledTools, restoreHistory, gitAuth, workingDir } = message.data;

  // Refreshed every run, like the config: connecting/disconnecting an
  // account (or the attached repo) between turns must take effect on the
  // next tool call.
  setGitAuth(gitAuth);
  setWorkingDir(workingDir);

  try {
    await ready;

    if (config.provider_type === "local") {
      activeAbort = new AbortController();
      await ensureLocalModel(config.model_url);
    }

    // Rebuilt on every run, not kept across messages: the enabled tools,
    // the provider config, and even which session is active can all change
    // between turns, and a long-lived agent would silently keep the old
    // ones. The conversation itself isn't lost by this — the main thread
    // persists the history after every turn and sends it back as
    // `restoreHistory` whenever the run continues a session.
    //
    // Cleared up front (not just reassigned below) so that if construction
    // throws, the catch block below doesn't attribute the failure to — and
    // export history from — the *previous* run's agent instead of this one.
    agent = null;
    agent = new WasmAgent(JSON.stringify(config));
    if (config.provider_type === "local") {
      agent.set_completion_bridge(localCompletion);
    }
    for (const name of enabledTools ?? []) {
      const tool = ALL_TOOLS[name];
      if (!tool) continue;
      agent.register_tool(JSON.stringify(tool.schema), tool.execute);
    }
    if (restoreHistory) {
      agent.import_history(restoreHistory);
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
  } finally {
    activeAbort = null;
  }
};
