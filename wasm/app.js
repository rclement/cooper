import init, { CooperAgent } from './pkg/cooper_wasm.js';

// ── Boot ──────────────────────────────────────────────────────────────────────

(async () => {
  try {
    await init();
    setReady(true);
  } catch (err) {
    appendSystemError('Failed to load WASM module: ' + err);
  }
})();

// ── State ─────────────────────────────────────────────────────────────────────

let busy = false;

function setReady(ready) {
  document.getElementById('prompt-input').disabled = !ready;
  document.getElementById('send-btn').disabled = !ready;
  if (ready) document.getElementById('prompt-input').focus();
}

// ── Config ────────────────────────────────────────────────────────────────────

const CONFIG_KEY = 'cooper-config';
const DEFAULTS = {
  base_url:      'http://localhost:11434/v1',
  api:           'openai-completions',
  api_key:       'ollama',
  model:         'qwen3:latest',
  system_prompt: 'You are a helpful AI assistant.',
};

function loadConfig() {
  try {
    const raw = localStorage.getItem(CONFIG_KEY);
    return raw ? { ...DEFAULTS, ...JSON.parse(raw) } : { ...DEFAULTS };
  } catch {
    return { ...DEFAULTS };
  }
}

function saveConfig() {
  const cfg = {
    base_url:      document.getElementById('base-url').value.trim(),
    api:           document.getElementById('api-type').value,
    api_key:       document.getElementById('api-key').value,
    model:         document.getElementById('model').value.trim(),
    system_prompt: document.getElementById('system-prompt').value,
  };
  localStorage.setItem(CONFIG_KEY, JSON.stringify(cfg));
  return cfg;
}

function populateSettings() {
  const cfg = loadConfig();
  document.getElementById('base-url').value      = cfg.base_url;
  document.getElementById('api-type').value      = cfg.api;
  document.getElementById('api-key').value       = cfg.api_key;
  document.getElementById('model').value         = cfg.model;
  document.getElementById('system-prompt').value = cfg.system_prompt;
}

// ── DOM helpers ───────────────────────────────────────────────────────────────

const $messages = () => document.getElementById('messages');

function scrollBottom() {
  const el = $messages();
  el.scrollTop = el.scrollHeight;
}

function removeWelcome() {
  document.getElementById('welcome')?.remove();
}

function esc(s) {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

function addUserMessage(text) {
  removeWelcome();
  const div = document.createElement('div');
  div.className = 'msg user';
  div.innerHTML =
    `<div class="msg-label">You</div>` +
    `<div class="msg-body">${esc(text)}</div>`;
  $messages().appendChild(div);
  scrollBottom();
}

function makeSpinner() {
  const d = document.createElement('div');
  d.className = 'spinner';
  d.innerHTML = '<span></span><span></span><span></span>';
  return d;
}

// Creates an assistant bubble and returns a handle for appending chunks.
function startAssistantBubble() {
  removeWelcome();

  const wrapper = document.createElement('div');
  wrapper.className = 'msg assistant';

  const label = document.createElement('div');
  label.className = 'msg-label';
  label.textContent = 'Cooper';
  wrapper.appendChild(label);

  const body = document.createElement('div');
  body.className = 'msg-body';
  wrapper.appendChild(body);

  let spinner = makeSpinner();
  body.appendChild(spinner);

  $messages().appendChild(wrapper);
  scrollBottom();

  // Lazily created <span> for streaming content text — avoids a new node per token.
  let contentNode = null;

  function ensureContentNode() {
    if (!contentNode) {
      contentNode = document.createElement('span');
      contentNode.className = 'content-text';
      body.appendChild(contentNode);
    }
    return contentNode;
  }

  function removeSpinner() {
    spinner?.remove();
    spinner = null;
  }

  function appendChunk(chunk) {
    removeSpinner();

    switch (chunk.type) {
      case 'content': {
        ensureContentNode().textContent += chunk.text;
        break;
      }

      case 'thinking': {
        // Collapse consecutive thinking tokens into one block per think session.
        let block = body.querySelector('.think-block:last-child');
        if (!block || block.nextSibling !== null) {
          // nextSibling check: start a new block when something else was added after the last one
          block = document.createElement('div');
          block.className = 'think-block';
          body.appendChild(block);
        }
        block.textContent += chunk.text;
        // Reset the streaming content node so next content text starts fresh after thinking.
        contentNode = null;
        break;
      }

      case 'tool_call': {
        // Reset content node so next content appears after the tool block.
        contentNode = null;

        const div = document.createElement('div');
        div.className = 'tool-block call';

        const hdr = document.createElement('div');
        hdr.className = 'tool-header';
        hdr.textContent = '▶ ' + chunk.name;
        div.appendChild(hdr);

        if (chunk.args && chunk.args !== '{}') {
          const pre = document.createElement('pre');
          pre.className = 'tool-code';
          try {
            pre.textContent = JSON.stringify(JSON.parse(chunk.args), null, 2);
          } catch {
            pre.textContent = chunk.args;
          }
          div.appendChild(pre);
        }

        body.appendChild(div);

        // Show a new spinner while waiting for the tool result.
        spinner = makeSpinner();
        body.appendChild(spinner);
        break;
      }

      case 'tool_result': {
        removeSpinner();
        contentNode = null;

        const div = document.createElement('div');
        div.className = 'tool-block result';

        const hdr = document.createElement('div');
        hdr.className = 'tool-header';
        hdr.textContent = '◀ ' + chunk.name;
        div.appendChild(hdr);

        const pre = document.createElement('pre');
        pre.className = 'tool-code';
        pre.textContent = chunk.output;
        div.appendChild(pre);

        body.appendChild(div);

        // Spinner while the model processes the result and generates the next response.
        spinner = makeSpinner();
        body.appendChild(spinner);
        break;
      }

      case 'usage': {
        contentNode = null;
        const line = document.createElement('div');
        line.className = 'usage-line';
        line.textContent =
          `${chunk.prompt_tokens} in · ${chunk.completion_tokens} out · ${chunk.total_tokens} total tokens`;
        body.appendChild(line);
        break;
      }

      default:
        break;
    }

    scrollBottom();
  }

  function finalize(errorMsg) {
    removeSpinner();
    if (errorMsg) {
      const err = document.createElement('div');
      err.className = 'error-inline';
      err.textContent = errorMsg;
      body.appendChild(err);
      scrollBottom();
    }
  }

  return { appendChunk, finalize };
}

function appendSystemError(msg) {
  const div = document.createElement('div');
  div.className = 'msg assistant';
  div.innerHTML =
    `<div class="msg-label">System</div>` +
    `<div class="msg-body"><div class="error-inline">${esc(msg)}</div></div>`;
  $messages().appendChild(div);
  scrollBottom();
}

// ── Send ──────────────────────────────────────────────────────────────────────

async function sendMessage() {
  if (busy) return;

  const input = document.getElementById('prompt-input');
  const text = input.value.trim();
  if (!text) return;

  const cfg = loadConfig();
  if (!cfg.base_url || !cfg.model) {
    alert('Please configure a Provider URL and Model in Settings first.');
    return;
  }

  input.value = '';
  autoResize(input);
  setBusy(true);

  addUserMessage(text);
  const bubble = startAssistantBubble();

  let agent;
  try {
    agent = new CooperAgent(JSON.stringify(cfg));
  } catch (err) {
    bubble.finalize('Failed to initialise agent: ' + err);
    setBusy(false);
    return;
  }

  try {
    await agent.run_prompt(text, (chunkJson) => {
      try {
        bubble.appendChunk(JSON.parse(chunkJson));
      } catch (e) {
        console.error('[cooper] chunk parse error:', e, chunkJson);
      }
    });
    bubble.finalize(null);
  } catch (err) {
    bubble.finalize(String(err));
  }

  setBusy(false);
  input.focus();
}

function setBusy(val) {
  busy = val;
  document.getElementById('send-btn').disabled = val;
  document.getElementById('prompt-input').disabled = val;
}

// ── Input auto-resize ─────────────────────────────────────────────────────────

function autoResize(el) {
  el.style.height = 'auto';
  el.style.height = Math.min(el.scrollHeight, 180) + 'px';
}

// ── Wire-up ───────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
  populateSettings();

  // Settings panel toggle
  document.getElementById('settings-btn').addEventListener('click', () => {
    document.getElementById('settings-panel').classList.toggle('hidden');
  });
  document.getElementById('settings-save').addEventListener('click', () => {
    saveConfig();
    document.getElementById('settings-panel').classList.add('hidden');
  });

  // Send button
  document.getElementById('send-btn').addEventListener('click', sendMessage);

  // Enter key (without Shift) submits; Shift+Enter inserts newline
  const input = document.getElementById('prompt-input');
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  });
  input.addEventListener('input', () => autoResize(input));
});
