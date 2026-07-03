// Provider/model settings: persisted to localStorage, with a small CRUD
// panel rendered directly into the DOM (no framework, no build step).
const STORAGE_KEY = "cooper.settings.v1";

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
  };
}

function load() {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw) {
    try {
      return JSON.parse(raw);
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

/// Keeps `defaultProviderId`/`defaultModel` pointing at something that still
/// exists, falling back to the first provider/model after any removal.
function ensureValidDefaults() {
  let provider = findProvider(settings.defaultProviderId);
  if (!provider) {
    provider = settings.providers[0];
    settings.defaultProviderId = provider?.id ?? null;
  }
  if (!provider || !provider.models.includes(settings.defaultModel)) {
    settings.defaultModel = provider?.models[0] ?? null;
  }
}

export function getCurrentConfig() {
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
  select.value = settings.defaultProviderId ?? "";
  select.disabled = settings.providers.length === 0;
}

function renderModelSelect() {
  const select = $("model-select");
  select.innerHTML = "";
  const provider = findProvider(settings.defaultProviderId);
  const models = provider?.models ?? [];
  for (const m of models) {
    const opt = document.createElement("option");
    opt.value = m;
    opt.textContent = m;
    select.appendChild(opt);
  }
  select.value = settings.defaultModel ?? "";
  select.disabled = models.length === 0;
}

function renderAll() {
  renderProviderList();
  renderProviderSelect();
  renderModelSelect();
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
  });

  $("model-select").addEventListener("change", (event) => {
    settings.defaultModel = event.target.value;
    persist();
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
