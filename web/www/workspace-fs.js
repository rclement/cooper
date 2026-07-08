// Shared OPFS (Origin Private File System) helpers for the Workspace
// feature. Used both by workspace.js (the main-thread file browser UI) and
// workspace-tools.js (agent tools, executed inside the agent Worker) — OPFS
// handles are available in both contexts via `navigator.storage.getDirectory()`,
// so no message-passing bridge is needed between them.
//
// Ported from the `opfs/` playground at the repo root, generalized to work
// off POSIX-style path strings (what an agent tool call naturally produces)
// instead of the playground's UI-driven path arrays.

const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico"];
const AUDIO_EXTENSIONS = ["mp3", "wav", "ogg", "oga", "m4a", "flac", "aac", "weba"];
const VIDEO_EXTENSIONS = ["mp4", "webm", "ogv", "mov", "m4v"];
const PDF_EXTENSIONS = ["pdf"];

export function pathKey(pathArray) {
  return pathArray.join("/");
}

export function extOf(name) {
  const idx = name.lastIndexOf(".");
  return idx === -1 ? "" : name.slice(idx + 1).toLowerCase();
}

export function isImageName(name) {
  return IMAGE_EXTENSIONS.includes(extOf(name));
}

export function isAudioName(name) {
  return AUDIO_EXTENSIONS.includes(extOf(name));
}

export function isVideoName(name) {
  return VIDEO_EXTENSIONS.includes(extOf(name));
}

export function isPdfName(name) {
  return PDF_EXTENSIONS.includes(extOf(name));
}

export function isPreviewOnlyName(name) {
  return isImageName(name) || isAudioName(name) || isVideoName(name) || isPdfName(name);
}

export function humanSize(bytes) {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  const value = bytes / Math.pow(1024, i);
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function validEntryName(name) {
  if (!name) return false;
  if (name === "." || name === "..") return false;
  if (/[/\\]/.test(name)) return false;
  return true;
}

/// Splits a POSIX-style path (as an agent tool call would supply, e.g.
/// `"src/main.rs"` or `"/src/main.rs"`) into workspace-relative segments.
/// `.` segments are dropped as no-ops; `..` is rejected outright — OPFS is
/// already sandboxed to the origin, but an agent-supplied path should still
/// never be able to reason about "escaping" the workspace root.
export function toSegments(path) {
  const parts = String(path ?? "")
    .split("/")
    .map((p) => p.trim())
    .filter((p) => p.length > 0 && p !== ".");
  for (const p of parts) {
    if (p === "..") {
      throw new Error(`Invalid path: ".." is not allowed ("${path}")`);
    }
  }
  return parts;
}

export async function getRoot() {
  return navigator.storage.getDirectory();
}

export async function getDirHandle(pathArray, create = false) {
  let handle = await getRoot();
  for (const part of pathArray) {
    handle = await handle.getDirectoryHandle(part, { create });
  }
  return handle;
}

export async function listEntries(dirHandle) {
  const entries = [];
  for await (const [name, handle] of dirHandle.entries()) {
    entries.push({ name, handle, kind: handle.kind });
  }
  entries.sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "directory" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  return entries;
}

export async function calcDirSize(dirHandle) {
  let total = 0;
  for await (const [, handle] of dirHandle.entries()) {
    if (handle.kind === "file") {
      const file = await handle.getFile();
      total += file.size;
    } else {
      total += await calcDirSize(handle);
    }
  }
  return total;
}

// ---------- Writing files (Safari has no `createWritable()`) ----------
//
// Safari implements OPFS but never shipped the async `createWritable()`
// stream API — only the synchronous `createSyncAccessHandle()`, which the
// spec (and every implementation of it) restricts to Worker contexts. So on
// the main thread, where `createWritable` is undefined, writes are handed
// off to a small dedicated worker that opens the sync access handle instead;
// FileSystemFileHandle is structured-cloneable, so the already-resolved
// handle can just be posted over. Callers already running inside a Worker
// (the agent worker, this write worker itself) use the sync handle directly
// with no extra hop.

export async function toUint8Array(data) {
  if (typeof data === "string") return new TextEncoder().encode(data);
  if (data instanceof Blob) return new Uint8Array(await data.arrayBuffer());
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  throw new Error("Unsupported data type for OPFS write");
}

const supportsCreateWritable =
  typeof FileSystemFileHandle !== "undefined" && "createWritable" in FileSystemFileHandle.prototype;
const isWorkerContext = typeof window === "undefined";

async function writeSync(fileHandle, data) {
  const accessHandle = await fileHandle.createSyncAccessHandle();
  try {
    const bytes = await toUint8Array(data);
    accessHandle.truncate(0);
    accessHandle.write(bytes, { at: 0 });
    accessHandle.flush();
  } finally {
    accessHandle.close();
  }
}

let writerWorker = null;
let writerMsgId = 0;
const writerPending = new Map();

function getWriterWorker() {
  if (!writerWorker) {
    writerWorker = new Worker("opfs-writer-worker.js", { type: "module" });
    writerWorker.onmessage = (e) => {
      const { id, ok, error } = e.data;
      const pending = writerPending.get(id);
      if (!pending) return;
      writerPending.delete(id);
      if (ok) pending.resolve();
      else pending.reject(new Error(error));
    };
  }
  return writerWorker;
}

function writeViaWorker(fileHandle, data) {
  return new Promise((resolve, reject) => {
    const id = ++writerMsgId;
    writerPending.set(id, { resolve, reject });
    getWriterWorker().postMessage({ id, fileHandle, data });
  });
}

/// Writes `data` (string, Blob, ArrayBuffer, or typed array) to `fileHandle`,
/// replacing its contents. Use this instead of calling `createWritable()`
/// directly — see the module-level comment above for why.
export async function writeFile(fileHandle, data) {
  if (supportsCreateWritable) {
    const writable = await fileHandle.createWritable();
    await writable.write(data);
    await writable.close();
  } else if (isWorkerContext) {
    await writeSync(fileHandle, data);
  } else {
    await writeViaWorker(fileHandle, data);
  }
}

export async function copyFileInto(srcFileHandle, destDirHandle, destName) {
  const file = await srcFileHandle.getFile();
  const destFileHandle = await destDirHandle.getFileHandle(destName, { create: true });
  await writeFile(destFileHandle, await file.arrayBuffer());
}

export async function copyRecursive(srcHandle, destParentDirHandle, destName) {
  if (srcHandle.kind === "file") {
    await copyFileInto(srcHandle, destParentDirHandle, destName);
  } else {
    const newDir = await destParentDirHandle.getDirectoryHandle(destName, { create: true });
    for await (const [childName, childHandle] of srcHandle.entries()) {
      await copyRecursive(childHandle, newDir, childName);
    }
  }
}

export function isSubPath(candidateParentPath, candidatePath) {
  if (candidatePath.length < candidateParentPath.length) return false;
  for (let i = 0; i < candidateParentPath.length; i++) {
    if (candidatePath[i] !== candidateParentPath[i]) return false;
  }
  return true;
}

// ---------- Path-string convenience wrappers (used by agent tools) ----------

export async function getFileHandleAt(path) {
  const segments = toSegments(path);
  if (segments.length === 0) throw new Error("A file path is required.");
  const name = segments.pop();
  const dirHandle = await resolveDir(segments, false, path);
  try {
    return await dirHandle.getFileHandle(name);
  } catch (err) {
    if (err.name === "NotFoundError") throw new Error(`No such file: "${path}"`);
    throw err;
  }
}

export async function readFileText(path) {
  const fileHandle = await getFileHandleAt(path);
  const file = await fileHandle.getFile();
  return file.text();
}

/// Returns the file as a `File` (a `Blob`) rather than decoding it as text —
/// for binary content (images, etc.) headed straight for an object URL.
export async function readFileBlob(path) {
  const fileHandle = await getFileHandleAt(path);
  return fileHandle.getFile();
}

export async function writeFileText(path, content) {
  const segments = toSegments(path);
  if (segments.length === 0) throw new Error("A file path is required.");
  const name = segments.pop();
  const dirHandle = await getDirHandle(segments, true); // mkdirp parents
  const fileHandle = await dirHandle.getFileHandle(name, { create: true });
  await writeFile(fileHandle, content);
}

async function resolveDir(segments, create, originalPath) {
  try {
    return await getDirHandle(segments, create);
  } catch (err) {
    if (err.name === "NotFoundError") {
      throw new Error(`No such directory in "${originalPath}"`);
    }
    throw err;
  }
}

/// Recursively lists `path` (default: workspace root) as a flat list of
/// paths relative to it, directories suffixed with `/`. Depth-limited so a
/// pathological or huge workspace can't produce an unbounded tool result.
export async function listTree(path, maxDepth = 10) {
  const segments = toSegments(path);
  const dirHandle = await resolveDir(segments, false, path || ".");
  const lines = [];
  await walk(dirHandle, "", lines, 0, maxDepth);
  return lines;
}

async function walk(dirHandle, prefix, lines, depth, maxDepth) {
  if (depth >= maxDepth) return;
  const entries = await listEntries(dirHandle);
  for (const entry of entries) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.kind === "directory") {
      lines.push(`${rel}/`);
      await walk(entry.handle, rel, lines, depth + 1, maxDepth);
    } else {
      lines.push(rel);
    }
  }
}

// ---------- isomorphic-git: OPFS-backed fs shim ----------
//
// isomorphic-git's FileSystem wrapper already tolerates ENOENT/EEXIST in its
// higher-level mkdir/rm/rmdir helpers and retries with mkdirp on write
// failures, as long as our low-level methods report errors using Node-style
// `.code` values. So this shim only needs to implement the single-level
// primitives; it does not need to reimplement that retry logic.

function opfsError(code, message) {
  const err = new Error(message || code);
  err.code = code;
  return err;
}

function splitPosixPath(p) {
  return p.split("/").filter(Boolean);
}

async function gitResolveDir(segments, create) {
  try {
    return await getDirHandle(segments, create);
  } catch (err) {
    if (err.name === "NotFoundError") {
      throw opfsError("ENOENT", `ENOENT: no such file or directory, '/${segments.join("/")}'`);
    }
    throw err;
  }
}

function makeStatObject(isDir, size, mtimeMs) {
  return {
    mode: isDir ? 0o40000 : 0o100644,
    size,
    mtimeMs,
    ctimeMs: mtimeMs,
    uid: 1,
    gid: 1,
    dev: 1,
    ino: 0,
    isFile: () => !isDir,
    isDirectory: () => isDir,
    isSymbolicLink: () => false,
  };
}

async function gitReadFile(filepath, opts) {
  const segments = splitPosixPath(filepath);
  const name = segments.pop();
  const dirHandle = await gitResolveDir(segments, false);
  let fileHandle;
  try {
    fileHandle = await dirHandle.getFileHandle(name);
  } catch (err) {
    if (err.name === "NotFoundError") {
      throw opfsError("ENOENT", `ENOENT: no such file or directory, open '${filepath}'`);
    }
    throw err;
  }
  const file = await fileHandle.getFile();
  const encoding = opts && typeof opts === "object" ? opts.encoding : opts;
  if (encoding === "utf8" || encoding === "utf-8") return file.text();
  return new Uint8Array(await file.arrayBuffer());
}

async function gitWriteFile(filepath, data) {
  const segments = splitPosixPath(filepath);
  const name = segments.pop();
  const dirHandle = await gitResolveDir(segments, true); // mkdirp parents defensively
  const fileHandle = await dirHandle.getFileHandle(name, { create: true });
  await writeFile(fileHandle, data);
}

async function gitMkdir(dirpath) {
  await getDirHandle(splitPosixPath(dirpath), true);
}

async function gitRemoveEntry(filepath) {
  const segments = splitPosixPath(filepath);
  const name = segments.pop();
  const parent = await gitResolveDir(segments, false);
  try {
    await parent.removeEntry(name, { recursive: true });
  } catch (err) {
    if (err.name === "NotFoundError") {
      throw opfsError("ENOENT", `ENOENT: no such file or directory, '${filepath}'`);
    }
    throw err;
  }
}

async function gitReaddir(dirpath) {
  const dirHandle = await gitResolveDir(splitPosixPath(dirpath), false);
  const names = [];
  for await (const name of dirHandle.keys()) names.push(name);
  return names;
}

async function gitStat(filepath) {
  const segments = splitPosixPath(filepath);
  if (segments.length === 0) return makeStatObject(true, 0, 0);
  const name = segments.pop();
  const parent = await gitResolveDir(segments, false);
  try {
    const fileHandle = await parent.getFileHandle(name);
    const file = await fileHandle.getFile();
    return makeStatObject(false, file.size, file.lastModified);
  } catch (err) {
    if (err.name !== "NotFoundError" && err.name !== "TypeMismatchError") throw err;
  }
  try {
    await parent.getDirectoryHandle(name);
    return makeStatObject(true, 0, 0);
  } catch {
    throw opfsError("ENOENT", `ENOENT: no such file or directory, stat '${filepath}'`);
  }
}

async function gitNotSupported() {
  throw opfsError("ENOSYS", "Symlinks are not supported on OPFS");
}

export const opfsFsClient = {
  promises: {
    readFile: gitReadFile,
    writeFile: gitWriteFile,
    mkdir: gitMkdir,
    rmdir: gitRemoveEntry,
    unlink: gitRemoveEntry,
    rm: gitRemoveEntry,
    stat: gitStat,
    lstat: gitStat,
    readdir: gitReaddir,
    readlink: gitNotSupported,
    symlink: gitNotSupported,
  },
};

export const PUBLIC_GIT_CORS_PROXY = "https://cors.isomorphic-git.org";

// `cooper web` exposes a same-origin git proxy at /git-proxy (probed once via
// a bare HEAD request); when the app is served statically without it, clones
// fall back to isomorphic-git's public proxy. Cached per-context, like the
// git modules below.
let gitProxyPromise = null;

export function resolveGitProxy() {
  if (!gitProxyPromise) {
    gitProxyPromise = fetch("/git-proxy", { method: "HEAD" })
      .then((r) => (r.ok ? new URL("/git-proxy", self.location.origin).toString() : PUBLIC_GIT_CORS_PROXY))
      .catch(() => PUBLIC_GIT_CORS_PROXY);
  }
  return gitProxyPromise;
}

// isomorphic-git ships as CommonJS; esm.sh serves a browser-ready ESM build
// on the fly (transpiled, cached at the edge) so both the main-thread UI and
// the agent Worker can `import()` it without a bundler or a build step —
// same CDN-based approach the opfs/ playground used via UMD script tags.
// Loaded lazily (only the first clone pays for it) and cached per-context.
let gitModulesPromise = null;

async function loadGit() {
  if (!gitModulesPromise) {
    gitModulesPromise = Promise.all([
      import(/* @vite-ignore */ "https://esm.sh/isomorphic-git@1.27.1"),
      import(/* @vite-ignore */ "https://esm.sh/isomorphic-git@1.27.1/http/web"),
    ]);
  }
  const [git, http] = await gitModulesPromise;
  return { git, http };
}

/// Clones `url` into `destPath` (a workspace-relative POSIX path; must not
/// already exist). `onProgress`, if given, is called with isomorphic-git's
/// raw progress events (`{ phase, loaded, total? }`).
export async function gitClone({ url, destPath, branch, shallow = true, onProgress }) {
  const destSegments = toSegments(destPath);
  if (destSegments.length === 0) throw new Error("A destination folder is required.");
  const parentSegments = destSegments.slice(0, -1);
  const folderName = destSegments[destSegments.length - 1];

  const parentDirHandle = await getDirHandle(parentSegments, true);
  const existingNames = (await listEntries(parentDirHandle)).map((e) => e.name);
  if (existingNames.includes(folderName)) {
    throw new Error(`"${destPath}" already exists in the workspace.`);
  }

  const { git, http } = await loadGit();
  await git.clone({
    fs: opfsFsClient,
    http,
    dir: `/${destSegments.join("/")}`,
    url,
    ref: branch || undefined,
    corsProxy: await resolveGitProxy(),
    singleBranch: true,
    depth: shallow ? 1 : undefined,
    onProgress,
  });
}
