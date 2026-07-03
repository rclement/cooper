// Session persistence: stores each conversation's metadata plus its
// exported `Vec<Message>` history (see WasmAgent::export_history) in
// IndexedDB, so sessions survive a page reload. localStorage isn't used
// here — a session's full message history (system prompt, every turn's
// text/reasoning/tool calls) can get large.
const DB_NAME = "cooper-sessions";
const DB_VERSION = 1;
const STORE = "sessions";

let dbPromise = null;

function openDb() {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      request.result.createObjectStore(STORE, { keyPath: "id" });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  return dbPromise;
}

async function withStore(mode, fn) {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, mode);
    const store = tx.objectStore(STORE);
    const result = fn(store);
    tx.oncomplete = () => resolve(result);
    tx.onerror = () => reject(tx.error);
  });
}

function requestToPromise(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/// `session` shape: { id, title, createdAt, updatedAt, providerName,
/// providerId, model, history: <wasm-exported JSON string, or null if the
/// session hasn't completed a turn yet> }
export async function saveSession(session) {
  await withStore("readwrite", (store) => store.put(session));
}

/// Returns sessions sorted most-recently-updated first.
export async function listSessions() {
  const sessions = await withStore("readonly", (store) =>
    requestToPromise(store.getAll()),
  );
  return sessions.sort((a, b) => b.updatedAt - a.updatedAt);
}

export async function deleteSession(id) {
  await withStore("readwrite", (store) => store.delete(id));
}
