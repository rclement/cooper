// Agent tool that runs SQL over an in-browser analytical database
// (DuckDB-wasm), loaded lazily from its CDN build the same way python-tool.js
// loads Pyodide — no bundler, no server round-trip. DuckDB-wasm ships its own
// worker bundle, so instantiating it here spawns a nested Worker inside the
// agent's own Worker (see worker.js); this is a standard, well-supported use
// of the Worker API, not something specific to this app.
//
// Workspace (OPFS) files are exposed to queries on demand via
// `db.registerFileBuffer`, which hands DuckDB the file's full bytes up
// front. `registerFileHandle` with the BROWSER_FSACCESS protocol looks like
// the better fit (lazy reads, no buffering) since it accepts any File System
// Access API handle and OPFS's `FileSystemFileHandle` qualifies — but that
// handle then has to be structured-cloned into DuckDB-wasm's own nested
// Worker, which in turn has to open its own sync access handle on it; that
// path doesn't work reliably in practice. Reading the bytes on this side and
// handing DuckDB a plain buffer sidesteps the whole handle-across-workers
// question, at the cost of loading the file into memory once (same
// trade-off python-tool.js already makes reading workspace files as text).
//
// Query results come back as a JSON array of row objects, deliberately the
// same shape render_chart's `data` parameter expects — so the agent can run a
// query and hand its result straight to render_chart without reshaping it.
import { readFileBlob } from "./workspace-fs.js";

const DUCKDB_VERSION = "1.29.0";
const DUCKDB_CDN = `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@${DUCKDB_VERSION}/+esm`;

const MAX_ROWS = 1000;

// Lazily created once per Worker, then reused across calls — instantiating
// the wasm database is the expensive part, and reuse means tables/views
// created by one run_sql call are still there for the next, like a real
// database session.
let statePromise = null;

async function ensureState() {
  if (!statePromise) {
    statePromise = (async () => {
      const duckdb = await import(/* @vite-ignore */ DUCKDB_CDN);
      const bundle = await duckdb.selectBundle(duckdb.getJsDelivrBundles());
      const workerUrl = URL.createObjectURL(
        new Blob([`importScripts("${bundle.mainWorker}");`], { type: "text/javascript" }),
      );
      const worker = new Worker(workerUrl);
      const db = new duckdb.AsyncDuckDB(new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING), worker);
      await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
      URL.revokeObjectURL(workerUrl);
      const conn = await db.connect();
      // DuckDB-wasm doesn't autoload extensions by default the way native
      // DuckDB does — without this, read_csv_auto('https://...')/
      // read_parquet('s3://...') etc. fail because httpfs (and any other
      // extension DuckDB recognizes as "known") is never installed/loaded.
      await conn.query("SET autoinstall_known_extensions=true; SET autoload_known_extensions=true;");
      return { duckdb, db, conn, registeredFiles: new Set() };
    })();
  }
  return statePromise;
}

// JSON.stringify chokes on BigInt, which is how DuckDB-wasm surfaces 64-bit
// integer columns (BIGINT, HUGEINT, row counts from aggregates, etc.).
// Downgrade to a plain number when it round-trips exactly, else keep full
// precision as a string rather than throwing or silently truncating.
function jsonSafe(_key, value) {
  if (typeof value !== "bigint") return value;
  const asNumber = Number(value);
  return Number.isSafeInteger(asNumber) ? asNumber : value.toString();
}

export const DUCKDB_TOOLS = {
  run_sql: {
    schema: {
      name: "run_sql",
      description:
        "Run a SQL query against an in-browser analytical database (DuckDB) and return the result as a JSON array of row objects — the same shape render_chart's `data` parameter expects, so a query result can be passed straight to render_chart. Supports full SQL (joins, window functions, aggregates, CTEs) plus DuckDB extras like read_csv_auto()/read_parquet()/read_json_auto() and CREATE TABLE/VIEW, which persist for later calls in this session. Remote datasets work directly by URL, e.g. SELECT * FROM read_parquet('https://.../file.parquet') or read_csv_auto('https://.../data.csv') — no setup needed. To query a workspace file instead, list its path in `files` and reference it in SQL by its filename, e.g. files: [\"data/sales.csv\"] then SELECT * FROM read_csv_auto('sales.csv'). Results over 1000 rows are truncated.",
      parameters: {
        query: {
          type: "string",
          description: "SQL to execute.",
          required: true,
        },
        files: {
          type: "string",
          description:
            "JSON-encoded array of workspace file paths (CSV/Parquet/JSON) to make available to the query, referenced in SQL by their filename.",
          required: false,
        },
      },
    },
    async execute(argsJson) {
      const { query, files } = JSON.parse(argsJson);
      const { db, conn, registeredFiles } = await ensureState();

      let paths = [];
      if (files) {
        try {
          paths = JSON.parse(files);
        } catch {
          throw new Error("`files` must be a JSON-encoded array of workspace file paths.");
        }
      }

      for (const path of paths) {
        const name = String(path).split("/").pop();
        if (registeredFiles.has(name)) continue;
        const blob = await readFileBlob(path);
        await db.registerFileBuffer(name, new Uint8Array(await blob.arrayBuffer()));
        registeredFiles.add(name);
      }

      let result;
      try {
        result = await conn.query(query);
      } catch (err) {
        // DuckDB-wasm's own JS bindings sometimes fail to translate a native
        // exception into a real Error (message/name end up empty), so it
        // surfaces as the useless "[object WebAssembly.Exception]" — this
        // reliably happens when a remote read_csv_auto/read_parquet URL
        // doesn't behave like a plain file server (blocked by CORS, a
        // non-200 status, a bot-challenge page, a redirect chain, etc). When
        // that's the shape of the failure and the query does reference a
        // remote URL, replace the opaque message with an actionable one
        // rather than leave the agent guessing.
        const message = (err && err.message) || String(err);
        const isOpaque = /WebAssembly\.Exception/i.test(message) || !message || message === "undefined";
        const referencesRemoteUrl = /['"]\s*(https?|s3):\/\//i.test(query);
        if (isOpaque && referencesRemoteUrl) {
          throw new Error(
            "DuckDB failed to read the remote URL, with no further detail available from the wasm runtime. " +
              "This most often means the URL returned a non-200 status, is missing CORS headers " +
              "(Access-Control-Allow-Origin) required for cross-origin browser fetches, or is gated by " +
              "bot-protection/a login wall. Verify with `curl -I <url>` that it returns 200 with the expected " +
              "Content-Type before assuming the query itself is wrong.",
          );
        }
        throw err;
      }
      const rows = result.toArray().map((row) => row.toJSON());
      const truncated = rows.length > MAX_ROWS;
      const output = truncated ? rows.slice(0, MAX_ROWS) : rows;

      const json = JSON.stringify(output, jsonSafe);
      return truncated
        ? `${json}\n(truncated to ${MAX_ROWS} of ${rows.length} rows)`
        : json;
    },
  },
};
