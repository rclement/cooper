// About/landing view: optional onboarding shortcuts to connect an external
// LLM provider or a GitHub account. Delegates all persistence to
// settings.js/git-accounts.js rather than duplicating their state — this
// view is just a slimmed-down entry point into it.
import { addProvider } from "./settings.js";
import {
  connectProvider,
  isProviderAvailable,
  getAccount,
  onAccountsChange,
} from "./git-accounts.js";

const $ = (id) => document.getElementById(id);

function renderGithubCard() {
  const statusEl = $("about-github-status");
  const btn = $("about-connect-github");
  if (!statusEl || !btn) return;

  const account = getAccount("github");
  if (account) {
    statusEl.textContent = `Connected as ${account.login ?? "GitHub"} — git_clone can reach your private repos.`;
    btn.hidden = true;
    return;
  }

  btn.hidden = false;
  statusEl.textContent = "Checking availability…";
  isProviderAvailable("github").then((available) => {
    if (getAccount("github")) return; // connected while this was in flight
    btn.disabled = !available;
    statusEl.textContent = available
      ? "Lets the agent's git_clone tool reach your private repositories."
      : "Needs the cooper web server configured with OAuth credentials — connect later in Settings.";
  });
}

export function initAbout() {
  onAccountsChange(renderGithubCard);
  renderGithubCard();

  $("about-connect-github").addEventListener("click", () => {
    connectProvider("github").catch((err) => {
      $("about-github-status").textContent = err.message || String(err);
    });
  });

  $("about-provider-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const name = $("about-provider-name").value.trim();
    const baseUrl = $("about-provider-base-url").value.trim();
    const apiKey = $("about-provider-api-key").value;
    const model = $("about-provider-model").value.trim();
    const statusEl = $("about-provider-status");

    if (addProvider({ name, baseUrl, apiKey, model: model || null })) {
      event.target.reset();
      statusEl.textContent = model
        ? `Saved — "${name}" is ready to use.`
        : `Saved — add a model for "${name}" in Settings before using it.`;
    } else {
      statusEl.textContent = "Name and base URL are required.";
    }
  });

  $("about-scroll-setup").addEventListener("click", () => {
    $("about-setup").scrollIntoView({ behavior: "smooth", block: "start" });
  });
}
