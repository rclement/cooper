use cooper_core::{SessionEvent, SessionLogger};
use js_sys::Date;
use serde_json::json;
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    IdbDatabase, IdbObjectStoreParameters, IdbOpenDbRequest, IdbTransactionMode,
    IdbVersionChangeEvent,
};

const DB_NAME: &str = "cooper_sessions";
const DB_VERSION: u32 = 1;
const STORE_NAME: &str = "entries";

async fn open_db() -> Result<IdbDatabase, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let idb = window
        .indexed_db()?
        .ok_or_else(|| JsValue::from_str("indexeddb unavailable"))?;
    let req = idb.open_with_u32(DB_NAME, DB_VERSION)?;

    // Runs synchronously the first time the DB is created; creates the object store.
    let on_upgrade = Closure::once(|evt: IdbVersionChangeEvent| {
        if let Some(target) = evt.target() {
            if let Ok(req) = target.dyn_into::<IdbOpenDbRequest>() {
                if let Ok(result) = req.result() {
                    if let Ok(db) = result.dyn_into::<IdbDatabase>() {
                        let params = IdbObjectStoreParameters::new();
                        params.set_auto_increment(true);
                        let _ =
                            db.create_object_store_with_optional_parameters(STORE_NAME, &params);
                    }
                }
            }
        }
    });
    req.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
    on_upgrade.forget();

    // Wrap onsuccess / onerror in a Promise so we can .await the open.
    let req2 = req.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let req_ref = req2.clone();
        let reject_clone = reject.clone();
        let on_success = Closure::once(move |_: web_sys::Event| match req_ref.result() {
            Ok(db) => {
                let _ = resolve.call1(&JsValue::UNDEFINED, &db);
            }
            Err(e) => {
                let _ = reject_clone.call1(&JsValue::UNDEFINED, &e);
            }
        });
        req2.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        on_success.forget();

        let on_error = Closure::once(move |_: web_sys::Event| {
            let _ = reject.call1(&JsValue::UNDEFINED, &JsValue::from_str("idb open failed"));
        });
        req2.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
    });

    JsFuture::from(promise).await?.dyn_into()
}

/// Serialize a `SessionEvent` enriched with `session_id` and `timestamp` fields,
/// then write the resulting JSON object to IndexedDB.
async fn write_entry(session_id: String, event: SessionEvent) {
    let mut val = match serde_json::to_value(&event) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(obj) = val.as_object_mut() {
        obj.insert("session_id".into(), json!(session_id));
        obj.insert("timestamp".into(), json!(Date::now()));
    }
    let json = match serde_json::to_string(&val) {
        Ok(s) => s,
        Err(_) => return,
    };
    let db = match open_db().await {
        Ok(db) => db,
        Err(_) => return,
    };
    let tx = match db.transaction_with_str_and_mode(STORE_NAME, IdbTransactionMode::Readwrite) {
        Ok(tx) => tx,
        Err(_) => return,
    };
    let store = match tx.object_store(STORE_NAME) {
        Ok(s) => s,
        Err(_) => return,
    };
    if let Ok(js_val) = js_sys::JSON::parse(&json) {
        let _ = store.add(&js_val);
    }
}

pub struct WasmSessionLogger {
    session_id: String,
}

impl WasmSessionLogger {
    pub fn new(provider: &str, model: &str) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let logger = Self { session_id: session_id.clone() };
        let started_at = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        let event = SessionEvent::SessionStart {
            id: session_id.clone(),
            provider: provider.to_string(),
            model: model.to_string(),
            project: String::new(),
            started_at,
        };
        spawn_local(async move { write_entry(session_id, event).await });
        logger
    }
}

impl SessionLogger for WasmSessionLogger {
    fn on_event(&mut self, event: &SessionEvent) {
        let session_id = self.session_id.clone();
        let event = event.clone();
        spawn_local(async move { write_entry(session_id, event).await });
    }
}
