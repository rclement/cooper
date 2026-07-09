// Provider/model settings: persisted to localStorage, with a small CRUD
// panel rendered directly into the DOM (no framework, no build step).
import { LOCAL_PROVIDER_ID, LOCAL_MODEL_CATALOG } from "./local-models.js";

const STORAGE_KEY = "cooper.settings.v1";
const LOCAL_PROVIDER_NAME = "Local (in-browser)";

const $ = (id) => document.getElementById(id);

let settings;

function uid() {
  return crypto.randomUUID();
}

function seedSettings() {
  return {
    providers: [],
    defaultProviderId: null,
    defaultModel: null,
    localModels: [],
  };
}

function load() {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw) {
    try {
      const parsed = JSON.parse(raw);
      // Tolerate settings persisted before `localModels` existed.
      if (!Array.isArray(parsed.localModels)) parsed.localModels = [];
      return parsed;
    } catch {
      // fall through to a fresh seed if the stored value is corrupt
    }
  }
  return seedSettings();
}

function persist() {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
}

function findProvider(id) {
  return settings.providers.find((p) => p.id === id);
}

// The local provider isn't stored in `settings.providers` (it's built-in and
// can't be removed) — its model list is the curated catalog plus whatever
// custom GGUF URLs the user has added.
function getLocalModels() {
  return [...LOCAL_MODEL_CATALOG, ...(settings.localModels ?? [])];
}

function findLocalModel(id) {
  return getLocalModels().find((m) => m.id === id);
}

/// Keeps `defaultProviderId`/`defaultModel` pointing at something that still
/// exists, falling back to the first provider/model after any removal.
function ensureValidDefaults() {
  if (settings.defaultProviderId === LOCAL_PROVIDER_ID) {
    if (!findLocalModel(settings.defaultModel)) {
      settings.defaultModel = getLocalModels()[0]?.id ?? null;
    }
    return;
  }
  let provider = findProvider(settings.defaultProviderId);
  if (!provider) {
    provider = settings.providers[0];
    settings.defaultProviderId = provider?.id ?? null;
  }
  if (!provider) {
    // No remote provider configured — fall back to the built-in local one,
    // so a fresh install is usable without any setup at all.
    settings.defaultProviderId = LOCAL_PROVIDER_ID;
    settings.defaultModel = getLocalModels()[0]?.id ?? null;
    return;
  }
  if (!provider.models.includes(settings.defaultModel)) {
    settings.defaultModel = provider.models[0] ?? null;
  }
}

export function getCurrentConfig() {
  if (settings.defaultProviderId === LOCAL_PROVIDER_ID) {
    const model = findLocalModel(settings.defaultModel);
    if (!model) return null;
    return {
      provider_type: "local",
      model: model.name,
      model_url: model.url,
      // Not sent to the agent — for the caller to attach to session
      // metadata. `modelId` is what `#model-select` uses as option value
      // (unlike remote providers, where the model name is the value), so
      // session restore needs it to re-select the model.
      providerId: LOCAL_PROVIDER_ID,
      providerName: LOCAL_PROVIDER_NAME,
      modelId: model.id,
    };
  }
  const provider = findProvider(settings.defaultProviderId);
  if (!provider || !settings.defaultModel) return null;
  return {
    base_url: provider.baseUrl,
    api_key: provider.apiKey,
    model: settings.defaultModel,
    // Not sent to the agent — for the caller to attach to session metadata
    // without persisting the API key itself.
    providerId: provider.id,
    providerName: provider.name,
  };
}

function fieldRow(labelText, value, onChange, type = "text") {
  const field = document.createElement("div");
  field.className = "field";

  const label = document.createElement("label");
  label.textContent = labelText;

  const input = document.createElement("input");
  input.type = type;
  input.value = value;
  input.addEventListener("change", () => onChange(input.value));

  field.append(label, input);
  return field;
}

function renderProviderBlock(provider) {
  const block = document.createElement("div");
  block.className = "provider";

  const header = document.createElement("div");
  header.className = "provider-header";

  const isDefaultProvider = provider.id === settings.defaultProviderId;

  const nameInput = document.createElement("input");
  nameInput.className = "provider-name";
  nameInput.value = provider.name;
  nameInput.addEventListener("change", () => {
    provider.name = nameInput.value.trim() || provider.name;
    persist();
    renderProviderSelect();
  });

  const removeBtn = document.createElement("button");
  removeBtn.type = "button";
  removeBtn.className = "icon-btn";
  removeBtn.textContent = "✕";
  removeBtn.title = "Remove provider";
  removeBtn.addEventListener("click", () => {
    if (!confirm(`Remove provider "${provider.name}" and all its models?`)) return;
    settings.providers = settings.providers.filter((p) => p.id !== provider.id);
    ensureValidDefaults();
    persist();
    renderAll();
  });

  header.append(nameInput, removeBtn);

  const fields = document.createElement("div");
  fields.className = "provider-fields";
  fields.append(
    fieldRow("Base URL", provider.baseUrl, (value) => {
      provider.baseUrl = value;
      persist();
    }),
    fieldRow(
      "API key",
      provider.apiKey,
      (value) => {
        provider.apiKey = value;
        persist();
      },
      "password",
    ),
  );

  const modelsSection = document.createElement("div");
  modelsSection.className = "models";

  const modelsLabel = document.createElement("label");
  modelsLabel.textContent = "Models";
  modelsSection.appendChild(modelsLabel);

  const tags = document.createElement("div");
  tags.className = "model-tags";
  for (const model of provider.models) {
    const isDefaultModel = isDefaultProvider && model === settings.defaultModel;

    const tag = document.createElement("span");
    tag.className = "model-tag" + (isDefaultModel ? " is-default" : "");

    const selectBtn = document.createElement("button");
    selectBtn.type = "button";
    selectBtn.className = "model-tag-select";
    selectBtn.title = "Use as default model";
    selectBtn.textContent = model;
    selectBtn.addEventListener("click", () => {
      settings.defaultProviderId = provider.id;
      settings.defaultModel = model;
      persist();
      renderAll();
    });
    tag.appendChild(selectBtn);

    const removeModelBtn = document.createElement("button");
    removeModelBtn.type = "button";
    removeModelBtn.className = "icon-btn";
    removeModelBtn.textContent = "✕";
    removeModelBtn.title = "Remove model";
    removeModelBtn.addEventListener("click", () => {
      provider.models = provider.models.filter((m) => m !== model);
      ensureValidDefaults();
      persist();
      renderAll();
    });
    tag.appendChild(removeModelBtn);
    tags.appendChild(tag);
  }
  modelsSection.appendChild(tags);

  const addModelRow = document.createElement("div");
  addModelRow.className = "add-model-row";
  const newModelInput = document.createElement("input");
  newModelInput.placeholder = "model name";
  const addModelBtn = document.createElement("button");
  addModelBtn.type = "button";
  addModelBtn.textContent = "Add model";
  addModelBtn.addEventListener("click", () => {
    const value = newModelInput.value.trim();
    if (!value || provider.models.includes(value)) return;
    provider.models.push(value);
    newModelInput.value = "";
    ensureValidDefaults();
    persist();
    renderAll();
  });
  addModelRow.append(newModelInput, addModelBtn);
  modelsSection.appendChild(addModelRow);

  block.append(header, fields, modelsSection);
  return block;
}

function renderLocalModelTag(model, { removable, isDefaultProvider, tags }) {
  const isDefaultModel = isDefaultProvider && model.id === settings.defaultModel;

  const tag = document.createElement("span");
  tag.className = "model-tag" + (isDefaultModel ? " is-default" : "");

  const selectBtn = document.createElement("button");
  selectBtn.type = "button";
  selectBtn.className = "model-tag-select";
  selectBtn.title = "Use as default model";
  selectBtn.textContent = model.name;
  selectBtn.addEventListener("click", () => {
    settings.defaultProviderId = LOCAL_PROVIDER_ID;
    settings.defaultModel = model.id;
    persist();
    renderAll();
  });
  tag.appendChild(selectBtn);

  if (removable) {
    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "icon-btn";
    removeBtn.textContent = "✕";
    removeBtn.title = "Remove model";
    removeBtn.addEventListener("click", () => {
      settings.localModels = settings.localModels.filter((m) => m.id !== model.id);
      ensureValidDefaults();
      persist();
      renderAll();
    });
    tag.appendChild(removeBtn);
  }

  tags.appendChild(tag);
}

function renderLocalProviderBlock() {
  const container = $("local-provider");
  if (!container) return;
  container.innerHTML = "";

  const block = document.createElement("div");
  block.className = "provider";

  const header = document.createElement("div");
  header.className = "provider-header";
  const name = document.createElement("span");
  name.className = "provider-name";
  name.textContent = LOCAL_PROVIDER_NAME;
  header.appendChild(name);

  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent =
    "Runs fully client-side via wllama/WebGPU — no API key, no server. " +
    "Models download on first use (roughly 150–650 MB) and are cached in " +
    "the browser for later runs.";

  const isDefaultProvider = settings.defaultProviderId === LOCAL_PROVIDER_ID;

  const modelsSection = document.createElement("div");
  modelsSection.className = "models";

  const modelsLabel = document.createElement("label");
  modelsLabel.textContent = "Models";
  modelsSection.appendChild(modelsLabel);

  const tags = document.createElement("div");
  tags.className = "model-tags";
  for (const model of LOCAL_MODEL_CATALOG) {
    renderLocalModelTag(model, { removable: false, isDefaultProvider, tags });
  }
  for (const model of settings.localModels ?? []) {
    renderLocalModelTag(model, { removable: true, isDefaultProvider, tags });
  }
  modelsSection.appendChild(tags);

  const addModelRow = document.createElement("div");
  addModelRow.className = "add-model-row";
  const newModelUrlInput = document.createElement("input");
  newModelUrlInput.placeholder = "https://…/model.gguf";
  const addModelBtn = document.createElement("button");
  addModelBtn.type = "button";
  addModelBtn.textContent = "Add model";
  addModelBtn.addEventListener("click", () => {
    const url = newModelUrlInput.value.trim();
    if (!url) return;
    const name = url.split("/").pop() || url;
    settings.localModels = settings.localModels ?? [];
    settings.localModels.push({ id: uid(), name, url });
    newModelUrlInput.value = "";
    ensureValidDefaults();
    persist();
    renderAll();
  });
  addModelRow.append(newModelUrlInput, addModelBtn);
  modelsSection.appendChild(addModelRow);

  block.append(header, hint, modelsSection);
  container.appendChild(block);
}

function renderProviderList() {
  const container = $("provider-list");
  container.innerHTML = "";
  if (settings.providers.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "No providers configured yet — add one below.";
    container.appendChild(empty);
    return;
  }
  for (const provider of settings.providers) {
    container.appendChild(renderProviderBlock(provider));
  }
}

function renderProviderSelect() {
  const select = $("provider-select");
  select.innerHTML = "";

  for (const p of settings.providers) {
    const opt = document.createElement("option");
    opt.value = p.id;
    opt.textContent = p.name;
    select.appendChild(opt);
  }

  // Built-in, always available — not one of the user-configured providers.
  const localOpt = document.createElement("option");
  localOpt.value = LOCAL_PROVIDER_ID;
  localOpt.textContent = LOCAL_PROVIDER_NAME;
  select.appendChild(localOpt);

  select.value = settings.defaultProviderId ?? "";
  select.disabled = false;
}

function renderModelSelect() {
  const select = $("model-select");
  select.innerHTML = "";
  const isLocal = settings.defaultProviderId === LOCAL_PROVIDER_ID;
  const models = isLocal
    ? getLocalModels().map((m) => ({ value: m.id, label: m.name }))
    : (findProvider(settings.defaultProviderId)?.models ?? []).map((m) => ({
        value: m,
        label: m,
      }));
  for (const { value, label } of models) {
    const opt = document.createElement("option");
    opt.value = value;
    opt.textContent = label;
    select.appendChild(opt);
  }
  select.value = settings.defaultModel ?? "";
  select.disabled = models.length === 0;
}

// The always-visible "model in use" pill in the prompt area — the selects
// themselves live in the collapsible context panel, so this is what tells
// the user which model a Run will hit when the panel is hidden.
function renderModelIndicator() {
  const indicator = $("model-indicator");
  if (!indicator) return;
  const isLocal = settings.defaultProviderId === LOCAL_PROVIDER_ID;
  const providerName = isLocal
    ? LOCAL_PROVIDER_NAME
    : findProvider(settings.defaultProviderId)?.name;
  const modelName = isLocal
    ? findLocalModel(settings.defaultModel)?.name
    : settings.defaultModel;
  indicator.textContent =
    providerName && modelName ? `${modelName} · ${providerName}` : "no model configured";
}

function renderAll() {
  renderProviderList();
  renderLocalProviderBlock();
  renderProviderSelect();
  renderModelSelect();
  renderModelIndicator();
}

export function initSettings() {
  settings = load();
  ensureValidDefaults();
  persist();

  renderAll();

  $("provider-select").addEventListener("change", (event) => {
    settings.defaultProviderId = event.target.value;
    ensureValidDefaults();
    persist();
    renderModelSelect();
    renderModelIndicator();
  });

  $("model-select").addEventListener("change", (event) => {
    settings.defaultModel = event.target.value;
    persist();
    renderModelIndicator();
  });

  $("add-provider-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const name = $("new-provider-name").value.trim();
    const baseUrl = $("new-provider-base-url").value.trim();
    const apiKey = $("new-provider-api-key").value;
    if (!name || !baseUrl) return;

    settings.providers.push({ id: uid(), name, baseUrl, apiKey, models: [] });
    event.target.reset();
    ensureValidDefaults();
    persist();
    renderAll();
  });

  $("reset-settings").addEventListener("click", () => {
    if (
      !confirm(
        "Reset all settings? This removes every provider and model you've configured.",
      )
    )
      return;
    settings = seedSettings();
    persist();
    renderAll();
  });
}
