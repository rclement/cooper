// Agent tools that give the model hands-on access to the Workspace (the
// OPFS-backed virtual filesystem browsable from the "Workspace" view). Same
// `{ schema, execute }` shape as builtin-tools.js, registered into the wasm
// agent the same way — see BUILTIN_TOOLS's header comment for the contract.
//
// These execute inside the agent Worker (see worker.js), which has its own
// direct handle onto OPFS — the same origin-private filesystem the main
// thread's Workspace view reads and writes, so nothing here needs to talk to
// the main thread to see the effects (or vice versa).
import { readFileText, writeFileText, listTree, gitClone } from "./workspace-fs.js";

export const WORKSPACE_TOOLS = {
  list_files: {
    schema: {
      name: "list_files",
      description:
        "List files and directories in the workspace, recursively. Directory entries are suffixed with '/'.",
      parameters: {
        path: {
          type: "string",
          description: "Directory path relative to the workspace root. Empty or omitted lists the root.",
          required: false,
        },
      },
    },
    async execute(argsJson) {
      const { path } = JSON.parse(argsJson);
      const lines = await listTree(path || "");
      return lines.length > 0 ? lines.join("\n") : "(empty directory)";
    },
  },

  read_file: {
    schema: {
      name: "read_file",
      description: "Read the full text content of a file in the workspace.",
      parameters: {
        path: {
          type: "string",
          description: "File path relative to the workspace root.",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { path } = JSON.parse(argsJson);
      return await readFileText(path);
    },
  },

  write_file: {
    schema: {
      name: "write_file",
      description:
        "Create or overwrite a file in the workspace with the given content. Creates any missing parent directories.",
      parameters: {
        path: {
          type: "string",
          description: "File path relative to the workspace root.",
          required: true,
        },
        content: {
          type: "string",
          description: "The full text content to write.",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { path, content } = JSON.parse(argsJson);
      await writeFileText(path, content ?? "");
      return `Wrote ${(content ?? "").length} characters to "${path}".`;
    },
  },

  edit_file: {
    schema: {
      name: "edit_file",
      description:
        "Replace an exact, unique occurrence of old_text with new_text in an existing workspace file. Fails if old_text isn't found, or isn't unique.",
      parameters: {
        path: {
          type: "string",
          description: "File path relative to the workspace root.",
          required: true,
        },
        old_text: {
          type: "string",
          description: "Exact text to find (must occur exactly once in the file).",
          required: true,
        },
        new_text: {
          type: "string",
          description: "Text to replace it with.",
          required: true,
        },
      },
    },
    async execute(argsJson) {
      const { path, old_text, new_text } = JSON.parse(argsJson);
      const current = await readFileText(path);
      const occurrences = current.split(old_text).length - 1;
      if (occurrences === 0) {
        throw new Error(`old_text not found in "${path}".`);
      }
      if (occurrences > 1) {
        throw new Error(`old_text matches ${occurrences} locations in "${path}"; it must be unique.`);
      }
      const updated = current.replace(old_text, new_text ?? "");
      await writeFileText(path, updated);
      return `Edited "${path}".`;
    },
  },

  git_clone: {
    schema: {
      name: "git_clone",
      description:
        "Clone a public git repository into the workspace via HTTP (through isomorphic-git's public CORS proxy). Fails if the destination already exists.",
      parameters: {
        url: {
          type: "string",
          description: "Repository URL, e.g. https://github.com/owner/repo.git",
          required: true,
        },
        path: {
          type: "string",
          description: "Destination folder path relative to the workspace root, e.g. 'repo'.",
          required: true,
        },
        branch: {
          type: "string",
          description: "Branch to check out. Defaults to the repository's default branch.",
          required: false,
        },
        shallow: {
          type: "string",
          description: "\"false\" to fetch full history instead of a depth-1 shallow clone. Defaults to shallow.",
          required: false,
        },
      },
    },
    async execute(argsJson) {
      const { url, path, branch, shallow } = JSON.parse(argsJson);
      await gitClone({ url, destPath: path, branch, shallow: shallow !== "false" });
      return `Cloned "${url}" into "${path}".`;
    },
  },
};
