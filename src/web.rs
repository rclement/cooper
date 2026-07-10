//! `cooper web`: serves the browser app (`web/www`, wasm-pack output
//! included at `web/www/pkg`) with the headers the app needs, plus a
//! same-origin git CORS proxy so workspace cloning doesn't depend on
//! cors.isomorphic-git.org.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

fn static_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
}

/// Where forwarded requests go. Production talks to the real internet
/// (`https` git hosts, GitHub's token endpoint); tests substitute local
/// plain-HTTP upstreams so upstream behavior can be scripted. A provider
/// appears in `oauth_token_urls` when the server knows where its
/// code-for-token exchange lives — adding one entry here (plus the
/// browser's `GIT_PROVIDERS`) is all that's needed to support it.
#[derive(Clone)]
struct Upstreams {
    client: reqwest::Client,
    git_scheme: &'static str,
    oauth_token_urls: HashMap<&'static str, String>,
}

impl Upstreams {
    fn real() -> Self {
        Upstreams {
            client: reqwest::Client::new(),
            git_scheme: "https",
            oauth_token_urls: HashMap::from([(
                "github",
                "https://github.com/login/oauth/access_token".to_string(),
            )]),
        }
    }
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
    State(upstreams): State<Upstreams>,
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

    let mut url = format!("{}://{rest}", upstreams.git_scheme);
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

    let upstream = match upstreams
        .client
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

// ---------- Git provider OAuth ----------
//
// The browser app runs the user-visible part of the OAuth dance (authorize
// redirect, state check, token storage); the server's only job is the step
// that must not happen client-side — exchanging the authorization code using
// the client secret. Credentials come from env vars per provider
// (GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET, GITLAB_CLIENT_ID/..., …); a
// provider is "configured" when both are set. Tokens are returned to the
// browser and never stored server-side.

fn oauth_env_vars(provider: &str) -> (String, String) {
    let prefix = provider.to_uppercase();
    (
        format!("{prefix}_CLIENT_ID"),
        format!("{prefix}_CLIENT_SECRET"),
    )
}

/// Client id + secret for `provider`, if both env vars are set and non-empty.
fn oauth_credentials(provider: &str) -> Option<(String, String)> {
    let (id_var, secret_var) = oauth_env_vars(provider);
    let id = std::env::var(id_var).ok().filter(|v| !v.is_empty())?;
    let secret = std::env::var(secret_var).ok().filter(|v| !v.is_empty())?;
    Some((id, secret))
}

/// `GET /oauth/providers`: which providers this server can complete the
/// token exchange for, with the public client id the browser needs to build
/// the authorize URL. Secrets never leave the server.
async fn oauth_providers(State(upstreams): State<Upstreams>) -> Json<serde_json::Value> {
    let mut providers = serde_json::Map::new();
    for name in upstreams.oauth_token_urls.keys() {
        if let Some((client_id, _)) = oauth_credentials(name) {
            providers.insert(
                name.to_string(),
                serde_json::json!({ "client_id": client_id }),
            );
        }
    }
    Json(serde_json::Value::Object(providers))
}

#[derive(serde::Deserialize)]
struct TokenExchangeRequest {
    code: String,
    redirect_uri: Option<String>,
}

/// `POST /oauth/{provider}/token`: exchanges an authorization code for an
/// access token, adding the client secret server-side. The upstream JSON is
/// relayed verbatim — GitHub reports failures (expired code, bad client)
/// as a 200 with an `error` field, which the browser handles.
async fn oauth_token(
    State(upstreams): State<Upstreams>,
    UrlPath(provider): UrlPath<String>,
    Json(request): Json<TokenExchangeRequest>,
) -> Response {
    let Some(token_url) = upstreams.oauth_token_urls.get(provider.as_str()) else {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown provider: {provider}"),
        )
            .into_response();
    };
    let Some((client_id, client_secret)) = oauth_credentials(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            format!("provider not configured: {provider} (set client id/secret env vars)"),
        )
            .into_response();
    };

    let body = serde_json::json!({
        "client_id": client_id,
        "client_secret": client_secret,
        "code": request.code,
        "redirect_uri": request.redirect_uri,
    });

    let upstream = match upstreams
        .client
        .post(token_url)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response();
        }
    };

    let status = upstream.status();
    let payload = upstream.bytes().await.unwrap_or_default();
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        payload,
    )
        .into_response()
}

/// Locates the `web/` directory to serve: an explicit `--dir`, or the
/// checkout this binary was compiled from (fine for the usual
/// build-and-run-from-the-repo workflow).
fn resolve_web_dir(dir: Option<PathBuf>) -> PathBuf {
    dir.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("web"))
}

fn app(web_dir: &Path, upstreams: Upstreams) -> axum::Router {
    axum::Router::new()
        // Bare probe used by the web app to detect the same-origin proxy.
        .route("/git-proxy", any(|| async { StatusCode::NO_CONTENT }))
        .route("/git-proxy/{*rest}", any(git_proxy))
        .route("/oauth/providers", get(oauth_providers))
        .route("/oauth/{provider}/token", post(oauth_token))
        .with_state(upstreams)
        // The app lives at the root (`/` serves www/index.html — ServeDir
        // appends index.html on directory requests; www/ is self-contained,
        // wasm pkg included). The legacy `/www/*` mount keeps pre-existing
        // deep links working — notably OAuth callback URLs registered as
        // /www/oauth-callback.html.
        .nest_service("/www", ServeDir::new(web_dir.join("www")))
        .fallback_service(ServeDir::new(web_dir.join("www")))
        .layer(static_header(
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ))
        .layer(static_header(
            HeaderName::from_static("cross-origin-embedder-policy"),
            "require-corp",
        ))
        .layer(static_header(header::CACHE_CONTROL, "no-store"))
}

pub async fn web_cmd(host: String, port: u16, dir: Option<PathBuf>) {
    let web_dir = resolve_web_dir(dir);

    if !web_dir.join("www/index.html").is_file() {
        eprintln!(
            "web app not found at {} (expected www/index.html); pass --dir <path-to-web>",
            web_dir.display()
        );
        std::process::exit(1);
    }
    if !web_dir.join("www/pkg/cooper_web.js").is_file() {
        eprintln!(
            "wasm package not found at {}; build it first:\n  wasm-pack build --target web --out-dir www/pkg web/",
            web_dir.join("www/pkg").display()
        );
        std::process::exit(1);
    }

    let app = app(&web_dir, Upstreams::real());

    let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {host}:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("serving http://{host}:{port}/ (cross-origin isolated, git proxy at /git-proxy)");
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
    fn oauth_env_vars_follow_provider_name() {
        assert_eq!(
            oauth_env_vars("github"),
            ("GITHUB_CLIENT_ID".into(), "GITHUB_CLIENT_SECRET".into())
        );
        assert_eq!(
            oauth_env_vars("gitlab"),
            ("GITLAB_CLIENT_ID".into(), "GITLAB_CLIENT_SECRET".into())
        );
    }

    #[test]
    fn only_known_providers_have_token_urls() {
        let urls = Upstreams::real().oauth_token_urls;

        assert!(urls.contains_key("github"));
        assert!(!urls.contains_key("bitbucket"));
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

    // ---------- Serving the routes for real ----------
    //
    // These tests bind the actual router on a loopback port and script the
    // *upstream* side (the git host, GitHub's token endpoint) with local
    // plain-HTTP servers, so the full forwarding behavior — what gets
    // stripped, what gets added, what gets relayed back — is observable.

    use std::sync::{Arc, Mutex};

    use crate::test_support::EnvVarsGuard;

    /// Binds `router` on a random loopback port; returns its `host:port`.
    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr.to_string()
    }

    /// One request as the scripted upstream saw it.
    #[derive(Clone)]
    struct SeenRequest {
        headers: HeaderMap,
        body: String,
    }

    type Seen = Arc<Mutex<Vec<SeenRequest>>>;

    /// An upstream that records every request and answers `response_body`
    /// with the given content type.
    async fn scripted_upstream(
        content_type: &'static str,
        response_body: &'static str,
    ) -> (String, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let router = axum::Router::new().route(
            "/{*rest}",
            any(move |headers: HeaderMap, body: Bytes| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().unwrap().push(SeenRequest {
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    });
                    ([(header::CONTENT_TYPE, content_type)], response_body)
                }
            }),
        );
        (serve(router).await, seen)
    }

    /// The router under test, with upstreams pointed at plain HTTP and
    /// GitHub's token endpoint replaced by `token_url`.
    async fn serve_app(token_url: &str) -> String {
        let upstreams = Upstreams {
            client: reqwest::Client::new(),
            git_scheme: "http",
            oauth_token_urls: HashMap::from([("github", token_url.to_string())]),
        };
        serve(app(Path::new("/nonexistent-web-dir"), upstreams)).await
    }

    #[tokio::test]
    async fn git_proxy_relays_a_clone_handshake_and_its_response() {
        let (upstream, _) = scripted_upstream(
            "application/x-git-upload-pack-advertisement",
            "refs-payload",
        )
        .await;
        let proxy = serve_app("http://unused").await;

        let response = reqwest::get(format!(
            "http://{proxy}/git-proxy/{upstream}/owner/repo.git/info/refs?service=git-upload-pack"
        ))
        .await
        .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-git-upload-pack-advertisement"
        );
        assert_eq!(response.text().await.unwrap(), "refs-payload");
    }

    #[tokio::test]
    async fn git_proxy_speaks_as_git_not_as_the_browser() {
        let (upstream, seen) =
            scripted_upstream("application/x-git-upload-pack-result", "pack").await;
        let proxy = serve_app("http://unused").await;

        reqwest::Client::new()
            .post(format!(
                "http://{proxy}/git-proxy/{upstream}/owner/repo.git/git-upload-pack"
            ))
            .header("cookie", "session=browser-secret")
            .header("origin", "http://127.0.0.1:8080")
            .header("sec-fetch-mode", "cors")
            .header("authorization", "Basic Z2l0OnRva2Vu")
            .body("0000want")
            .send()
            .await
            .unwrap();

        let request = seen.lock().unwrap()[0].clone();
        assert!(request.headers.get("cookie").is_none());
        assert!(request.headers.get("origin").is_none());
        assert!(request.headers.get("sec-fetch-mode").is_none());
        assert_eq!(request.headers["user-agent"], "git/cooper-proxy");
        assert_eq!(request.headers["authorization"], "Basic Z2l0OnRva2Vu");
        assert_eq!(request.body, "0000want");
    }

    #[tokio::test]
    async fn git_proxy_refuses_to_be_an_open_proxy() {
        let proxy = serve_app("http://unused").await;

        let response = reqwest::get(format!("http://{proxy}/git-proxy/example.com/anything"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn git_proxy_reports_an_unreachable_host_as_bad_gateway() {
        let proxy = serve_app("http://unused").await;

        let response = reqwest::Client::new()
            .post(format!(
                "http://{proxy}/git-proxy/127.0.0.1:1/owner/repo.git/git-upload-pack"
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn every_response_carries_isolation_and_no_store_headers() {
        let proxy = serve_app("http://unused").await;

        let response = reqwest::get(format!("http://{proxy}/git-proxy"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()["cross-origin-opener-policy"],
            "same-origin"
        );
        assert_eq!(
            response.headers()["cross-origin-embedder-policy"],
            "require-corp"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
    }

    #[tokio::test]
    async fn token_exchange_adds_the_client_secret_server_side() {
        let (upstream, seen) =
            scripted_upstream("application/json", r#"{"access_token":"tok-789"}"#).await;
        let proxy = serve_app(&format!("http://{upstream}/login/oauth/access_token")).await;
        let _env = EnvVarsGuard::set(&[
            ("GITHUB_CLIENT_ID", "id-123"),
            ("GITHUB_CLIENT_SECRET", "sec-456"),
        ]);

        let response = reqwest::Client::new()
            .post(format!("http://{proxy}/oauth/github/token"))
            .json(&serde_json::json!({ "code": "abc", "redirect_uri": "http://cb" }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            response.text().await.unwrap(),
            r#"{"access_token":"tok-789"}"#
        );
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.body.contains("\"code\":\"abc\""));
        assert!(request.body.contains("\"client_id\":\"id-123\""));
        assert!(request.body.contains("\"client_secret\":\"sec-456\""));
    }

    #[tokio::test]
    async fn token_exchange_rejects_unknown_providers() {
        let proxy = serve_app("http://unused").await;

        let response = reqwest::Client::new()
            .post(format!("http://{proxy}/oauth/bitbucket/token"))
            .json(&serde_json::json!({ "code": "abc" }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_exchange_rejects_providers_without_credentials() {
        let proxy = serve_app("http://unused").await;
        let _env = EnvVarsGuard::set(&[("GITHUB_CLIENT_ID", ""), ("GITHUB_CLIENT_SECRET", "")]);

        let response = reqwest::Client::new()
            .post(format!("http://{proxy}/oauth/github/token"))
            .json(&serde_json::json!({ "code": "abc" }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_exchange_reports_an_unreachable_endpoint_as_bad_gateway() {
        let proxy = serve_app("http://127.0.0.1:1/token").await;
        let _env = EnvVarsGuard::set(&[
            ("GITHUB_CLIENT_ID", "id-123"),
            ("GITHUB_CLIENT_SECRET", "sec-456"),
        ]);

        let response = reqwest::Client::new()
            .post(format!("http://{proxy}/oauth/github/token"))
            .json(&serde_json::json!({ "code": "abc" }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn configured_providers_are_advertised_with_their_public_id_only() {
        let proxy = serve_app("http://unused").await;
        let _env = EnvVarsGuard::set(&[
            ("GITHUB_CLIENT_ID", "id-123"),
            ("GITHUB_CLIENT_SECRET", "sec-456"),
        ]);

        let body = reqwest::get(format!("http://{proxy}/oauth/providers"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(body.contains(r#""client_id":"id-123""#));
        assert!(!body.contains("sec-456"));
    }

    #[tokio::test]
    async fn unconfigured_providers_are_not_advertised() {
        let proxy = serve_app("http://unused").await;
        let _env = EnvVarsGuard::set(&[("GITHUB_CLIENT_ID", ""), ("GITHUB_CLIENT_SECRET", "")]);

        let body = reqwest::get(format!("http://{proxy}/oauth/providers"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(body, "{}");
    }
}
