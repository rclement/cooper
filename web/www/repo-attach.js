// "Attach a git repo" to a session: an opt-in chip next to the prompt box.
// Picking a repo (connecting the provider account inline first, if needed)
// shallow-clones its default branch into a per-attachment workspace folder
// ("<reponame>-<suffix>"); the run path then scopes the agent to it — the
// clone dir becomes the tools' working dir and the system prompt's
// current_working_dir, and the repo's AGENTS.md (if any) is fed to the
// core's agent_instructions slot. app.js records the attachment in session
// metadata so resuming a session re-attaches its repo.
//
// The repo list comes straight from the provider's API with the user's
// OAuth token (api.github.com is CORS-enabled), so no backend involvement.
import {
  GIT_PROVIDERS,
  getAccount,
  getGitAuth,
  isProviderAvailable,
  connectProvider,
  onAccountsChange,
} from "./git-accounts.js";
import { gitClone, setGitAuth, getDirHandle, toSegments, removePath, readFileText } from "./workspace-fs.js";

const $ = (id) => document.getElementById(id);

// { provider, fullName, branch, dir, status: "cloning"|"ready"|"missing",
//   usedInSession } — null when nothing is attached.
let attached = null;
let panelOpen = false;
let panelStatus = "";
let repoCache = {}; // provider -> [{ fullName, private, defaultBranch }]

// The repo binding is fixed for a session's lifetime: the system prompt
// (working dir, AGENTS.md) is built into the history on the first turn, so
// swapping repos mid-conversation would leave the model reasoning about the
// old repo while the tools act on the new one. app.js supplies this hook;
// it returns false to cancel, or ends the active session and returns true.
let bindingChange = () => true;

/// The current attachment in whatever state it's in ("cloning", "ready",
/// "missing"), or null. Callers gate on `status === "ready"` — app.js
/// refuses to run with a half-attached repo rather than silently starting
/// an unscoped session.
export function getRepoAttachment() {
  return attached;
}

/// Re-attaches a repo recorded in session metadata. Verifies the clone
/// still exists in the workspace; if not, the chip offers a re-clone.
export async function restoreAttachedRepo(repo) {
  panelOpen = false;
  // Displacing an attachment that never made it into a session orphans its
  // clone — remove it, same as a detach would.
  if (attached && !attached.usedInSession && attached.dir !== repo?.dir) {
    await removePath(attached.dir).catch(() => {});
  }
  if (!repo) {
    attached = null;
    render();
    return;
  }
  attached = { ...repo, status: "ready", usedInSession: true };
  try {
    await getDirHandle(toSegments(repo.dir), false);
  } catch {
    attached.status = "missing";
  }
  render();
}

/// Called by app.js once the attachment is recorded in a saved session —
/// from then on the clone dir belongs to that session (cleaned up when the
/// session is deleted), so detaching must not remove it.
export function markAttachedRepoUsed() {
  if (attached) attached.usedInSession = true;
}

async function listRepos(providerName) {
  if (!repoCache[providerName]) {
    const account = getAccount(providerName);
    repoCache[providerName] = await GIT_PROVIDERS[providerName].listRepos(account.accessToken);
  }
  return repoCache[providerName];
}

function cloneDirFor(fullName) {
  const repoName = fullName.split("/").pop();
  return `${repoName}-${crypto.randomUUID().slice(0, 8)}`;
}

/// `repo`: { url, fullName, branch?, provider? } — from the picker (own
/// repos, all fields known) or a pasted URL (any public repo — or private,
/// when a connected account's token matches the host; branch stays null and
/// the default branch is cloned).
async function attachRepo(repo) {
  if (!bindingChange()) return;
  attached = {
    ...repo,
    dir: cloneDirFor(repo.fullName),
    status: "cloning",
    usedInSession: false,
  };
  panelOpen = false;
  render();
  await cloneAttached();
}

/// "owner/repo" out of a repo URL, for the chip label and the clone folder.
function repoFullNameFromUrl(url) {
  const path = new URL(url).pathname.replace(/\/+$/, "").replace(/\.git$/, "");
  const segments = path.split("/").filter(Boolean);
  return segments.slice(-2).join("/") || "repo";
}

async function attachByUrl(rawUrl) {
  let fullName;
  try {
    fullName = repoFullNameFromUrl(rawUrl);
  } catch {
    panelStatus = `Not a valid repository URL: "${rawUrl}"`;
    render();
    return;
  }
  await attachRepo({ url: rawUrl, fullName, branch: null, provider: null });
}

async function cloneAttached() {
  const repo = attached;
  repo.status = "cloning";
  repo.progress = "";
  render();
  try {
    setGitAuth(getGitAuth()); // main-thread clone needs the credentials too
    await gitClone({
      url: repo.url,
      destPath: repo.dir,
      shallow: true,
      onProgress: ({ phase, loaded, total }) => {
        repo.progress = total ? `${phase} ${Math.round((100 * loaded) / total)}%` : phase;
        renderChipOnly();
      },
    });
    repo.status = "ready";
    panelStatus = "";
  } catch (err) {
    attached = null;
    panelStatus = `Clone failed: ${err.message || err}`;
    panelOpen = true;
  }
  render();
}

/// The repo's AGENTS.md content, or null — read fresh at run time so edits
/// (by the user or the agent itself) are picked up on the next turn.
export async function readAttachedAgentsMd() {
  if (attached?.status !== "ready") return null;
  try {
    return await readFileText(`${attached.dir}/AGENTS.md`);
  } catch {
    return null;
  }
}

async function detach() {
  if (!bindingChange()) return;
  const repo = attached;
  attached = null;
  panelStatus = "";
  // An attachment no session ever used is ours to clean up; once recorded
  // in session metadata, the clone belongs to the session (deleted with it).
  if (repo && !repo.usedInSession) {
    await removePath(repo.dir).catch(() => {});
  }
  render();
}

/// For app.js's "New session" button: an attachment the old session used
/// stays with that session (the chip clears); one attached in anticipation
/// — never run with — carries into the session about to start.
export function clearUsedAttachment() {
  if (attached?.usedInSession) attached = null;
  render();
}

// ---------- rendering ----------

function renderChipOnly() {
  const chip = document.querySelector("#repo-attach .repo-chip");
  if (chip && attached?.status === "cloning") {
    chip.querySelector(".repo-chip-label").textContent =
      `${attached.fullName} — cloning… ${attached.progress ?? ""}`;
  }
}

function renderChip(container) {
  const chip = document.createElement("span");
  chip.className = "repo-chip" + (attached.status === "missing" ? " is-missing" : "");

  const label = document.createElement("span");
  label.className = "repo-chip-label";
  label.textContent =
    attached.status === "cloning"
      ? `${attached.fullName} — cloning…`
      : attached.status === "missing"
        ? `${attached.fullName} — clone missing`
        : attached.branch
          ? `${attached.fullName} (${attached.branch})`
          : attached.fullName;
  chip.appendChild(label);

  if (attached.status === "missing") {
    const recloneBtn = document.createElement("button");
    recloneBtn.type = "button";
    recloneBtn.textContent = "Re-clone";
    recloneBtn.addEventListener("click", cloneAttached);
    chip.appendChild(recloneBtn);
  }

  if (attached.status !== "cloning") {
    const detachBtn = document.createElement("button");
    detachBtn.type = "button";
    detachBtn.className = "icon-btn";
    detachBtn.textContent = "✕";
    detachBtn.title = "Detach repo";
    detachBtn.addEventListener("click", detach);
    chip.appendChild(detachBtn);
  }

  container.appendChild(chip);
}

async function renderPanel(container) {
  const panel = document.createElement("div");
  panel.className = "repo-panel";
  container.appendChild(panel);

  if (panelStatus) {
    const status = document.createElement("p");
    status.className = "hint";
    status.textContent = panelStatus;
    panel.appendChild(status);
  }

  // Any repo by URL — public ones need no account at all (and a pasted
  // private URL still works when a connected account's token matches the
  // host, since the clone goes through the same onAuth lookup).
  const urlRow = document.createElement("div");
  urlRow.className = "repo-url-row";
  const urlInput = document.createElement("input");
  urlInput.placeholder = "https://github.com/owner/repo.git";
  const urlBtn = document.createElement("button");
  urlBtn.type = "button";
  urlBtn.textContent = "Attach URL";
  const submitUrl = () => {
    if (urlInput.value.trim()) attachByUrl(urlInput.value.trim());
  };
  urlBtn.addEventListener("click", submitUrl);
  urlInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submitUrl();
  });
  urlRow.append(urlInput, urlBtn);
  panel.appendChild(urlRow);

  const providerName = "github";
  const provider = GIT_PROVIDERS[providerName];

  if (!getAccount(providerName)) {
    const hint = document.createElement("p");
    hint.className = "hint";

    if (await isProviderAvailable(providerName)) {
      hint.textContent = `Or connect your ${provider.label} account to pick from your repositories (private ones included).`;
      const connectBtn = document.createElement("button");
      connectBtn.type = "button";
      connectBtn.textContent = `Connect ${provider.label}`;
      connectBtn.addEventListener("click", () => {
        connectProvider(providerName).catch((err) => {
          panelStatus = String(err.message || err);
          render();
        });
      });
      panel.append(hint, connectBtn);
    } else {
      hint.textContent =
        `Browsing your own ${provider.label} repositories needs a connected account, ` +
        "and this server has no OAuth credentials configured (see README: " +
        "GITHUB_CLIENT_ID / GITHUB_CLIENT_SECRET). Public URLs still work above.";
      panel.appendChild(hint);
    }
    return;
  }

  const filter = document.createElement("input");
  filter.placeholder = "Filter repositories…";
  filter.className = "repo-filter";
  const list = document.createElement("div");
  list.className = "repo-list";
  panel.append(filter, list);

  let repos;
  try {
    list.textContent = "Loading repositories…";
    repos = await listRepos(providerName);
  } catch (err) {
    list.textContent = `Could not list repositories: ${err.message || err}`;
    return;
  }

  const show = () => {
    const needle = filter.value.trim().toLowerCase();
    list.innerHTML = "";
    const matches = repos.filter((r) => r.fullName.toLowerCase().includes(needle));
    if (matches.length === 0) {
      list.textContent = "No matching repositories.";
      return;
    }
    const CAP = 50;
    for (const repo of matches.slice(0, CAP)) {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "repo-item";
      item.textContent = repo.fullName + (repo.private ? " 🔒" : "");
      item.addEventListener("click", () =>
        attachRepo({
          url: `https://${provider.host}/${repo.fullName}.git`,
          fullName: repo.fullName,
          branch: repo.defaultBranch,
          provider: providerName,
        }),
      );
      list.appendChild(item);
    }
    if (matches.length > CAP) {
      const more = document.createElement("p");
      more.className = "hint";
      more.textContent = `…and ${matches.length - CAP} more — type to narrow the list.`;
      list.appendChild(more);
    }
  };
  filter.addEventListener("input", show);
  show();
  filter.focus();
}

function render() {
  const container = $("repo-attach");
  if (!container) return;
  container.innerHTML = "";

  const row = document.createElement("div");
  row.className = "repo-attach-row";
  container.appendChild(row);

  if (attached) {
    renderChip(row);
  } else {
    const attachBtn = document.createElement("button");
    attachBtn.type = "button";
    attachBtn.className = "secondary repo-attach-btn";
    attachBtn.textContent = panelOpen ? "Cancel" : "Attach Git repository";
    attachBtn.addEventListener("click", () => {
      panelOpen = !panelOpen;
      panelStatus = "";
      render();
    });
    row.appendChild(attachBtn);
  }

  if (panelOpen && !attached) {
    renderPanel(container); // async — fills in as data arrives
  }
}

export function initRepoAttach({ onBindingChange } = {}) {
  bindingChange = onBindingChange ?? (() => true);
  // A connect finishing while the picker is open moves it straight to the
  // repo list; a disconnect invalidates the cached list's token.
  onAccountsChange(() => {
    repoCache = {};
    render();
  });
  render();
}
