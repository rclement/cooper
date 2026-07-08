// Dedicated worker used as a fallback OPFS write path for browsers (Safari)
// that implement `createSyncAccessHandle()` but not the async
// `createWritable()` API — and, per spec, only allow the sync access handle
// to be opened from inside a Worker, never the main thread. The main thread
// resolves/creates the FileSystemFileHandle as usual (that part works
// everywhere) and posts it here, since FileSystemHandle objects are
// structured-cloneable. See `writeFile()` in workspace-fs.js, the only
// caller of this worker.
import { toUint8Array } from "./workspace-fs.js";

self.onmessage = async (e) => {
  const { id, fileHandle, data } = e.data;
  try {
    const accessHandle = await fileHandle.createSyncAccessHandle();
    try {
      const bytes = await toUint8Array(data);
      accessHandle.truncate(0);
      accessHandle.write(bytes, { at: 0 });
      accessHandle.flush();
    } finally {
      accessHandle.close();
    }
    self.postMessage({ id, ok: true });
  } catch (err) {
    self.postMessage({ id, ok: false, error: err && err.message ? err.message : String(err) });
  }
};
