//! `cooper web`: serves the browser app (`web/www` + the wasm-pack output in
//! `web/pkg`) with the headers the app needs, plus a same-origin git CORS
//! proxy so workspace cloning doesn't depend on cors.isomorphic-git.org.
//!
//! Headers that plain static servers don't send:
//!
//! - COOP/COEP: makes the page cross-origin isolated, which unlocks
//!   `SharedArrayBuffer` and therefore *multi-threaded* wllama inference —
//!   without them, local models silently run on a single core. Cross-origin
//!   fetches (Hugging Face GGUFs, remote API providers) still work because
//!   they're CORS requests, which COEP permits.
//! - `Cache-Control: no-store`: plain reloads always pick up fresh builds —
//!   no stale-wasm hunting after `wasm-pack build` (worker module imports
//!   are otherwise cached aggressively, notably by Firefox).

use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

fn static_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
}

/// Only proxy git smart-HTTP endpoints (same restriction as isomorphic-git's
/// own cors-proxy) so this doesn't double as an open proxy to arbitrary URLs.
fn is_git_request(path: &str, query: Option<&str>) -> bool {
    let info_refs = path.ends_with("/info/refs")
        && query.is_some_and(|q| q.contains("service=git-upload-pack"));
    info_refs || path.ends_with("/git-upload-pack")
}

/// Forwards `/git-proxy/{host}/{path}` to `https://{host}/{path}`, streaming
/// both bodies. The browser talks same-origin, so no CORS dance is needed;
/// hop-by-hop and browser-identity headers are stripped before forwarding.
async fn git_proxy(
    State(client): State<reqwest::Client>,
    method: Method,
    UrlPath(rest): UrlPath<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_git_request(&rest, query.as_deref()) {
        return (
            StatusCode::FORBIDDEN,
            "only git smart-HTTP requests are proxied",
        )
            .into_response();
    }

    let mut url = format!("https://{rest}");
    if let Some(q) = &query {
        url.push('?');
        url.push_str(q);
    }

    let mut forwarded = reqwest::header::HeaderMap::new();
    for (name, value) in &headers {
        // Skip headers that describe the browser connection rather than the
        // git request; reqwest recomputes host/content-length itself.
        let skip = matches!(
            name.as_str(),
            "host"
                | "origin"
                | "referer"
                | "cookie"
                | "connection"
                | "content-length"
                | "user-agent"
        ) || name.as_str().starts_with("sec-");
        if !skip {
            forwarded.insert(name.clone(), value.clone());
        }
    }
    forwarded.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static("git/cooper-proxy"),
    );

    let upstream = match client
        .request(method, &url)
        .headers(forwarded)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response();
        }
    };

    let status = upstream.status();
    let mut response_headers = HeaderMap::new();
    for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
        if let Some(value) = upstream.headers().get(&name) {
            response_headers.insert(name, value.clone());
        }
    }

    (
        status,
        response_headers,
        Body::from_stream(upstream.bytes_stream()),
    )
        .into_response()
}

/// Locates the `web/` directory to serve: an explicit `--dir`, or the
/// checkout this binary was compiled from (fine for the usual
/// build-and-run-from-the-repo workflow).
fn resolve_web_dir(dir: Option<PathBuf>) -> PathBuf {
    dir.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("web"))
}

pub async fn web_cmd(port: u16, dir: Option<PathBuf>) {
    let web_dir = resolve_web_dir(dir);

    if !web_dir.join("www/index.html").is_file() {
        eprintln!(
            "web app not found at {} (expected www/index.html); pass --dir <path-to-web>",
            web_dir.display()
        );
        std::process::exit(1);
    }
    if !web_dir.join("pkg/cooper_web.js").is_file() {
        eprintln!(
            "wasm package not found at {}; build it first:\n  wasm-pack build --target web web/",
            web_dir.join("pkg").display()
        );
        std::process::exit(1);
    }

    let client = reqwest::Client::new();
    let app = axum::Router::new()
        .route(
            "/",
            get(|| async { Redirect::temporary("/www/index.html") }),
        )
        // Bare probe used by the web app to detect the same-origin proxy.
        .route("/git-proxy", any(|| async { StatusCode::NO_CONTENT }))
        .route("/git-proxy/{*rest}", any(git_proxy))
        .with_state(client)
        .fallback_service(ServeDir::new(&web_dir))
        .layer(static_header(
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ))
        .layer(static_header(
            HeaderName::from_static("cross-origin-embedder-policy"),
            "require-corp",
        ))
        .layer(static_header(header::CACHE_CONTROL, "no-store"));

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind port {port}: {e}");
            std::process::exit(1);
        }
    };
    println!("serving http://127.0.0.1:{port}/ (cross-origin isolated, git proxy at /git-proxy)");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_refs_upload_pack_is_proxied() {
        assert!(is_git_request(
            "github.com/owner/repo.git/info/refs",
            Some("service=git-upload-pack"),
        ));
    }

    #[test]
    fn upload_pack_post_is_proxied() {
        assert!(is_git_request(
            "github.com/owner/repo.git/git-upload-pack",
            None,
        ));
    }

    #[test]
    fn receive_pack_is_rejected() {
        assert!(!is_git_request(
            "github.com/owner/repo.git/info/refs",
            Some("service=git-receive-pack"),
        ));
        assert!(!is_git_request(
            "github.com/owner/repo.git/git-receive-pack",
            None,
        ));
    }

    #[test]
    fn arbitrary_urls_are_rejected() {
        assert!(!is_git_request("example.com/anything", None));
        assert!(!is_git_request("example.com/anything", Some("foo=bar")));
    }

    #[test]
    fn explicit_dir_wins_over_default() {
        let dir = PathBuf::from("/custom/web");
        assert_eq!(resolve_web_dir(Some(dir.clone())), dir);
        assert_eq!(
            resolve_web_dir(None),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("web")
        );
    }
}
