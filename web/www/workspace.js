// Workspace view: a file-manager UI over OPFS (the same virtual filesystem
// the agent's list_files/read_file/write_file/edit_file/git_clone tools
// operate on — see workspace-tools.js). Ported from the `opfs/` playground
// at the repo root, adapted to this app's DOM (ids prefixed `ws-` to avoid
// clashing with the rest of the SPA) and to share its OPFS/git plumbing with
// the agent tools via workspace-fs.js instead of duplicating it.
import {
  getRoot,
  getDirHandle,
  listEntries,
  calcDirSize,
  copyRecursive,
  isSubPath,
  humanSize,
  extOf,
  isImageName,
  isAudioName,
  isVideoName,
  isPdfName,
  isPreviewOnlyName,
  validEntryName,
  pathKey,
  gitClone,
  GIT_CORS_PROXY,
} from "./workspace-fs.js";

const $ = (id) => document.getElementById(id);

let currentPath = []; // array of directory names, relative to root
let expandedPaths = new Set([""]); // path keys joined with "/" that are expanded in the tree

// ---------- Toast ----------

function showToast(message, type) {
  const container = $("ws-toast-container");
  const el = document.createElement("div");
  el.className = "ws-toast" + (type ? ` ${type}` : "");
  el.textContent = message;
  container.appendChild(el);
  setTimeout(() => el.remove(), 3500);
}

async function withErrorToast(fn) {
  try {
    return await fn();
  } catch (err) {
    console.error(err);
    showToast(err && err.message ? err.message : String(err), "error");
    return undefined;
  }
}

// ---------- Modal ----------

let currentModalObjectUrl = null;

function openModal(title, contentEl, objectUrl) {
  $("ws-modal-title").textContent = title;
  const content = $("ws-modal-content");
  content.innerHTML = "";
  content.appendChild(contentEl);
  $("ws-modal-overlay").hidden = false;
  currentModalObjectUrl = objectUrl || null;
}

function closeModal() {
  $("ws-modal-overlay").hidden = true;
  $("ws-modal-content").innerHTML = "";
  if (currentModalObjectUrl) {
    URL.revokeObjectURL(currentModalObjectUrl);
    currentModalObjectUrl = null;
  }
}

// ---------- Rendering: tree ----------

async function renderTree() {
  const rootUl = $("ws-tree-root");
  rootUl.innerHTML = "";
  const rootLi = await buildTreeNode("Workspace", [], true);
  rootUl.appendChild(rootLi);
}

async function buildTreeNode(label, pathArray, isRoot) {
  const key = pathKey(pathArray);
  const li = document.createElement("li");

  const nodeDiv = document.createElement("div");
  nodeDiv.className = "ws-tree-node";
  if (pathKey(currentPath) === key) nodeDiv.classList.add("is-active");

  const toggle = document.createElement("span");
  toggle.className = "ws-tree-toggle";
  toggle.textContent = expandedPaths.has(key) ? "▾" : "▸";

  const nameSpan = document.createElement("span");
  nameSpan.textContent = (isRoot ? "📁 " : "📁 ") + label;

  nodeDiv.append(toggle, nameSpan);
  li.appendChild(nodeDiv);

  const childUl = document.createElement("ul");
  li.appendChild(childUl);

  nodeDiv.addEventListener("click", async () => {
    currentPath = pathArray.slice();
    if (expandedPaths.has(key)) expandedPaths.delete(key);
    else expandedPaths.add(key);
    await renderAll();
  });

  if (expandedPaths.has(key)) {
    await withErrorToast(async () => {
      const dirHandle = await getDirHandle(pathArray, false);
      const entries = await listEntries(dirHandle);
      for (const entry of entries) {
        if (entry.kind === "directory") {
          const childPath = pathArray.concat(entry.name);
          const childLi = await buildTreeNode(entry.name, childPath, false);
          childUl.appendChild(childLi);
        }
      }
    });
  }

  return li;
}

// ---------- Rendering: breadcrumb ----------

function renderBreadcrumb() {
  const el = $("ws-breadcrumb");
  el.innerHTML = "";

  const rootCrumb = document.createElement("span");
  rootCrumb.className = "ws-crumb";
  rootCrumb.textContent = "Workspace";
  rootCrumb.addEventListener("click", async () => {
    currentPath = [];
    await renderAll();
  });
  el.appendChild(rootCrumb);

  let accum = [];
  for (const part of currentPath) {
    accum = accum.concat(part);
    const sep = document.createElement("span");
    sep.className = "ws-sep";
    sep.textContent = "/";
    el.appendChild(sep);

    const crumb = document.createElement("span");
    crumb.className = "ws-crumb";
    crumb.textContent = part;
    const targetPath = accum.slice();
    crumb.addEventListener("click", async () => {
      currentPath = targetPath;
      await renderAll();
    });
    el.appendChild(crumb);
  }
}

// ---------- Rendering: entries table ----------

async function renderEntries() {
  const tbody = $("ws-entries-body");
  const emptyState = $("ws-empty-state");
  tbody.innerHTML = "";

  const dirHandle = await getDirHandle(currentPath, false);
  const entries = await listEntries(dirHandle);

  emptyState.hidden = entries.length > 0;

  for (const entry of entries) {
    const tr = document.createElement("tr");

    const nameTd = document.createElement("td");
    const nameSpan = document.createElement("span");
    nameSpan.className = "ws-entry-name" + (entry.kind === "directory" ? " is-dir" : "");
    nameSpan.textContent = (entry.kind === "directory" ? "📁 " : "📄 ") + entry.name;
    nameTd.appendChild(nameSpan);
    tr.appendChild(nameTd);

    const typeTd = document.createElement("td");
    typeTd.textContent = entry.kind === "directory" ? "Folder" : (extOf(entry.name) || "file");
    tr.appendChild(typeTd);

    const sizeTd = document.createElement("td");
    const modifiedTd = document.createElement("td");
    if (entry.kind === "file") {
      const file = await entry.handle.getFile();
      sizeTd.textContent = humanSize(file.size);
      modifiedTd.textContent = new Date(file.lastModified).toLocaleString();
    } else {
      sizeTd.textContent = "—";
      modifiedTd.textContent = "—";
    }
    tr.append(sizeTd, modifiedTd);

    const actionsTd = document.createElement("td");
    actionsTd.className = "ws-row-actions";

    if (entry.kind === "directory") {
      const openBtn = document.createElement("button");
      openBtn.type = "button";
      openBtn.textContent = "Open";
      openBtn.addEventListener("click", async () => {
        currentPath = currentPath.concat(entry.name);
        expandedPaths.add(pathKey(currentPath));
        await renderAll();
      });
      actionsTd.appendChild(openBtn);
    } else {
      const editBtn = document.createElement("button");
      editBtn.type = "button";
      editBtn.textContent = isPreviewOnlyName(entry.name) ? "View" : "Edit";
      editBtn.addEventListener("click", () => openFileEditor(entry.name, entry.handle));
      actionsTd.appendChild(editBtn);

      const downloadBtn = document.createElement("button");
      downloadBtn.type = "button";
      downloadBtn.textContent = "Download";
      downloadBtn.addEventListener("click", () => downloadFile(entry.name, entry.handle));
      actionsTd.appendChild(downloadBtn);
    }

    const renameBtn = document.createElement("button");
    renameBtn.type = "button";
    renameBtn.textContent = "Rename";
    renameBtn.addEventListener("click", () => renameEntry(entry));
    actionsTd.appendChild(renameBtn);

    const moveBtn = document.createElement("button");
    moveBtn.type = "button";
    moveBtn.textContent = "Move";
    moveBtn.addEventListener("click", () => openMoveModal(entry));
    actionsTd.appendChild(moveBtn);

    const deleteBtn = document.createElement("button");
    deleteBtn.type = "button";
    deleteBtn.textContent = "Delete";
    deleteBtn.className = "ws-danger-text";
    deleteBtn.addEventListener("click", () => deleteEntry(entry));
    actionsTd.appendChild(deleteBtn);

    tr.appendChild(actionsTd);
    tbody.appendChild(tr);

    if (entry.kind !== "directory") {
      nameSpan.addEventListener("click", () => openFileEditor(entry.name, entry.handle));
    } else {
      nameSpan.addEventListener("click", async () => {
        currentPath = currentPath.concat(entry.name);
        expandedPaths.add(pathKey(currentPath));
        await renderAll();
      });
    }
  }
}

// ---------- Rendering: storage panel ----------

async function renderStorage() {
  const fillEl = $("ws-storage-bar-fill");
  const textEl = $("ws-storage-text");
  const wsEl = $("ws-workspace-size-text");

  await withErrorToast(async () => {
    if (navigator.storage && navigator.storage.estimate) {
      const { usage, quota } = await navigator.storage.estimate();
      const pct = quota ? Math.min(100, (usage / quota) * 100) : 0;
      fillEl.style.width = `${pct.toFixed(1)}%`;
      textEl.textContent = `${humanSize(usage)} / ${humanSize(quota)} (${pct.toFixed(1)}%)`;
    } else {
      textEl.textContent = "Storage estimate API not available";
    }
  });

  await withErrorToast(async () => {
    const total = await calcDirSize(await getRoot());
    wsEl.textContent = `Workspace files: ${humanSize(total)}`;
  });
}

// ---------- Full re-render ----------

async function renderAll() {
  await withErrorToast(renderTree);
  renderBreadcrumb();
  await withErrorToast(renderEntries);
  await renderStorage();
}

// ---------- Actions ----------

async function createNewFile() {
  const name = prompt("New file name (with extension):");
  if (name === null) return;
  if (!validEntryName(name)) {
    showToast("Invalid file name.", "error");
    return;
  }
  await withErrorToast(async () => {
    const dirHandle = await getDirHandle(currentPath, false);
    const fileHandle = await dirHandle.getFileHandle(name, { create: true });
    const existing = await fileHandle.getFile();
    if (existing.size === 0) {
      const writable = await fileHandle.createWritable();
      await writable.write("");
      await writable.close();
    }
    showToast(`Created "${name}".`, "success");
    await renderAll();
  });
}

async function createNewFolder() {
  const name = prompt("New folder name:");
  if (name === null) return;
  if (!validEntryName(name)) {
    showToast("Invalid folder name.", "error");
    return;
  }
  await withErrorToast(async () => {
    const dirHandle = await getDirHandle(currentPath, false);
    await dirHandle.getDirectoryHandle(name, { create: true });
    showToast(`Created folder "${name}".`, "success");
    await renderAll();
  });
}

async function uploadFiles(fileList) {
  await withErrorToast(async () => {
    const dirHandle = await getDirHandle(currentPath, false);
    for (const file of fileList) {
      const fileHandle = await dirHandle.getFileHandle(file.name, { create: true });
      const writable = await fileHandle.createWritable();
      await writable.write(await file.arrayBuffer());
      await writable.close();
    }
    showToast(`Uploaded ${fileList.length} file(s).`, "success");
    await renderAll();
  });
}

async function fetchFromUrl() {
  const wrapper = document.createElement("div");
  wrapper.innerHTML = `
    <label class="ws-modal-label">URL</label>
    <input id="ws-fetch-url-input" type="text" placeholder="https://example.com/file.txt" class="ws-modal-input">
    <label class="ws-modal-label">Save as (optional, defaults to URL file name)</label>
    <input id="ws-fetch-name-input" type="text" placeholder="my-file.txt" class="ws-modal-input">
    <div class="ws-modal-actions">
      <button id="ws-fetch-cancel" type="button" class="secondary">Cancel</button>
      <button id="ws-fetch-confirm" type="button">Fetch &amp; Save</button>
    </div>
  `;
  openModal("Fetch file from URL", wrapper);

  wrapper.querySelector("#ws-fetch-cancel").addEventListener("click", closeModal);
  wrapper.querySelector("#ws-fetch-confirm").addEventListener("click", async () => {
    const url = wrapper.querySelector("#ws-fetch-url-input").value.trim();
    let name = wrapper.querySelector("#ws-fetch-name-input").value.trim();
    if (!url) {
      showToast("Please enter a URL.", "error");
      return;
    }
    if (!name) {
      try {
        const u = new URL(url);
        name = decodeURIComponent(u.pathname.split("/").filter(Boolean).pop() || "downloaded-file");
      } catch {
        name = "downloaded-file";
      }
    }
    if (!validEntryName(name)) {
      showToast("Invalid resulting file name.", "error");
      return;
    }
    await withErrorToast(async () => {
      const response = await fetch(url);
      if (!response.ok) throw new Error(`Fetch failed: HTTP ${response.status}`);
      const blob = await response.blob();
      const dirHandle = await getDirHandle(currentPath, false);
      const fileHandle = await dirHandle.getFileHandle(name, { create: true });
      const writable = await fileHandle.createWritable();
      await writable.write(blob);
      await writable.close();
      showToast(`Saved "${name}" from URL.`, "success");
      closeModal();
      await renderAll();
    });
  });
}

async function cloneRepo() {
  const wrapper = document.createElement("div");
  wrapper.innerHTML = `
    <p class="hint ws-modal-hint">
      Public repositories only. Runs entirely in the browser via
      <a href="https://isomorphic-git.org/" target="_blank" rel="noopener">isomorphic-git</a>,
      routed through its official CORS proxy (${GIT_CORS_PROXY}) since GitHub/GitLab
      don't send CORS headers for git's smart HTTP protocol.
    </p>
    <label class="ws-modal-label">Repository URL</label>
    <input id="ws-clone-url-input" type="text" placeholder="https://github.com/owner/repo.git" class="ws-modal-input">
    <label class="ws-modal-label">Folder name (optional)</label>
    <input id="ws-clone-folder-input" type="text" placeholder="repo" class="ws-modal-input">
    <label class="ws-modal-label">Branch (optional, default branch used if empty)</label>
    <input id="ws-clone-branch-input" type="text" placeholder="main" class="ws-modal-input">
    <label class="ws-modal-checkbox-label">
      <input id="ws-clone-shallow-input" type="checkbox" checked> Shallow clone (depth=1, faster, no history)
    </label>
    <div id="ws-clone-status" class="ws-modal-status"></div>
    <div class="ws-modal-actions">
      <button id="ws-clone-cancel" type="button" class="secondary">Cancel</button>
      <button id="ws-clone-confirm" type="button">Clone</button>
    </div>
  `;
  openModal("Clone Git repository", wrapper);

  const statusEl = wrapper.querySelector("#ws-clone-status");
  const cancelBtn = wrapper.querySelector("#ws-clone-cancel");
  const confirmBtn = wrapper.querySelector("#ws-clone-confirm");

  cancelBtn.addEventListener("click", closeModal);
  confirmBtn.addEventListener("click", async () => {
    const url = wrapper.querySelector("#ws-clone-url-input").value.trim();
    let folder = wrapper.querySelector("#ws-clone-folder-input").value.trim();
    const branch = wrapper.querySelector("#ws-clone-branch-input").value.trim() || undefined;
    const shallow = wrapper.querySelector("#ws-clone-shallow-input").checked;

    if (!url) {
      showToast("Please enter a repository URL.", "error");
      return;
    }
    if (!folder) {
      folder = url.replace(/\/+$/, "").split("/").pop().replace(/\.git$/, "") || "repo";
    }
    if (!validEntryName(folder)) {
      showToast("Invalid folder name.", "error");
      return;
    }

    confirmBtn.disabled = true;
    cancelBtn.disabled = true;
    statusEl.textContent = "Starting clone…";

    await withErrorToast(async () => {
      const destPath = currentPath.concat(folder).join("/");
      await gitClone({
        url,
        destPath,
        branch,
        shallow,
        onProgress: (event) => {
          const suffix = event.total ? ` (${event.loaded}/${event.total})` : event.loaded ? ` (${event.loaded})` : "";
          statusEl.textContent = `${event.phase}${suffix}`;
        },
      });

      showToast(`Cloned into "${folder}".`, "success");
      closeModal();
      expandedPaths.add(pathKey(currentPath.concat(folder)));
      await renderAll();
    });

    confirmBtn.disabled = false;
    cancelBtn.disabled = false;
  });
}

function downloadFile(name, fileHandle) {
  withErrorToast(async () => {
    const file = await fileHandle.getFile();
    const url = URL.createObjectURL(file);
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.click();
    URL.revokeObjectURL(url);
  });
}

async function openFileEditor(name, fileHandle) {
  await withErrorToast(async () => {
    const file = await fileHandle.getFile();
    const wrapper = document.createElement("div");

    if (isPreviewOnlyName(name)) {
      const url = URL.createObjectURL(file);

      if (isImageName(name)) {
        wrapper.innerHTML = `<img class="ws-preview" src="${url}" alt="${name}">`;
      } else if (isAudioName(name)) {
        wrapper.innerHTML = `<audio class="ws-preview" src="${url}" controls autoplay></audio>`;
      } else if (isVideoName(name)) {
        wrapper.innerHTML = `<video class="ws-preview" src="${url}" controls autoplay></video>`;
      } else if (isPdfName(name)) {
        wrapper.innerHTML = `<iframe class="ws-preview ws-pdf-preview" src="${url}" title="${name}"></iframe>`;
      }

      const actions = document.createElement("div");
      actions.className = "ws-modal-actions";
      const closeBtn = document.createElement("button");
      closeBtn.type = "button";
      closeBtn.className = "secondary";
      closeBtn.textContent = "Close";
      closeBtn.addEventListener("click", closeModal);
      actions.appendChild(closeBtn);
      wrapper.appendChild(actions);
      openModal(name, wrapper, url);
      return;
    }

    const text = await file.text();
    const textarea = document.createElement("textarea");
    textarea.className = "ws-editor-textarea";
    textarea.value = text;
    wrapper.appendChild(textarea);

    const actions = document.createElement("div");
    actions.className = "ws-modal-actions";
    const cancelBtn = document.createElement("button");
    cancelBtn.type = "button";
    cancelBtn.className = "secondary";
    cancelBtn.textContent = "Cancel";
    cancelBtn.addEventListener("click", closeModal);
    const saveBtn = document.createElement("button");
    saveBtn.type = "button";
    saveBtn.textContent = "Save";
    saveBtn.addEventListener("click", async () => {
      await withErrorToast(async () => {
        const writable = await fileHandle.createWritable();
        await writable.write(textarea.value);
        await writable.close();
        showToast(`Saved "${name}".`, "success");
        closeModal();
        await renderAll();
      });
    });
    actions.append(cancelBtn, saveBtn);
    wrapper.appendChild(actions);

    openModal(`Edit: ${name}`, wrapper);
  });
}

async function renameEntry(entry) {
  const newName = prompt(`Rename "${entry.name}" to:`, entry.name);
  if (newName === null || newName === entry.name) return;
  if (!validEntryName(newName)) {
    showToast("Invalid name.", "error");
    return;
  }
  await withErrorToast(async () => {
    const dirHandle = await getDirHandle(currentPath, false);
    const existingNames = (await listEntries(dirHandle)).map((e) => e.name);
    if (existingNames.includes(newName)) {
      throw new Error(`An entry named "${newName}" already exists here.`);
    }
    await copyRecursive(entry.handle, dirHandle, newName);
    await dirHandle.removeEntry(entry.name, { recursive: true });
    showToast(`Renamed to "${newName}".`, "success");
    await renderAll();
  });
}

async function deleteEntry(entry) {
  const kindLabel = entry.kind === "directory" ? "folder (and everything inside it)" : "file";
  if (!confirm(`Delete ${kindLabel} "${entry.name}"? This cannot be undone.`)) return;
  await withErrorToast(async () => {
    const dirHandle = await getDirHandle(currentPath, false);
    await dirHandle.removeEntry(entry.name, { recursive: true });
    showToast(`Deleted "${entry.name}".`, "success");
    await renderAll();
  });
}

// ---------- Move modal ----------

async function openMoveModal(entry) {
  const sourcePath = currentPath;
  const wrapper = document.createElement("div");
  wrapper.innerHTML = `<p class="ws-modal-hint">Choose a destination folder for "${entry.name}":</p>`;

  const treeContainer = document.createElement("div");
  treeContainer.className = "ws-modal-tree";
  wrapper.appendChild(treeContainer);

  let selectedPath = [];

  async function buildPickerNode(label, pathArray, isRoot) {
    const li = document.createElement("li");
    const nodeDiv = document.createElement("div");
    nodeDiv.className = "ws-tree-node";
    nodeDiv.textContent = (isRoot ? "📁 " : "📁 ") + label;

    const disallowed =
      entry.kind === "directory" && isSubPath(sourcePath.concat(entry.name), pathArray);

    if (disallowed) {
      nodeDiv.style.opacity = "0.4";
      nodeDiv.style.cursor = "not-allowed";
    } else {
      nodeDiv.addEventListener("click", () => {
        selectedPath = pathArray;
        treeContainer.querySelectorAll(".ws-tree-node").forEach((n) => n.classList.remove("is-active"));
        nodeDiv.classList.add("is-active");
      });
    }

    li.appendChild(nodeDiv);
    const childUl = document.createElement("ul");
    li.appendChild(childUl);

    if (!disallowed) {
      const dirHandle = await getDirHandle(pathArray, false);
      const entries = await listEntries(dirHandle);
      for (const child of entries) {
        if (child.kind === "directory") {
          const childLi = await buildPickerNode(child.name, pathArray.concat(child.name), false);
          childUl.appendChild(childLi);
        }
      }
    }

    return li;
  }

  const rootUl = document.createElement("ul");
  rootUl.className = "ws-tree";
  rootUl.appendChild(await buildPickerNode("Workspace", [], true));
  treeContainer.appendChild(rootUl);

  const actions = document.createElement("div");
  actions.className = "ws-modal-actions";
  const cancelBtn = document.createElement("button");
  cancelBtn.type = "button";
  cancelBtn.className = "secondary";
  cancelBtn.textContent = "Cancel";
  cancelBtn.addEventListener("click", closeModal);
  const moveBtn = document.createElement("button");
  moveBtn.type = "button";
  moveBtn.textContent = "Move here";
  moveBtn.addEventListener("click", async () => {
    await withErrorToast(async () => {
      if (pathKey(selectedPath) === pathKey(sourcePath)) {
        throw new Error("Source and destination are the same folder.");
      }
      const destDirHandle = await getDirHandle(selectedPath, false);
      const destEntries = (await listEntries(destDirHandle)).map((e) => e.name);
      if (destEntries.includes(entry.name)) {
        throw new Error(`An entry named "${entry.name}" already exists in the destination.`);
      }
      await copyRecursive(entry.handle, destDirHandle, entry.name);
      const srcDirHandle = await getDirHandle(sourcePath, false);
      await srcDirHandle.removeEntry(entry.name, { recursive: true });
      showToast(`Moved "${entry.name}".`, "success");
      closeModal();
      await renderAll();
    });
  });
  actions.append(cancelBtn, moveBtn);
  wrapper.appendChild(actions);

  openModal(`Move "${entry.name}"`, wrapper);
}

// ---------- Wiring ----------

function wireToolbar() {
  $("ws-btn-new-file").addEventListener("click", createNewFile);
  $("ws-btn-new-folder").addEventListener("click", createNewFolder);
  $("ws-btn-refresh").addEventListener("click", () => renderAll());
  $("ws-btn-fetch-url").addEventListener("click", fetchFromUrl);
  $("ws-btn-clone-repo").addEventListener("click", cloneRepo);

  const fileInput = $("ws-file-input");
  $("ws-btn-upload").addEventListener("click", () => fileInput.click());
  fileInput.addEventListener("change", async (e) => {
    if (e.target.files && e.target.files.length > 0) {
      await uploadFiles(e.target.files);
    }
    fileInput.value = "";
  });

  $("ws-modal-close").addEventListener("click", closeModal);
  $("ws-modal-overlay").addEventListener("click", (e) => {
    if (e.target.id === "ws-modal-overlay") closeModal();
  });
}

let supported = false;

export async function initWorkspace() {
  if (!("storage" in navigator) || !navigator.storage.getDirectory) {
    const view = $("view-workspace");
    view.innerHTML =
      '<p class="hint">This browser does not support the Origin Private File System (OPFS), which the Workspace feature needs. Try a recent Chrome, Edge, or Firefox.</p>';
    return;
  }

  supported = true;
  wireToolbar();
  await renderAll();
}

// Re-reads the workspace from OPFS. Called whenever the Workspace nav item
// is (re-)selected — files can have changed since the view was last shown,
// either from an agent run's tool calls or the "New session" flow spinning
// up a Worker with its own OPFS handle, so a stale in-memory listing would
// otherwise linger until a manual "Refresh" click.
export async function refreshWorkspace() {
  if (!supported) return;
  await renderAll();
}
