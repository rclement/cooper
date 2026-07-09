// Agent tool that runs Python in-browser via Pyodide (CPython compiled to
// wasm). The interpreter itself (pyodide.mjs/.asm.wasm, stdlib) is vendored
// under vendor/pyodide/ — see vendor/README.md — so startup doesn't depend
// on a CDN being reachable. Individual packages (numpy, pandas, etc.) are
// NOT vendored: they aren't part of the pyodide npm distribution at all and
// are fetched lazily from jsDelivr's package repository only the first time
// something actually imports them, via `packageBaseUrl` below.
import { getRoot } from "./workspace-fs.js";

const PYODIDE_VERSION = "0.29.4";
const PYODIDE_INDEX_URL = new URL("./vendor/pyodide/", import.meta.url).href;
const PYODIDE_PACKAGE_BASE_URL = `https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full/`;

// Lazily created once per Worker (or tab, for a hypothetical main-thread
// use), then reused across calls — loading the runtime is the expensive
// part, and reuse also means variables/imports persist between run_python
// calls within the same session, like a real REPL.
let statePromise = null;

async function ensureState() {
  if (!statePromise) {
    statePromise = (async () => {
      const { loadPyodide } = await import(/* @vite-ignore */ `${PYODIDE_INDEX_URL}pyodide.mjs`);
      const pyodide = await loadPyodide({
        indexURL: PYODIDE_INDEX_URL,
        packageBaseUrl: PYODIDE_PACKAGE_BASE_URL,
      });
      // Mounted at /mnt/opfs rather than something like /workspace: a name
      // that reads as a plausible directory an agent might invent by habit
      // (e.g. "save output to workspace/") collides with the mount itself,
      // silently nesting a real "workspace" folder inside OPFS root instead
      // of writing where intended.
      const nativeFs = await pyodide.mountNativeFS("/mnt/opfs", await getRoot());
      // Pyodide's cwd otherwise defaults to "/", a throwaway in-memory root
      // unrelated to OPFS — a bare relative path like `to_csv("out.csv")`
      // would silently "succeed" there and then vanish, invisible to every
      // other tool. Chdir'ing into the mount makes relative paths resolve
      // into the workspace by default, matching every other tool's
      // workspace-relative convention.
      pyodide.FS.chdir("/mnt/opfs");
      // micropip itself ships as a Pyodide package, not a stdlib module —
      // without this, `import micropip` fails with ModuleNotFoundError on
      // the very first call, before the agent even gets to `.install(...)`.
      await pyodide.loadPackage("micropip");
      return { pyodide, nativeFs };
    })();
  }
  return statePromise;
}

export const PYTHON_TOOLS = {
  run_python: {
    schema: {
      name: "run_python",
      description:
        "Run Python code in an in-browser interpreter (Pyodide). The working directory is the workspace root, so plain relative paths (e.g. open('data.csv'), df.to_csv('out.csv')) read/write the exact files visible in the Workspace view. Returns captured stdout/stderr plus the string form of the last expression's value, if any. Variables and imports persist between calls in the same session. Packages bundled with Pyodide (numpy, pandas, and most other common scientific-Python packages) load automatically the first time they're imported. For anything else, `import micropip` then `await micropip.install(\"package-name\")` installs a pure-Python wheel from PyPI.",
      parameters: {
        code: {
          type: "string",
          description: "Python source to execute.",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { code } = JSON.parse(argsJson);
      const { pyodide, nativeFs } = await ensureState();

      let stdout = "";
      let stderr = "";
      pyodide.setStdout({ batched: (line) => { stdout += `${line}\n`; } });
      pyodide.setStderr({ batched: (line) => { stderr += `${line}\n`; } });

      try {
        let resultText = null;
        try {
          // Scans `code` for import statements and fetches/loads any that
          // match a package in Pyodide's bundled distribution — this is
          // what makes `import numpy` "just work" on first use without the
          // agent needing to reach for micropip at all.
          await pyodide.loadPackagesFromImports(code);
          const result = await pyodide.runPythonAsync(code);
          if (result !== undefined && result !== null) {
            resultText = String(result);
            result.destroy?.();
          }
        } catch (err) {
          throw new Error([stdout, stderr, String(err)].filter(Boolean).join("\n"));
        }

        const parts = [];
        if (stdout) parts.push(stdout.trimEnd());
        if (stderr) parts.push(`[stderr]\n${stderr.trimEnd()}`);
        if (resultText !== null) parts.push(`=> ${resultText}`);
        return parts.length > 0 ? parts.join("\n") : "(no output)";
      } finally {
        pyodide.setStdout({});
        pyodide.setStderr({});
        await nativeFs.syncfs();
      }
    },
  },
};
