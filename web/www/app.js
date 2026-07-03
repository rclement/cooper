// Main-thread UI glue: wires the form to the agent Worker and renders the
// events it streams back. No framework, no build step.
import { initSettings, getCurrentConfig } from "./settings.js";
import { initContext, getContextConfig, getEnabledToolNames } from "./context.js";
import { renderMarkdown } from "./markdown.js";

const worker = new Worker("worker.js", { type: "module" });

const $ = (id) => document.getElementById(id);

initSettings();
initContext();

for (const navItem of document.querySelectorAll(".nav-item")) {
  navItem.addEventListener("click", () => {
    for (const item of document.querySelectorAll(".nav-item")) {
      item.classList.toggle("is-active", item === navItem);
    }
    for (const view of document.querySelectorAll(".view")) {
      view.classList.toggle("is-active", view.id === `view-${navItem.dataset.view}`);
    }
  });
}

// Renders the agent's stream as a vertical sequence of typed blocks
// (reasoning / response / tool call / usage), each visually distinct.
// Reasoning and tool blocks are collapsed by default with a one-line
// preview; a new block starts whenever the event type changes, so each
// turn's reasoning/response/tool-call naturally gets its own block.
const BLOCK_KIND = {
  prompt: { label: "You", icon: "›", collapsible: false },
  context: { label: "Context", icon: "▤", collapsible: true },
  reasoning: { label: "Reasoning", icon: "◌", collapsible: true },
  response: { label: "Response", icon: "◆", collapsible: false },
  tool: { label: "Tool", icon: "⚙", collapsible: true },
};

// Blocks that represent work happening "behind the scenes" (collapsed by
// default, so there's otherwise no visual cue anything is going on) get a
// pulsing icon for as long as they're the block actively being written to.
const PULSING_KINDS = new Set(["reasoning", "tool"]);

let current = { type: null, body: null, preview: null, raw: "", icon: null };

function truncate(text, max = 90) {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

function stopPulse() {
  current.icon?.classList.remove("is-active");
}

function openBlock(type) {
  stopPulse();

  const kind = BLOCK_KIND[type];
  const el = document.createElement(kind.collapsible ? "details" : "div");
  el.className = `block block-${type}`;

  const header = document.createElement(kind.collapsible ? "summary" : "div");
  header.className = "block-header";

  const icon = document.createElement("span");
  icon.className = "block-icon";
  icon.textContent = kind.icon;
  if (PULSING_KINDS.has(type)) icon.classList.add("is-active");

  const label = document.createElement("span");
  label.className = "block-label";
  label.textContent = kind.label;

  header.append(icon, label);

  let preview = null;
  if (kind.collapsible) {
    preview = document.createElement("span");
    preview.className = "block-preview";
    header.appendChild(preview);
  }

  const body = document.createElement("div");
  body.className = "block-body";

  el.append(header, body);
  $("timeline").appendChild(el);

  current = { type, body, preview, raw: "", icon };
}

function appendPrompt(text) {
  openBlock("prompt");
  current.body.textContent = text;
  current.type = null; // one-shot; next event starts a fresh block
}

function appendSystemPrompt(text) {
  openBlock("context");
  current.body.textContent = text;
  current.preview.textContent = `${text.length.toLocaleString()} chars`;
  current.type = null; // one-shot; next event starts a fresh block
}

function appendReasoning(text) {
  if (current.type !== "reasoning") openBlock("reasoning");
  current.body.textContent += text;
  current.preview.textContent = truncate(current.body.textContent);
}

function appendResponse(text) {
  if (current.type !== "response") openBlock("response");
  current.raw += text;
  current.body.innerHTML = renderMarkdown(current.raw);
}

function appendToolCall(event) {
  openBlock("tool");
  const argsPreview = JSON.stringify(event.arguments);

  const line = document.createElement("div");
  line.className = "tool-line tool-call";
  line.textContent = `▶ ${event.name} ${argsPreview}`;
  current.body.appendChild(line);

  current.preview.textContent = truncate(`${event.name} ${argsPreview}`, 60);
}

function appendToolResult(event) {
  if (current.type !== "tool") openBlock("tool");
  const { Ok, Err } = event.result;
  const isError = Err !== undefined;

  const line = document.createElement("div");
  line.className = "tool-line tool-result" + (isError ? " is-error" : "");
  line.textContent = isError ? `◀ error: ${Err}` : `◀ ${Ok}`;
  current.body.appendChild(line);

  current.preview.textContent = `${isError ? "✗" : "✓"} ${current.preview.textContent}`;
  stopPulse();
  current.type = null; // next event starts a fresh block, even if also a tool call
}

function appendUsage(event) {
  stopPulse();
  const el = document.createElement("div");
  el.className = "block block-usage";
  el.textContent =
    `${event.total_tokens} tokens` +
    ` · ${event.prompt_tokens} prompt · ${event.completion_tokens} completion`;
  $("timeline").appendChild(el);
  current = { type: null, body: null, preview: null, raw: "", icon: null };
}

function handleEvent(event) {
  switch (event.type) {
    case "system_prompt":
      appendSystemPrompt(event.text);
      break;
    case "chunk":
      if (event.reasoning) appendReasoning(event.reasoning);
      if (event.text) appendResponse(event.text);
      break;
    case "usage":
      appendUsage(event);
      break;
    case "tool_call":
      appendToolCall(event);
      break;
    case "tool_result":
      appendToolResult(event);
      break;
  }
}

worker.onmessage = (message) => {
  const msg = message.data;
  if (msg.type === "event") {
    handleEvent(msg.event);
  } else if (msg.type === "done") {
    stopPulse();
    $("status").textContent = "done";
    $("run").disabled = false;
  } else if (msg.type === "error") {
    stopPulse();
    $("status").textContent = `error: ${msg.error}`;
    $("run").disabled = false;
  }
};

// A session spans multiple prompt/response turns sharing one conversation
// history (kept on the wasm side). `sessionActive` tracks whether the next
// Run click is a follow-up turn (append to the timeline) or the start of a
// new session (clear it and tell the worker to build a fresh WasmAgent).
let sessionActive = false;

function startNewSession() {
  sessionActive = false;
  $("timeline").innerHTML = "";
  current = { type: null, body: null, preview: null, raw: "", icon: null };
  $("status").textContent = "";
}

$("new-session").addEventListener("click", startNewSession);

$("run").addEventListener("click", () => {
  const prompt = $("prompt").value.trim();
  if (!prompt) return;

  const providerConfig = getCurrentConfig();
  if (!providerConfig) {
    $("status").textContent = "error: configure a provider and model first";
    return;
  }
  const contextConfig = getContextConfig();
  const config = {
    ...providerConfig,
    system_prompt_template: contextConfig.systemPromptTemplate,
    agent_instructions: contextConfig.agentInstructions,
    context_files: contextConfig.contextFiles,
  };
  const enabledTools = getEnabledToolNames();
  const newSession = !sessionActive;

  if (newSession) {
    $("timeline").innerHTML = "";
    current = { type: null, body: null, preview: null, raw: "", icon: null };
  }
  appendPrompt(prompt);
  $("status").textContent = "running…";
  $("run").disabled = true;
  $("prompt").value = "";

  worker.postMessage({ prompt, config, enabledTools, newSession });
  sessionActive = true;
});
