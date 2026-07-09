// Git provider accounts (OAuth): lets the user connect a GitHub (later
// GitLab, …) account so `git_clone` can reach private repositories.
//
// Split of responsibilities: this module runs the browser-side of the OAuth
// dance — authorize popup, CSRF state check, token storage, the "Connected
// accounts" settings section — while the `cooper web` server contributes only
// the step that needs the client secret (code-for-token exchange, see
// `/oauth/*` in src/web.rs). Which providers are connectable is discovered
// from `GET /oauth/providers`; when the app is served statically without the
// cooper server, the section explains what's missing instead.
//
// Access tokens live client-side only (localStorage, one account per
// provider) — the server never stores them. The popup can't reach us through
// `window.opener` (COOP: same-origin severs it on the cross-origin hop to
// the provider), so oauth-callback.html reports back over a BroadcastChannel.

const STORAGE_KEY = "cooper.gitAccounts.v1";
const CHANNEL_NAME = "cooper-oauth";

export const GIT_PROVIDERS = {
  github: {
    label: "GitHub",
    host: "github.com",
    authorizeUrl: "https://github.com/login/oauth/authorize",
    // Full private-repo read access for OAuth Apps. If the registered app is
    // a GitHub App instead, the scope is ignored and access is whatever
    // repositories the user selected at install time.
    scope: "repo",
    async fetchIdentity(token) {
      const response = await fetch("https://api.github.com/user", {
        headers: {
          Authorization: `Bearer ${token}`,
          Accept: "application/vnd.github+json",
        },
      });
      if (!response.ok) throw new Error(`GitHub /user failed (${response.status})`);
      const user = await response.json();
      return { login: user.login, avatarUrl: user.avatar_url };
    },
    // Basic-auth shape isomorphic-git sends; this username works for both
    // OAuth App and GitHub App user tokens.
    auth(token) {
      return { username: "x-access-token", password: token };
    },
    async listRepos(token) {
      // Paginated: a page returning fewer than `per_page` items is the last
      // one. Capped at 10 pages (1000 repos) as a runaway guard — the picker
      // has a filter, exhaustiveness matters more than completeness there.
      const all = [];
      for (let page = 1; page <= 10; page++) {
        const response = await fetch(
          `https://api.github.com/user/repos?per_page=100&sort=pushed&page=${page}`,
          {
            headers: {
              Authorization: `Bearer ${token}`,
              Accept: "application/vnd.github+json",
            },
          },
        );
        if (!response.ok) throw new Error(`listing repos failed (${response.status})`);
        const repos = await response.json();
        all.push(...repos);
        if (repos.length < 100) break;
      }
      return all.map((r) => ({
        fullName: r.full_name,
        private: r.private,
        defaultBranch: r.default_branch,
      }));
    },
  },
};

const $ = (id) => document.getElementById(id);

let accounts = {}; // provider name -> { accessToken, login, avatarUrl, ... }
let serverProviders = null; // /oauth/providers payload, null when unavailable
let providersProbe = null; // resolves once /oauth/providers has been probed
let pendingState = null; // CSRF state of the in-flight authorize popup
let statusMessage = "";

// Other UI (e.g. the repo attachment picker) can react to accounts being
// connected/disconnected without owning any of the OAuth machinery.
const changeListeners = new Set();

export function onAccountsChange(listener) {
  changeListeners.add(listener);
}

function notifyChange() {
  for (const listener of changeListeners) listener();
}

export function getAccount(name) {
  return accounts[name] ?? null;
}

/// True when the server can complete the OAuth exchange for `name`, i.e. a
/// Connect attempt can succeed. Await-able because the probe is async.
export async function isProviderAvailable(name) {
  await providersProbe;
  return Boolean(serverProviders?.[name]?.client_id);
}

/// Starts the connect flow for `name` from anywhere in the UI (must be
/// called from a user gesture — the authorize popup is blocked otherwise).
export async function connectProvider(name) {
  await providersProbe;
  const clientId = serverProviders?.[name]?.client_id;
  if (!clientId) throw new Error(`${GIT_PROVIDERS[name].label} is not configured on the server`);
  connect(name, clientId);
}

function load() {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY));
    if (parsed && typeof parsed === "object") return parsed;
  } catch {
    // corrupt value: start fresh
  }
  return {};
}

function persist() {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(accounts));
}

/// Credentials for `git_clone`, keyed by git host — the shape
/// isomorphic-git's `onAuth` expects. Passed into the agent worker with each
/// run (workers can't read localStorage).
export function getGitAuth() {
  const auth = {};
  for (const [name, account] of Object.entries(accounts)) {
    const provider = GIT_PROVIDERS[name];
    if (provider && account?.accessToken) {
      auth[provider.host] = provider.auth(account.accessToken);
    }
  }
  return auth;
}

function redirectUri() {
  return new URL("oauth-callback.html", location.href).toString();
}

function connect(name, clientId) {
  const provider = GIT_PROVIDERS[name];
  // The provider echoes `state` back untouched; embedding the name lets the
  // channel handler know which provider the code belongs to, and the random
  // suffix is the CSRF check.
  const state = `${name}:${crypto.randomUUID()}`;
  pendingState = state;

  const url = new URL(provider.authorizeUrl);
  url.searchParams.set("client_id", clientId);
  url.searchParams.set("redirect_uri", redirectUri());
  url.searchParams.set("scope", provider.scope);
  url.searchParams.set("state", state);

  const popup = window.open(url, "cooper-oauth", "width=900,height=720,popup");
  if (!popup) {
    pendingState = null;
    statusMessage = "Popup blocked — allow popups for this site and retry.";
  } else {
    statusMessage = `Waiting for ${provider.label} authorization…`;
  }
  render();
}

async function completeConnect({ code, state, error, errorDescription }) {
  if (!pendingState || state !== pendingState) return; // stale or foreign
  pendingState = null;
  const name = state.split(":")[0];
  const provider = GIT_PROVIDERS[name];

  try {
    if (error) throw new Error(errorDescription || error);
    if (!code) throw new Error("no authorization code returned");

    const response = await fetch(`/oauth/${name}/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, redirect_uri: redirectUri() }),
    });
    const data = await response.json().catch(() => ({}));
    // GitHub reports expired/invalid codes as 200 + { error } — check both.
    if (!response.ok || data.error || !data.access_token) {
      throw new Error(
        data.error_description || data.error || `token exchange failed (${response.status})`,
      );
    }

    // Best-effort: the account works without a resolved identity, the card
    // just shows the provider name instead of the username.
    const identity = await provider.fetchIdentity(data.access_token).catch(() => ({}));

    accounts[name] = {
      accessToken: data.access_token,
      tokenType: data.token_type,
      scope: data.scope,
      ...identity,
      connectedAt: Date.now(),
    };
    persist();
    statusMessage = "";
    notifyChange();
  } catch (err) {
    statusMessage = `Connecting ${provider.label} failed: ${err.message || err}`;
  }
  render();
}

function disconnect(name) {
  delete accounts[name];
  persist();
  statusMessage = "";
  notifyChange();
  render();
}

function renderAccountRow(name) {
  const provider = GIT_PROVIDERS[name];
  const account = accounts[name];
  const configured = Boolean(serverProviders?.[name]?.client_id);

  const row = document.createElement("div");
  row.className = "git-account-row";

  const label = document.createElement("span");
  label.className = "git-account-provider";
  label.textContent = provider.label;
  row.appendChild(label);

  if (account) {
    if (account.avatarUrl) {
      const avatar = document.createElement("img");
      avatar.className = "git-account-avatar";
      avatar.src = account.avatarUrl;
      avatar.alt = "";
      // COEP: require-corp blocks the avatar unless it opts in via CORS.
      avatar.crossOrigin = "anonymous";
      row.appendChild(avatar);
    }
    const login = document.createElement("span");
    login.className = "git-account-login";
    login.textContent = account.login ?? "connected";
    row.appendChild(login);

    const disconnectBtn = document.createElement("button");
    disconnectBtn.type = "button";
    disconnectBtn.textContent = "Disconnect";
    disconnectBtn.addEventListener("click", () => disconnect(name));
    row.appendChild(disconnectBtn);
  } else if (configured) {
    const connectBtn = document.createElement("button");
    connectBtn.type = "button";
    connectBtn.textContent = "Connect";
    connectBtn.addEventListener("click", () =>
      connect(name, serverProviders[name].client_id),
    );
    row.appendChild(connectBtn);
  } else {
    const unavailable = document.createElement("span");
    unavailable.className = "hint";
    unavailable.textContent = "not configured on the server";
    row.appendChild(unavailable);
  }

  return row;
}

function render() {
  const container = $("git-accounts");
  if (!container) return;
  container.innerHTML = "";

  const block = document.createElement("div");
  block.className = "provider";

  const header = document.createElement("div");
  header.className = "provider-header";
  const title = document.createElement("span");
  title.className = "provider-name";
  title.textContent = "Connected accounts";
  header.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent =
    serverProviders === null
      ? "Connecting a git account needs the cooper web server with OAuth " +
        "credentials configured (e.g. GITHUB_CLIENT_ID / GITHUB_CLIENT_SECRET)."
      : "Connect a git account to let git_clone access your private " +
        "repositories. Tokens are stored in this browser only.";

  block.append(header, hint);
  for (const name of Object.keys(GIT_PROVIDERS)) {
    // With no server support, only show providers that still hold a stored
    // account (so they can be disconnected).
    if (serverProviders === null && !accounts[name]) continue;
    block.appendChild(renderAccountRow(name));
  }

  if (statusMessage) {
    const status = document.createElement("p");
    status.className = "hint git-account-status";
    status.textContent = statusMessage;
    block.appendChild(status);
  }

  container.appendChild(block);
}

export async function initGitAccounts() {
  accounts = load();

  new BroadcastChannel(CHANNEL_NAME).onmessage = (event) => {
    completeConnect(event.data ?? {});
  };

  render(); // immediately, before the providers probe resolves

  providersProbe = (async () => {
    try {
      const response = await fetch("/oauth/providers");
      serverProviders = response.ok ? await response.json() : null;
    } catch {
      serverProviders = null;
    }
  })();
  await providersProbe;
  render();
}
