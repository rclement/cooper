//! Dev server for the web app: `cargo run -p cooper-dev-server [-- <port>]`
//!
//! Serves `web/` (so `/www/index.html` and `/pkg/...` resolve exactly like
//! the e2e static server) with two header sets that plain static servers
//! don't send:
//!
//! - COOP/COEP: makes the page cross-origin isolated, which unlocks
//!   `SharedArrayBuffer` and therefore *multi-threaded* wllama inference —
//!   without them, local models silently run on a single core. Cross-origin
//!   fetches (Hugging Face GGUFs, remote API providers) still work because
//!   they're CORS requests, which COEP permits.
//! - `Cache-Control: no-store`: plain reloads always pick up fresh builds —
//!   no stale-wasm hunting after `wasm-pack build` (worker module imports
//!   are otherwise cached aggressively, notably by Firefox).

use std::path::Path;

use axum::http::{HeaderName, HeaderValue, header};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

fn static_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .map(|p| p.parse().expect("port must be a number"))
        .unwrap_or(8080);

    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("dev-server crate has a parent directory")
        .join("web");

    let app = axum::Router::new()
        .fallback_service(ServeDir::new(web_dir))
        .layer(static_header(
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ))
        .layer(static_header(
            HeaderName::from_static("cross-origin-embedder-policy"),
            "require-corp",
        ))
        .layer(static_header(header::CACHE_CONTROL, "no-store"));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("failed to bind dev server port");
    println!("serving http://127.0.0.1:{port}/www/index.html (cross-origin isolated, no-store)");
    axum::serve(listener, app).await.expect("dev server failed");
}
