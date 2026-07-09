// Analytics view: aggregates every saved session (see sessions.js) into
// usage stats, filterable like a small BI dashboard (date range, provider,
// model). Nothing here talks to a server — it's a read of this device's own
// IndexedDB session store, computed entirely client-side.
//
// Charts reuse chart-render.js's renderChart directly (the same hand-rolled
// SVG renderer the agent's render_chart tool draws with) rather than a
// second rendering path — the rows/spec shape is exactly what that function
// already expects.
import { listSessions } from "./sessions.js";
import { renderChart } from "./chart-render.js";

const $ = (id) => document.getElementById(id);

const RANGE_PRESETS = {
  all: { label: "All time", days: null },
  today: { label: "Today", days: 1 },
  "7d": { label: "Last 7 days", days: 7 },
  "30d": { label: "Last 30 days", days: 30 },
  "90d": { label: "Last 90 days", days: 90 },
};

let allSessions = [];
let initialized = false;

function formatCompact(n) {
  if (n < 10_000) return Math.round(n).toLocaleString();
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

function dayKey(ms) {
  return new Date(ms).toISOString().slice(0, 10);
}

/// Derives per-session stats from its persisted `history` alone (see
/// core/src/agent.rs's `Message`/`ToolCall` — the exact JSON shape exported
/// by `WasmAgent::export_history` and stored verbatim by sessions.js).
/// Token usage and timings live on the messages themselves, so history is
/// the single source of truth — there's no separate usage log to keep in
/// sync. Each assistant round with usage also yields a timestamped
/// `usagePoints` entry (`at_ms` when the round started; sessions saved
/// before timestamps existed fall back to the session's creation time), so
/// charts can bucket token spend by when it actually happened.
function analyzeSession(session) {
  const messages = session.history ? JSON.parse(session.history) : [];
  let turns = 0;
  const toolNameByCallId = new Map();
  const toolStats = new Map(); // name -> { count, errors }
  const usage = { prompt: 0, completion: 0, total: 0 };
  const usagePoints = []; // { at, prompt, completion }

  for (const m of messages) {
    if (m.User !== undefined) {
      turns++;
    } else if (m.Assistant !== undefined) {
      for (const tc of m.Assistant.tool_calls ?? []) {
        toolNameByCallId.set(tc.id, tc.name);
        const s = toolStats.get(tc.name) ?? { count: 0, errors: 0 };
        s.count++;
        toolStats.set(tc.name, s);
      }
      if (m.Assistant.usage) {
        const { prompt_tokens, completion_tokens, total_tokens } = m.Assistant.usage;
        usage.prompt += prompt_tokens ?? 0;
        usage.completion += completion_tokens ?? 0;
        usage.total += total_tokens ?? 0;
        usagePoints.push({
          at: m.Assistant.at_ms ?? session.createdAt,
          prompt: prompt_tokens ?? 0,
          completion: completion_tokens ?? 0,
        });
      }
    } else if (m.Tool !== undefined) {
      const name = toolNameByCallId.get(m.Tool.call_id);
      const stats = name && toolStats.get(name);
      if (stats && m.Tool.result?.Err !== undefined) stats.errors++;
    }
  }

  return { turns, toolStats, usage, usagePoints };
}

function populateFilterOptions() {
  const providers = [...new Set(allSessions.map((s) => s.providerName).filter(Boolean))].sort();
  const models = [...new Set(allSessions.map((s) => s.model).filter(Boolean))].sort();

  const fillSelect = (select, values, allLabel) => {
    const current = select.value;
    select.innerHTML = "";
    const allOpt = document.createElement("option");
    allOpt.value = "";
    allOpt.textContent = allLabel;
    select.appendChild(allOpt);
    for (const v of values) {
      const opt = document.createElement("option");
      opt.value = v;
      opt.textContent = v;
      select.appendChild(opt);
    }
    if (values.includes(current)) select.value = current;
  };

  fillSelect($("af-provider"), providers, "All providers");
  fillSelect($("af-model"), models, "All models");
}

function applyFilters() {
  const range = RANGE_PRESETS[$("af-range").value] ?? RANGE_PRESETS.all;
  const provider = $("af-provider").value;
  const model = $("af-model").value;

  const cutoff = range.days ? Date.now() - range.days * 86_400_000 : null;

  return allSessions.filter((s) => {
    if (cutoff !== null && s.createdAt < cutoff) return false;
    if (provider && s.providerName !== provider) return false;
    if (model && s.model !== model) return false;
    return true;
  });
}

function renderKpis(sessions, analyzed) {
  const totalTurns = analyzed.reduce((sum, a) => sum + a.turns, 0);
  const totalTokens = analyzed.reduce((sum, a) => sum + a.usage.total, 0);
  const totalToolCalls = analyzed.reduce(
    (sum, a) => sum + [...a.toolStats.values()].reduce((s, v) => s + v.count, 0),
    0,
  );
  const totalToolErrors = analyzed.reduce(
    (sum, a) => sum + [...a.toolStats.values()].reduce((s, v) => s + v.errors, 0),
    0,
  );

  const tiles = [
    { label: "Sessions", value: formatCompact(sessions.length) },
    { label: "Total turns", value: formatCompact(totalTurns) },
    { label: "Total tokens", value: totalTokens > 0 ? formatCompact(totalTokens) : "—" },
    {
      label: "Tool calls",
      value: formatCompact(totalToolCalls),
      sub: totalToolCalls > 0 ? `${totalToolErrors} error${totalToolErrors === 1 ? "" : "s"}` : null,
    },
  ];

  const container = $("af-kpis");
  container.innerHTML = "";
  for (const tile of tiles) {
    const el = document.createElement("div");
    el.className = "kpi-tile";
    const label = document.createElement("div");
    label.className = "kpi-label";
    label.textContent = tile.label;
    const value = document.createElement("div");
    value.className = "kpi-value";
    value.textContent = tile.value;
    el.append(label, value);
    if (tile.sub) {
      const sub = document.createElement("div");
      sub.className = "kpi-sub" + (totalToolErrors > 0 ? " is-error" : "");
      sub.textContent = tile.sub;
      el.appendChild(sub);
    }
    container.appendChild(el);
  }
}

function renderEmptyChart(container, message) {
  container.innerHTML = "";
  const p = document.createElement("p");
  p.className = "hint";
  p.textContent = message;
  container.appendChild(p);
}

function renderSessionsOverTime(sessions) {
  const container = $("af-chart-sessions");
  if (sessions.length === 0) return renderEmptyChart(container, "No sessions in this range.");

  const byDay = new Map();
  for (const s of sessions) {
    const key = dayKey(s.createdAt);
    byDay.set(key, (byDay.get(key) ?? 0) + 1);
  }
  const rows = [...byDay.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([date, count]) => ({ date, count }));

  container.innerHTML = "";
  renderChart(container, rows, { type: "bar", x: "date", y: ["count"], title: null });
}

function renderTokensOverTime(sessions, analyzed) {
  const container = $("af-chart-tokens");
  const byDay = new Map();
  for (const a of analyzed) {
    for (const point of a.usagePoints) {
      const key = dayKey(point.at);
      const bucket = byDay.get(key) ?? { prompt: 0, completion: 0 };
      bucket.prompt += point.prompt;
      bucket.completion += point.completion;
      byDay.set(key, bucket);
    }
  }
  if (byDay.size === 0) {
    return renderEmptyChart(container, "No token data in this range (older sessions predate usage tracking).");
  }
  const rows = [...byDay.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([date, v]) => ({ date, prompt: v.prompt, completion: v.completion }));

  container.innerHTML = "";
  renderChart(container, rows, { type: "line", x: "date", y: ["prompt", "completion"], title: null });
}

function renderToolUsage(analyzed) {
  const container = $("af-chart-tools");
  const totals = new Map();
  for (const a of analyzed) {
    for (const [name, stats] of a.toolStats) {
      totals.set(name, (totals.get(name) ?? 0) + stats.count);
    }
  }
  if (totals.size === 0) return renderEmptyChart(container, "No tool calls in this range.");

  const rows = [...totals.entries()]
    .sort(([, a], [, b]) => b - a)
    .slice(0, 12)
    .map(([tool, count]) => ({ tool, count }));

  container.innerHTML = "";
  renderChart(container, rows, { type: "bar", x: "tool", y: ["count"], title: null });
}

function renderTable(sessions, analyzed) {
  const container = $("af-table");
  container.innerHTML = "";
  if (sessions.length === 0) {
    const p = document.createElement("p");
    p.className = "hint";
    p.textContent = "No sessions match the current filters.";
    container.appendChild(p);
    return;
  }

  const table = document.createElement("table");
  table.className = "analytics-table";
  const thead = document.createElement("thead");
  thead.innerHTML =
    "<tr><th>Session</th><th>Provider</th><th>Model</th><th>Created</th><th>Turns</th><th>Tokens</th><th>Tool calls</th></tr>";
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  const paired = sessions.map((s, i) => [s, analyzed[i]]).sort(([a], [b]) => b.updatedAt - a.updatedAt);
  for (const [session, stats] of paired) {
    const tr = document.createElement("tr");
    const toolCalls = [...stats.toolStats.values()].reduce((s, v) => s + v.count, 0);
    const toolErrors = [...stats.toolStats.values()].reduce((s, v) => s + v.errors, 0);

    const cells = [
      session.title || "(empty prompt)",
      session.providerName ?? "—",
      session.model ?? "—",
      new Date(session.createdAt).toLocaleString(),
      String(stats.turns),
      stats.usage.total > 0 ? stats.usage.total.toLocaleString() : "—",
      toolErrors > 0 ? `${toolCalls} (${toolErrors} error${toolErrors === 1 ? "" : "s"})` : String(toolCalls),
    ];
    for (const value of cells) {
      const td = document.createElement("td");
      td.textContent = value;
      tr.appendChild(td);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  container.appendChild(table);
}

async function render() {
  allSessions = await listSessions();
  populateFilterOptions();

  const sessions = applyFilters();
  const analyzed = sessions.map(analyzeSession);

  renderKpis(sessions, analyzed);
  renderSessionsOverTime(sessions);
  renderTokensOverTime(sessions, analyzed);
  renderToolUsage(analyzed);
  renderTable(sessions, analyzed);
}

function buildLayout(root) {
  root.innerHTML = `
    <div class="analytics-filters">
      <div class="field">
        <label for="af-range">Date range</label>
        <select id="af-range">
          ${Object.entries(RANGE_PRESETS)
            .map(([key, { label }]) => `<option value="${key}">${label}</option>`)
            .join("")}
        </select>
      </div>
      <div class="field">
        <label for="af-provider">Provider</label>
        <select id="af-provider"><option value="">All providers</option></select>
      </div>
      <div class="field">
        <label for="af-model">Model</label>
        <select id="af-model"><option value="">All models</option></select>
      </div>
      <button id="af-reset" type="button" class="secondary">Reset filters</button>
    </div>

    <div id="af-kpis" class="analytics-kpis"></div>

    <div class="analytics-charts">
      <div class="analytics-chart-card">
        <h3>Sessions per day</h3>
        <div id="af-chart-sessions"></div>
      </div>
      <div class="analytics-chart-card">
        <h3>Tokens per day</h3>
        <div id="af-chart-tokens"></div>
      </div>
      <div class="analytics-chart-card">
        <h3>Tool calls</h3>
        <div id="af-chart-tools"></div>
      </div>
    </div>

    <div class="analytics-table-card">
      <h3>Sessions</h3>
      <div id="af-table"></div>
    </div>
  `;

  $("af-range").addEventListener("change", render);
  $("af-provider").addEventListener("change", render);
  $("af-model").addEventListener("change", render);
  $("af-reset").addEventListener("click", () => {
    $("af-range").value = "all";
    $("af-provider").value = "";
    $("af-model").value = "";
    render();
  });
}

export async function initAnalytics() {
  buildLayout($("analytics-root"));
  initialized = true;
  await render();
}

/// Called whenever the Analytics nav item is (re-)selected — sessions saved
/// since the view was last shown (or filter option lists that have grown)
/// need a fresh read of IndexedDB, same reasoning as workspace.js's
/// refreshWorkspace.
export async function refreshAnalytics() {
  if (!initialized) return;
  await render();
}
