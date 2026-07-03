// Per-session context customization: system prompt template override, agent
// instructions (an AGENTS.md equivalent), and context files fetched by URL.
// Persisted to localStorage, same pattern as settings.js.
const STORAGE_KEY = "cooper.context.v1";

const $ = (id) => document.getElementById(id);

let context;

function uid() {
  return crypto.randomUUID();
}

function seed() {
  return { systemPromptTemplate: "", agentInstructions: "", contextFiles: [] };
}

function load() {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw) {
    try {
      return { ...seed(), ...JSON.parse(raw) };
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

export function getContextConfig() {
  return {
    systemPromptTemplate: context.systemPromptTemplate.trim() || null,
    agentInstructions: context.agentInstructions.trim() || null,
    contextFiles: context.contextFiles
      .filter((f) => !f.loading && !f.error)
      .map((f) => ({ path: f.path, content: f.content })),
  };
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
    path.textContent = file.path;

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

export function initContext() {
  context = load();

  $("system-prompt-template").value = context.systemPromptTemplate;
  $("agent-instructions").value = context.agentInstructions;
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
}
