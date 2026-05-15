use cooper_core::{Message, SessionLogger, Usage};
use js_sys::Date;
use serde::Serialize;
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

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionEntry {
    Request {
        session_id: String,
        messages: Vec<Message>,
        timestamp: f64,
    },
    Response {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        message: Message,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        timestamp: f64,
    },
}

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

async fn write_entry(entry: SessionEntry) {
    let json = match serde_json::to_string(&entry) {
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
    turn_start_ms: Option<f64>,
}

impl WasmSessionLogger {
    pub fn new() -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            turn_start_ms: None,
        }
    }
}

impl SessionLogger for WasmSessionLogger {
    fn on_request(&mut self, messages: &[Message]) {
        self.turn_start_ms = Some(Date::now());
        let entry = SessionEntry::Request {
            session_id: self.session_id.clone(),
            messages: messages.to_vec(),
            timestamp: Date::now(),
        };
        spawn_local(async move { write_entry(entry).await });
    }

    fn on_response(&mut self, thinking: Option<&str>, message: &Message, usage: Option<&Usage>) {
        let duration_ms = self
            .turn_start_ms
            .map(|start| (Date::now() - start) as u64)
            .unwrap_or(0);
        let entry = SessionEntry::Response {
            session_id: self.session_id.clone(),
            thinking: thinking.map(str::to_string),
            message: message.clone(),
            duration_ms,
            usage: usage.cloned(),
            timestamp: Date::now(),
        };
        spawn_local(async move { write_entry(entry).await });
    }
}
