// Main-thread UI glue: wires the form to the agent Worker and renders the
// events it streams back. No framework, no build step.
import { initSettings, getCurrentConfig } from "./settings.js";
import { initContext, getContextConfig, getEnabledToolNames } from "./context.js";
import { renderMarkdown } from "./markdown.js";
import { saveSession, listSessions, deleteSession } from "./sessions.js";
import { initWorkspace, refreshWorkspace } from "./workspace.js";
import { initAnalytics, refreshAnalytics } from "./analytics.js";
import { parseChartCall } from "./chart-common.js";
import { renderChart } from "./chart-render.js";
import { renderImage, renderSvg } from "./media-render.js";

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
initAnalytics();

for (const navItem of document.querySelectorAll(".nav-item")) {
  navItem.addEventListener("click", () => {
    for (const item of document.querySelectorAll(".nav-item")) {
      item.classList.toggle("is-active", item === navItem);
    }
    for (const view of document.querySelectorAll(".view")) {
      view.classList.toggle("is-active", view.id === `view-${navItem.dataset.view}`);
    }
    if (navItem.dataset.view === "workspace") refreshWorkspace();
    if (navItem.dataset.view === "analytics") refreshAnalytics();
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
  chart: { label: "Chart", icon: "▥", collapsible: false },
  image: { label: "Image", icon: "🖼", collapsible: false },
  svg: { label: "SVG", icon: "▨", collapsible: false },
};

// Blocks that represent work happening "behind the scenes" (collapsed by
// default, so there's otherwise no visual cue anything is going on) get a
// pulsing icon for as long as they're the block actively being written to.
const PULSING_KINDS = new Set(["reasoning", "tool"]);

let current = { type: null, body: null, preview: null, raw: "", icon: null, duration: null };

function truncate(text, max = 90) {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

function formatDuration(ms) {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
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

  const duration = document.createElement("span");
  duration.className = "block-duration";
  header.append(icon, label, duration);

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

  current = { type, body, preview, raw: "", icon, duration };
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

// The reasoning/response blocks of the round currently streaming. Kept so
// the round's finalized `message` event (which carries the measured
// durations) can stamp them onto the right blocks even though `current` may
// have moved on by then. Cleared when the round is finalized.
let roundBlocks = { reasoning: null, response: null };

function appendReasoning(text) {
  if (current.type !== "reasoning") {
    openBlock("reasoning");
    roundBlocks.reasoning = current;
  }
  current.body.textContent += text;
  current.preview.textContent = truncate(current.body.textContent);
}

function appendResponse(text) {
  if (current.type !== "response") {
    openBlock("response");
    roundBlocks.response = current;
  }
  current.raw += text;
  current.body.innerHTML = renderMarkdown(current.raw);
}

// A tool call always gets the normal collapsed "Tool" block — name, full
// arguments, and (once it arrives) the result — same as every other tool.
// Nothing is special-cased away: this is the one place to check exactly what
// the model sent when a chart/image/SVG looks wrong (wrong field,
// hallucinated rows, stale path), so hiding it behind the pretty rendering
// would trade away the "no black magic" observability this app is built
// around.
//
// The tools below *additionally* get a dedicated block rendered right after,
// straight from these same tool_call arguments — see the appendXBlock
// functions. The tool block stays addressable (`pendingToolResultBlock`) so
// the tool_result that follows lands on it instead of on the block that's
// `current` by then.
const INLINE_TOOL_RENDERERS = {
  render_chart: appendChartBlock,
  show_image: appendImageBlock,
  show_svg: appendSvgBlock,
};

let pendingToolResultBlock = null;

function appendToolCall(event) {
  openBlock("tool");
  const argsPreview = JSON.stringify(event.arguments);

  const line = document.createElement("div");
  line.className = "tool-line tool-call";
  line.textContent = `▶ ${event.name} ${argsPreview}`;
  current.body.appendChild(line);

  current.preview.textContent = truncate(`${event.name} ${argsPreview}`, 60);

  const renderer = INLINE_TOOL_RENDERERS[event.name];
  if (renderer) {
    pendingToolResultBlock = current;
    renderer(event);
  }
}

function inlineRenderError(body, prefix, err) {
  const msg = document.createElement("p");
  msg.className = "hint";
  msg.textContent = `${prefix}: ${err.message ?? err}`;
  body.appendChild(msg);
}

// Renders straight from the tool_call's own arguments rather than waiting on
// the tool_result (which is just a plain confirmation string for the
// agent's own benefit; see chart-tool.js).
function appendChartBlock(event) {
  openBlock("chart");
  try {
    const { rows, spec } = parseChartCall(JSON.stringify(event.arguments));
    renderChart(current.body, rows, spec);
  } catch (err) {
    inlineRenderError(current.body, "Couldn't render chart", err);
  }
}

// Async (reads the file from OPFS) — `body` is captured up front so the
// result lands in the right block even if `current` has moved on to a later
// event by the time the read resolves.
function appendImageBlock(event) {
  openBlock("image");
  const body = current.body;
  const { path, caption } = event.arguments;
  renderImage(body, path, caption).catch((err) => inlineRenderError(body, "Couldn't display image", err));
}

function appendSvgBlock(event) {
  openBlock("svg");
  const { svg, caption } = event.arguments;
  try {
    renderSvg(current.body, svg, caption);
  } catch (err) {
    inlineRenderError(current.body, "Couldn't render SVG", err);
  }
}

// A finalized assistant round arrived — the message carries the measured
// reasoning/response durations and the provider-reported token usage. The
// content itself already streamed in as chunks (live) or was just rendered
// (replay); here we only stamp the metadata onto this round's blocks.
function applyAssistantMessage(assistant) {
  if (roundBlocks.reasoning?.duration && assistant.reasoning_duration_ms != null) {
    roundBlocks.reasoning.duration.textContent = formatDuration(assistant.reasoning_duration_ms);
  }
  if (roundBlocks.response?.duration && assistant.response_duration_ms != null) {
    roundBlocks.response.duration.textContent = formatDuration(assistant.response_duration_ms);
  }
  roundBlocks = { reasoning: null, response: null };

  if (assistant.usage) appendUsage(assistant.usage);
}

// A finalized tool result arrived. Normally the tool block itself is still
// `current` and gets the result appended in place. For an
// INLINE_TOOL_RENDERERS tool, a chart/image/svg block was opened right after
// it, so `current` now points there instead — route the result back onto the
// stashed tool block rather than losing it.
function applyToolMessage(tool) {
  if (pendingToolResultBlock === null && current.type !== "tool") openBlock("tool");
  const block = pendingToolResultBlock ?? current;
  pendingToolResultBlock = null;

  const { Ok, Err } = tool.result;
  const isError = Err !== undefined;

  const line = document.createElement("div");
  line.className = "tool-line tool-result" + (isError ? " is-error" : "");
  line.textContent = isError ? `◀ error: ${Err}` : `◀ ${Ok}`;
  block.body.appendChild(line);
  block.preview.textContent = `${isError ? "✗" : "✓"} ${block.preview.textContent}`;
  if (block.duration && tool.duration_ms != null) {
    block.duration.textContent = formatDuration(tool.duration_ms);
  }

  stopPulse();
  // Whichever block is actually open right now (the tool block in the
  // normal case, or the chart/image/svg block for an inline-rendered tool) —
  // either way, the next event should start a fresh block rather than
  // appending into this one.
  current.type = null;
}

function appendUsage(usage) {
  stopPulse();
  const el = document.createElement("div");
  el.className = "block block-usage";
  el.textContent =
    `${usage.total_tokens} tokens` +
    ` · ${usage.prompt_tokens} prompt · ${usage.completion_tokens} completion`;
  $("timeline").appendChild(el);
  current = { type: null, body: null, preview: null, raw: "", icon: null, duration: null };
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
    case "chunk":
      if (event.reasoning) appendReasoning(event.reasoning);
      if (event.text) appendResponse(event.text);
      break;
    case "tool_call":
      appendToolCall(event);
      break;
    // A finalized message — same JSON shape as one entry of the exported
    // history (externally tagged: `{ System: "..." }`, `{ Assistant: {...} }`,
    // `{ Tool: {...} }`), so this and renderHistory share the same code.
    case "message":
      if (event.message.System !== undefined) appendSystemPrompt(event.message.System);
      if (event.message.Assistant) applyAssistantMessage(event.message.Assistant);
      if (event.message.Tool) applyToolMessage(event.message.Tool);
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
  current = { type: null, body: null, preview: null, raw: "", icon: null, duration: null };
  roundBlocks = { reasoning: null, response: null };
  pendingToolResultBlock = null;
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
    setRunning(false);
    if (currentSession) {
      currentSession.updatedAt = Date.now();
      currentSession.history = msg.history;
      // "done" is announced only once the session is actually persisted.
      // Announcing it before the IndexedDB write commits would invite
      // navigating away while the write is in flight — navigation aborts
      // pending transactions, silently losing the turn.
      saveSession(currentSession).then(() => {
        renderSessionList();
        $("status").textContent = "done";
      });
    } else {
      $("status").textContent = "done";
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
      // Render the content that streamed in as chunks live, then apply the
      // same metadata finalization the live `message` event goes through —
      // durations, usage and results replay for free because they were
      // persisted on the message itself.
      const { text, reasoning, tool_calls } = message.Assistant;
      if (reasoning) appendReasoning(reasoning);
      if (text) appendResponse(text);
      applyAssistantMessage(message.Assistant);
      for (const toolCall of tool_calls) {
        appendToolCall(toolCall);
      }
    } else if (message.Tool !== undefined) {
      applyToolMessage(message.Tool);
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
