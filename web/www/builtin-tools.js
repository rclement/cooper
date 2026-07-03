// Built-in browser-side tools, registered into the wasm agent through the
// generic JS tool bridge (see `WasmAgent.register_tool` in web/src/lib.rs).
// `schema` matches Rust's `ToolSchema` JSON shape; `execute` receives the
// arguments as a JSON string of `{ [param]: string }` and must return (a
// promise resolving to) a string result — throwing/rejecting reports a tool
// error back to the agent. This is also the shape a future Pyodide-backed
// tool would use, with `execute` calling into the Python runtime instead.
export const BUILTIN_TOOLS = {
  fetch_url: {
    schema: {
      name: "fetch_url",
      description:
        "Fetch the contents of a URL over HTTP(S) and return the response body as text. Subject to the target server's CORS policy.",
      parameters: {
        url: {
          type: "string",
          description: "The URL to fetch",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { url } = JSON.parse(argsJson);
      const res = await fetch(url);
      if (!res.ok) throw new Error(`HTTP ${res.status} fetching ${url}`);
      return await res.text();
    },
  },
};
