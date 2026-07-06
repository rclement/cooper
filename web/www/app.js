// Main-thread UI glue: wires the form to the agent Worker and renders the
// events it streams back. No framework, no build step.
import { initSettings, getCurrentConfig } from "./settings.js";
import { initContext, getContextConfig, getEnabledToolNames } from "./context.js";
import { renderMarkdown } from "./markdown.js";
import { saveSession, listSessions, deleteSession } from "./sessions.js";
import { initWorkspace, refreshWorkspace } from "./workspace.js";

// `let` + factory (not a one-shot const): the Stop button's escalation path
// terminates the worker mid-run — losing the in-memory agent and, for local
// models, the loaded weights — and replaces it with a fresh one. Sessions
// still resume afterwards because run messages carry `restoreHistory`.
let worker = null;

function createWorker() {
  worker = new Worker("worker.js", { type: "module" });
  worker.onmessage = handleWorkerMessage;
}

const $ = (id) => document.getElementById(id);

initSettings();
initContext();
initWorkspace();

for (const navItem of document.querySelectorAll(".nav-item")) {
  navItem.addEventListener("click", () => {
    for (const item of document.querySelectorAll(".nav-item")) {
      item.classList.toggle("is-active", item === navItem);
    }
    for (const view of document.querySelectorAll(".view")) {
      view.classList.toggle("is-active", view.id === `view-${navItem.dataset.view}`);
    }
    if (navItem.dataset.view === "workspace") refreshWorkspace();
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

// Placeholder block shown while a local model is prefilling/decoding but
// hasn't streamed anything yet (the silent stretch right after the prompt —
// and after every tool result). The first real event replaces it, so it
// only ever lives at the bottom of the timeline.
let generatingEl = null;

function showGenerating() {
  if (generatingEl) return;
  const el = document.createElement("div");
  el.className = "block block-generating";

  const header = document.createElement("div");
  header.className = "block-header";

  const icon = document.createElement("span");
  icon.className = "block-icon is-active";
  icon.textContent = "◌";

  const label = document.createElement("span");
  label.className = "block-label";
  label.textContent = "Generating";

  header.append(icon, label);
  el.appendChild(header);
  $("timeline").appendChild(el);
  generatingEl = el;
}

function clearGenerating() {
  generatingEl?.remove();
  generatingEl = null;
}

function handleEvent(event) {
  clearGenerating();
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

// A session spans multiple prompt/response turns sharing one conversation
// history. `currentSession` is null when the next Run click should start a
// fresh one; otherwise it's the record being built up (or resumed). The
// only persisted payload is `history` — the wasm-exported `Vec<Message>`
// JSON, the same structure `agent_loop_stream` threads through the CLI's
// `chat` command. It's already the right granularity to persist: one entry
// per message (system/user/assistant/tool), never per streamed SSE delta —
// unlike an earlier version of this file, which recorded the *live* event
// stream (including every chunk) into its own separate log and had to
// special-case coalescing consecutive chunks back together. Using the same
// history the agent loop already produces sidesteps that class of bug
// entirely, and means a saved session's timeline is reconstructed with
// `renderHistory` below rather than replayed event-by-event.
let currentSession = null;

function uid() {
  return crypto.randomUUID();
}

function clearTimeline() {
  $("timeline").innerHTML = "";
  current = { type: null, body: null, preview: null, raw: "", icon: null };
  generatingEl = null;
}

function startNewSession() {
  currentSession = null;
  clearTimeline();
  $("status").textContent = "";
  renderSessionList();
}

$("new-session").addEventListener("click", startNewSession);

function handleWorkerMessage(message) {
  const msg = message.data;
  if (msg.type === "event") {
    handleEvent(msg.event);
  } else if (msg.type === "done") {
    stopPulse();
    clearGenerating();
    $("status").textContent = "done";
    setRunning(false);
    if (currentSession) {
      currentSession.updatedAt = Date.now();
      currentSession.history = msg.history;
      saveSession(currentSession).then(renderSessionList);
    }
  } else if (msg.type === "error") {
    stopPulse();
    clearGenerating();
    $("status").textContent = `error: ${msg.error}`;
    setRunning(false);
    if (currentSession && msg.history) {
      currentSession.updatedAt = Date.now();
      currentSession.history = msg.history;
      saveSession(currentSession).then(renderSessionList);
    }
  } else if (msg.type === "model_status") {
    if (msg.status === "generating") showGenerating();
    $("status").textContent =
      msg.status === "loading"
        ? "loading local model…"
        : msg.status === "generating"
          ? "generating…"
          : "running…";
  } else if (msg.type === "model_progress") {
    const loadedMb = (msg.loaded / (1024 * 1024)).toFixed(0);
    if (msg.total) {
      const totalMb = (msg.total / (1024 * 1024)).toFixed(0);
      const pct = Math.round((msg.loaded / msg.total) * 100);
      $("status").textContent = `downloading model… ${pct}% (${loadedMb} / ${totalMb} MB)`;
    } else {
      $("status").textContent = `downloading model… ${loadedMb} MB`;
    }
  }
}

createWorker();

// Stop handling: two levels. For a local run, the first click asks the
// worker to abort the in-flight completion (the turn then completes with
// whatever partial output exists, and the loaded model survives). A second
// click — or any click during a remote run, whose HTTP request can't be
// cancelled from here — terminates the worker outright and replaces it.
let runningIsLocal = false;
let stopRequested = false;

function setRunning(running) {
  $("run").disabled = running;
  $("stop").disabled = !running;
  if (!running) stopRequested = false;
}

$("stop").addEventListener("click", () => {
  if ($("stop").disabled) return;
  if (runningIsLocal && !stopRequested) {
    stopRequested = true;
    $("status").textContent = "stopping…";
    worker.postMessage({ type: "stop" });
    return;
  }
  worker.terminate();
  createWorker();
  stopPulse();
  clearGenerating();
  $("status").textContent = "stopped";
  setRunning(false);
});

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
  const newSession = !currentSession;

  if (newSession) {
    clearTimeline();
    currentSession = {
      id: uid(),
      title: prompt.slice(0, 80),
      createdAt: Date.now(),
      updatedAt: Date.now(),
      providerName: providerConfig.providerName,
      providerId: providerConfig.providerId,
      model: providerConfig.model,
      // Local models: the select's option value is the catalog id, not the
      // display name stored in `model` — needed to re-select on restore.
      modelId: providerConfig.modelId ?? providerConfig.model,
      history: null,
    };
  }

  appendPrompt(prompt);
  $("status").textContent = "running…";
  runningIsLocal = config.provider_type === "local";
  setRunning(true);
  $("prompt").value = "";

  worker.postMessage({
    prompt,
    config,
    enabledTools,
    newSession,
    restoreHistory: newSession ? null : currentSession.history,
  });
});

// --- Past sessions: list, load (replay + resume), delete ---

function formatRelativeTime(ms) {
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}

// Reconstructs the timeline from a session's persisted `Vec<Message>`
// history — the exact JSON shape `WasmAgent::export_history` produces
// (externally-tagged: `{ System: "..." }`, `{ User: "..." }`, `{ Assistant:
// { text, reasoning, tool_calls } }`, `{ Tool: { call_id, result } }`).
// Since it's message-level, not delta-level, this always renders one block
// per message regardless of how many SSE chunks the model streamed it in.
function renderHistory(historyJson) {
  const messages = JSON.parse(historyJson);
  for (const message of messages) {
    if (message.System !== undefined) {
      appendSystemPrompt(message.System);
    } else if (message.User !== undefined) {
      appendPrompt(message.User);
    } else if (message.Assistant !== undefined) {
      const { text, reasoning, tool_calls } = message.Assistant;
      if (reasoning) appendReasoning(reasoning);
      if (text) appendResponse(text);
      for (const toolCall of tool_calls) {
        appendToolCall(toolCall);
      }
    } else if (message.Tool !== undefined) {
      appendToolResult({ result: message.Tool.result });
    }
  }
}

function loadSession(session) {
  currentSession = session;
  clearTimeline();
  if (session.history) renderHistory(session.history);

  // Point the config UI at the provider/model this session used, so
  // continuing the conversation reuses the same connection. If that
  // provider was since removed, leave the current selection as-is — the
  // status message flags it so the user can pick a replacement.
  const providerSelect = $("provider-select");
  providerSelect.value = session.providerId;
  providerSelect.dispatchEvent(new Event("change"));
  const modelSelect = $("model-select");
  modelSelect.value = session.modelId ?? session.model;
  modelSelect.dispatchEvent(new Event("change"));

  $("status").textContent =
    providerSelect.value === session.providerId
      ? "loaded session — continue chatting or start a new one"
      : `loaded session — original provider "${session.providerName}" is no longer configured`;

  renderSessionList();
}

async function renderSessionList() {
  const container = $("session-list");
  const sessions = await listSessions();

  container.innerHTML = "";
  if (sessions.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "No saved sessions yet.";
    container.appendChild(empty);
    return;
  }

  for (const session of sessions) {
    const item = document.createElement("div");
    item.className =
      "session-item" + (session.id === currentSession?.id ? " is-active" : "");
    item.addEventListener("click", () => loadSession(session));

    const info = document.createElement("div");
    info.className = "session-item-info";

    const title = document.createElement("div");
    title.className = "session-item-title";
    title.textContent = session.title || "(empty prompt)";

    const meta = document.createElement("div");
    meta.className = "session-item-meta";
    meta.textContent = `${formatRelativeTime(session.updatedAt)} · ${session.model}`;

    info.append(title, meta);

    const deleteBtn = document.createElement("button");
    deleteBtn.type = "button";
    deleteBtn.className = "session-item-delete";
    deleteBtn.textContent = "✕";
    deleteBtn.title = "Delete session";
    deleteBtn.addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!confirm(`Delete session "${session.title}"?`)) return;
      await deleteSession(session.id);
      if (currentSession?.id === session.id) startNewSession();
      else renderSessionList();
    });

    item.append(info, deleteBtn);
    container.appendChild(item);
  }
}

renderSessionList();
