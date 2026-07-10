//! `cooper web`: starting the browser-app server — what it refuses to serve
//! without, and what a running instance actually answers. Handler-level
//! behavior (proxy forwarding, token exchange) is specced in src/web.rs;
//! these tests exercise the served binary end to end.

use crate::support::*;

/// A directory shaped like a built `web/` checkout: index.html plus the
/// wasm-pack output the server checks for.
fn fake_web_dir(cli: &Cli) -> std::path::PathBuf {
    let dir = cli.home().join("web");
    std::fs::create_dir_all(dir.join("www/pkg")).unwrap();
    std::fs::write(dir.join("www/index.html"), "<h1>cooper</h1>").unwrap();
    std::fs::write(dir.join("www/pkg/cooper_web.js"), "// wasm glue").unwrap();
    dir
}

#[test]
fn refuses_to_start_without_the_web_app() {
    let cli = Cli::without_config();

    let output = cli.run(&["web", "-d", "/nonexistent/web"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("web app not found"));
    assert!(stderr(&output).contains("pass --dir <path-to-web>"));
}

#[test]
fn refuses_to_start_before_the_wasm_package_is_built() {
    let cli = Cli::without_config();
    let dir = cli.home().join("web");
    std::fs::create_dir_all(dir.join("www")).unwrap();
    std::fs::write(dir.join("www/index.html"), "<h1>cooper</h1>").unwrap();

    let output = cli.run(&["web", "-d", dir.to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("wasm package not found"));
    assert!(stderr(&output).contains("wasm-pack build"));
}

#[test]
fn reports_a_port_already_in_use() {
    let cli = Cli::without_config();
    let dir = fake_web_dir(&cli);
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();

    let output = cli.run(&["web", "-d", dir.to_str().unwrap(), "-P", &port.to_string()]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains(&format!("failed to bind 127.0.0.1:{port}")));
}

#[tokio::test(flavor = "multi_thread")]
async fn binds_the_requested_host() {
    let cli = Cli::without_config();
    let dir = fake_web_dir(&cli);
    let port = free_port();
    let _server = cli.spawn(
        &[
            "web",
            "-d",
            dir.to_str().unwrap(),
            "-P",
            &port.to_string(),
            "--host",
            "0.0.0.0",
        ],
        &[],
    );
    wait_until_listening(format!("127.0.0.1:{port}").parse().unwrap());

    let index = reqwest::get(format!("http://127.0.0.1:{port}"))
        .await
        .unwrap();
    assert!(index.text().await.unwrap().contains("<h1>cooper</h1>"));
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_app_isolated_with_proxy_and_oauth_endpoints() {
    let cli = Cli::without_config();
    let dir = fake_web_dir(&cli);
    let port = free_port();
    let _server = cli.spawn(
        &["web", "-d", dir.to_str().unwrap(), "-P", &port.to_string()],
        &[
            ("GITHUB_CLIENT_ID", "id-123"),
            ("GITHUB_CLIENT_SECRET", "sec-456"),
        ],
    );
    wait_until_listening(format!("127.0.0.1:{port}").parse().unwrap());
    let base = format!("http://127.0.0.1:{port}");

    // The app is served from the root, cross-origin isolated, never cached.
    let index = reqwest::get(&base).await.unwrap();
    assert_eq!(index.headers()["cross-origin-opener-policy"], "same-origin");
    assert_eq!(
        index.headers()["cross-origin-embedder-policy"],
        "require-corp"
    );
    assert_eq!(index.headers()["cache-control"], "no-store");
    assert!(index.text().await.unwrap().contains("<h1>cooper</h1>"));

    // Legacy /www deep links (registered OAuth callbacks) keep working.
    let legacy = reqwest::get(format!("{base}/www/index.html"))
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);

    // The bare probe tells the app the same-origin git proxy is present.
    let probe = reqwest::get(format!("{base}/git-proxy")).await.unwrap();
    assert_eq!(probe.status(), 204);

    // OAuth providers configured via env are advertised, secrets withheld.
    let providers = reqwest::get(format!("{base}/oauth/providers"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(providers.contains(r#""client_id":"id-123""#));
    assert!(!providers.contains("sec-456"));
}
