// Main-thread UI glue: wires the form to the agent Worker and renders the
// events it streams back. No framework, no build step.
const worker = new Worker("worker.js", { type: "module" });

const $ = (id) => document.getElementById(id);

function appendToolCall(event) {
  const div = document.createElement("div");
  div.className = "tool-call";
  div.textContent = `▶ ${event.name} ${JSON.stringify(event.arguments)}`;
  $("tool-log").appendChild(div);
}

function appendToolResult(event) {
  const div = document.createElement("div");
  div.className = "tool-result";
  const { Ok, Err } = event.result;
  div.textContent = Ok !== undefined ? `◀ ${Ok}` : `◀ error: ${Err}`;
  $("tool-log").appendChild(div);
}

function handleEvent(event) {
  switch (event.type) {
    case "chunk":
      if (event.text) $("output").textContent += event.text;
      if (event.reasoning) $("reasoning").textContent += event.reasoning;
      break;
    case "usage":
      $("usage").textContent =
        `prompt: ${event.prompt_tokens}, ` +
        `completion: ${event.completion_tokens}, ` +
        `total: ${event.total_tokens}`;
      break;
    case "tool_call":
      appendToolCall(event);
      break;
    case "tool_result":
      appendToolResult(event);
      break;
  }
}

worker.onmessage = (message) => {
  const msg = message.data;
  if (msg.type === "event") {
    handleEvent(msg.event);
  } else if (msg.type === "done") {
    $("status").textContent = "done";
    $("run").disabled = false;
  } else if (msg.type === "error") {
    $("status").textContent = `error: ${msg.error}`;
    $("run").disabled = false;
  }
};

$("run").addEventListener("click", () => {
  const config = {
    base_url: $("base-url").value,
    api_key: $("api-key").value,
    model: $("model").value,
  };
  const prompt = $("prompt").value;

  $("output").textContent = "";
  $("reasoning").textContent = "";
  $("tool-log").textContent = "";
  $("usage").textContent = "";
  $("status").textContent = "running…";
  $("run").disabled = true;

  worker.postMessage({ prompt, config });
});
