// Per-session context customization: which built-in tools are enabled,
// system prompt template override, agent instructions (an AGENTS.md
// equivalent), and context files fetched by URL. Persisted to localStorage,
// same pattern as settings.js.
import { ALL_TOOLS } from "./tools.js";
import { readFileText } from "./workspace-fs.js";

const STORAGE_KEY = "cooper.context.v1";

const $ = (id) => document.getElementById(id);

let context;

function uid() {
  return crypto.randomUUID();
}

function seed() {
  return {
    enabledTools: Object.fromEntries(Object.keys(ALL_TOOLS).map((name) => [name, true])),
    systemPromptTemplate: "",
    agentInstructions: "",
    contextFiles: [],
  };
}

function load() {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw) {
    try {
      const stored = JSON.parse(raw);
      // Any built-in tool added since this was last saved defaults to enabled.
      const enabledTools = { ...seed().enabledTools, ...stored.enabledTools };
      return { ...seed(), ...stored, enabledTools };
    } catch {
      // fall through to a fresh seed if the stored value is corrupt
    }
  }
  return seed();
}

function persist() {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(context));
}

function derivePath(url) {
  try {
    const u = new URL(url);
    const path = `${u.hostname}${u.pathname}`;
    return path.endsWith("/") ? path.slice(0, -1) : path;
  } catch {
    return url;
  }
}

export function getEnabledToolNames() {
  return Object.entries(context.enabledTools)
    .filter(([, enabled]) => enabled)
    .map(([name]) => name);
}

export function getContextConfig() {
  return {
    systemPromptTemplate: context.systemPromptTemplate.trim() || null,
    agentInstructions: context.agentInstructions.trim() || null,
    contextFiles: context.contextFiles
      .filter((f) => !f.loading && !f.error)
      .map((f) => ({ path: f.path, content: f.content })),
  };
}

function closeAllTooltips() {
  for (const el of document.querySelectorAll(".tool-info.is-open")) {
    el.classList.remove("is-open");
  }
}

function allToolsEnabled() {
  return Object.keys(ALL_TOOLS).every((name) => context.enabledTools[name]);
}

// The toggle's label always names the action it would perform, so it reads
// "select none" while everything is checked and "select all" otherwise.
function updateToolsSelectToggle() {
  $("tools-select-toggle").textContent = allToolsEnabled() ? "select none" : "select all";
}

function setAllTools(enabled) {
  for (const name of Object.keys(ALL_TOOLS)) {
    context.enabledTools[name] = enabled;
  }
  persist();
  renderToolList();
}

function renderToolList() {
  const container = $("tool-list");
  container.innerHTML = "";
  updateToolsSelectToggle();

  for (const [name, tool] of Object.entries(ALL_TOOLS)) {
    const row = document.createElement("label");
    row.className = "tool-row";

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = Boolean(context.enabledTools[name]);
    checkbox.addEventListener("change", () => {
      context.enabledTools[name] = checkbox.checked;
      persist();
      updateToolsSelectToggle();
    });

    const label = document.createElement("span");
    label.className = "tool-row-name";
    label.textContent = name;

    const info = document.createElement("span");
    info.className = "tool-info";
    info.tabIndex = 0;
    info.textContent = "?";
    const tooltip = document.createElement("span");
    tooltip.className = "tool-tooltip";
    tooltip.textContent = tool.schema.description;
    info.appendChild(tooltip);

    const toggleTooltip = (event) => {
      // Prevent the enclosing <label> from toggling the checkbox.
      event.preventDefault();
      event.stopPropagation();
      const isOpen = info.classList.contains("is-open");
      closeAllTooltips();
      if (!isOpen) info.classList.add("is-open");
    };
    info.addEventListener("click", toggleTooltip);
    info.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") toggleTooltip(event);
    });

    row.append(checkbox, label, info);
    container.appendChild(row);
  }
}

function renderFileList() {
  const container = $("context-file-list");
  container.innerHTML = "";
  if (context.contextFiles.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "No context files added yet.";
    container.appendChild(empty);
    return;
  }

  for (const file of context.contextFiles) {
    const row = document.createElement("div");
    row.className = "context-file";

    const info = document.createElement("div");
    info.className = "context-file-info";

    const path = document.createElement("span");
    path.className = "context-file-path";
    path.textContent = (file.source === "workspace" ? "📁 " : "🌐 ") + file.path;

    const status = document.createElement("span");
    status.className = "context-file-status";
    if (file.loading) {
      status.textContent = "fetching…";
    } else if (file.error) {
      status.textContent = `error: ${file.error}`;
      status.classList.add("is-error");
    } else {
      status.textContent = `${file.content.length.toLocaleString()} chars`;
    }

    info.append(path, status);

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "icon-btn";
    removeBtn.textContent = "✕";
    removeBtn.title = "Remove";
    removeBtn.addEventListener("click", () => {
      context.contextFiles = context.contextFiles.filter((f) => f.id !== file.id);
      persist();
      renderFileList();
    });

    row.append(info, removeBtn);
    container.appendChild(row);
  }
}

async function addContextFileFromUrl(url) {
  const file = {
    id: uid(),
    url,
    path: derivePath(url),
    content: "",
    error: null,
    loading: true,
    source: "url",
  };
  context.contextFiles.push(file);
  persist();
  renderFileList();

  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    file.content = await res.text();
  } catch (err) {
    file.error = err.message ?? String(err);
  } finally {
    file.loading = false;
    persist();
    renderFileList();
  }
}

// Reads a file out of the Workspace (OPFS) at the moment it's added — a
// one-time snapshot, same as a URL fetch, not a live link. If the workspace
// file changes afterwards, re-add it to refresh the snapshot; this keeps
// "what's injected into the prompt" exactly equal to what's listed here,
// with no hidden re-fetching behind the scenes.
async function addContextFileFromWorkspace(path) {
  const file = {
    id: uid(),
    path,
    content: "",
    error: null,
    loading: true,
    source: "workspace",
  };
  context.contextFiles.push(file);
  persist();
  renderFileList();

  try {
    file.content = await readFileText(path);
  } catch (err) {
    file.error = err.message ?? String(err);
  } finally {
    file.loading = false;
    persist();
    renderFileList();
  }
}

export function initContext() {
  context = load();

  $("system-prompt-template").value = context.systemPromptTemplate;
  $("agent-instructions").value = context.agentInstructions;
  renderToolList();
  $("tools-select-toggle").addEventListener("click", () => setAllTools(!allToolsEnabled()));
  document.addEventListener("click", closeAllTooltips);
  renderFileList();

  $("system-prompt-template").addEventListener("change", (event) => {
    context.systemPromptTemplate = event.target.value;
    persist();
  });

  $("agent-instructions").addEventListener("change", (event) => {
    context.agentInstructions = event.target.value;
    persist();
  });

  $("add-context-file-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const input = $("new-context-file-url");
    const url = input.value.trim();
    if (!url) return;
    input.value = "";
    addContextFileFromUrl(url);
  });

  $("add-context-file-workspace-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const input = $("new-context-file-path");
    const path = input.value.trim();
    if (!path) return;
    input.value = "";
    addContextFileFromWorkspace(path);
  });
}
