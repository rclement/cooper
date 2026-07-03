//! Shared plumbing for the e2e suite: runs the real `cooper-mock-server`
//! in-process (scripted OpenAI-compatible SSE responses) and a small static
//! file server for `web/` (so `pkg/` and `www/` are reachable exactly as in
//! real usage), then drives the app with a real headless Chromium via
//! `chromiumoxide` (talks directly to the Chrome DevTools Protocol — no
//! Node.js anywhere in this crate or its dependency graph).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chromiumoxide::{Browser, BrowserConfig, Page};
use cooper_mock_server::Fixture;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("e2e crate has a parent directory")
        .to_path_buf()
}

/// Fails fast with a clear message instead of an opaque browser/network
/// error if the wasm build hasn't been produced yet.
pub fn assert_prerequisites_built() -> Result<(), Box<dyn std::error::Error>> {
    let pkg_marker = repo_root().join("web/pkg/cooper_web.js");
    if !pkg_marker.exists() {
        return Err(format!(
            "wasm build not found at {} — run `wasm-pack build --target web` in web/ first.",
            pkg_marker.display()
        )
        .into());
    }
    Ok(())
}

async fn free_port() -> Result<u16, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    Ok(listener.local_addr()?.port())
}

async fn wait_for_ready(
    addr: SocketAddr,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(format!("server at {addr} did not become ready within {timeout:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub enum MockFixture {
    /// Name of a file under `mock-server/fixtures/`.
    File(&'static str),
    /// Raw YAML content, for fixtures that need a runtime-computed value
    /// (e.g. a URL pointing at the static server's dynamically-chosen port).
    Yaml(String),
}

pub struct MockServer {
    pub base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Runs `cooper-mock-server` in-process (via its library `run` entry point,
/// not a spawned subprocess) on a freshly allocated port.
pub async fn start_mock_server(
    fixture: MockFixture,
) -> Result<MockServer, Box<dyn std::error::Error>> {
    let fixture = match fixture {
        MockFixture::File(name) => {
            let path = repo_root().join("mock-server/fixtures").join(name);
            Fixture::load(&path)?
        }
        MockFixture::Yaml(yaml) => Fixture::from_yaml_str(&yaml)?,
    };

    let port = free_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let handle = tokio::spawn(async move {
        if let Err(e) = cooper_mock_server::run(fixture, addr).await {
            eprintln!("mock server error: {e}");
        }
    });

    wait_for_ready(addr, Duration::from_secs(5)).await?;

    Ok(MockServer {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        handle,
    })
}

pub struct StaticServer {
    pub base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Serves `web/` (so `pkg/` and `www/` are reachable as siblings, matching
/// how worker.js imports `../pkg/cooper_web.js`).
pub async fn start_static_server() -> Result<StaticServer, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    let app = axum::Router::new()
        .fallback_service(tower_http::services::ServeDir::new(repo_root().join("web")));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    wait_for_ready(addr, Duration::from_secs(5)).await?;

    Ok(StaticServer {
        base_url: format!("http://127.0.0.1:{}", addr.port()),
        handle,
    })
}

pub struct BrowserHandle {
    pub browser: Browser,
    handler_task: tokio::task::JoinHandle<()>,
}

impl Drop for BrowserHandle {
    fn drop(&mut self) {
        self.handler_task.abort();
    }
}

pub async fn launch_browser() -> Result<BrowserHandle, Box<dyn std::error::Error>> {
    let mut builder = BrowserConfig::builder().no_sandbox();
    // Locally, chromiumoxide's own detection finds a system Chrome/Chromium
    // install fine. CI provisions one at a less predictable path, so it sets
    // this env var to point at exactly the binary it installed.
    if let Ok(path) = std::env::var("CHROMIUM_EXECUTABLE") {
        builder = builder.chrome_executable(path);
    }
    let config = builder.build()?;
    let (browser, mut handler) = Browser::launch(config).await?;
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    Ok(BrowserHandle {
        browser,
        handler_task,
    })
}

/// Opens the app pointed at `mock_base_url` as its sole configured provider,
/// with any extra localStorage entries (e.g. `cooper.context.v1`) pre-seeded.
pub async fn open_app(
    browser: &Browser,
    static_base_url: &str,
    mock_base_url: &str,
    extra_storage: &[(&str, serde_json::Value)],
) -> Result<Page, Box<dyn std::error::Error>> {
    let index_url = format!("{static_base_url}/www/index.html");
    let page = browser.new_page(index_url.clone()).await?;

    let settings = json!({
        "providers": [{
            "id": "p1",
            "name": "Mock",
            "baseUrl": mock_base_url,
            "apiKey": "x",
            "models": ["mock-model"],
        }],
        "defaultProviderId": "p1",
        "defaultModel": "mock-model",
    });

    let mut js = format!(
        "localStorage.clear(); localStorage.setItem('cooper.settings.v1', {});",
        serde_json::to_string(&settings.to_string())?
    );
    for (key, value) in extra_storage {
        js.push_str(&format!(
            "localStorage.setItem({}, {});",
            serde_json::to_string(key)?,
            serde_json::to_string(&value.to_string())?
        ));
    }
    page.evaluate(js).await?;

    page.goto(index_url).await?;
    Ok(page)
}

/// Sets the prompt textarea, clicks Run, and waits for the run to finish
/// (status becomes "done"), returning an error if it errors out or times out.
pub async fn run_prompt(page: &Page, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
    let js = format!(
        "document.getElementById('prompt').value = {}; document.getElementById('run').click();",
        serde_json::to_string(prompt)?
    );
    page.evaluate(js).await?;

    let start = Instant::now();
    let timeout = Duration::from_secs(15);
    loop {
        let status: String = page
            .evaluate("document.getElementById('status')?.textContent ?? ''")
            .await?
            .into_value()?;
        if status == "done" {
            return Ok(());
        }
        if status.starts_with("error") {
            return Err(format!("run did not complete successfully: {status}").into());
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "timed out waiting for the run to finish (last status: {status:?})"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Deserialize)]
pub struct TimelineBlock {
    pub class_name: String,
    pub text: String,
    pub body_html: Option<String>,
}

impl TimelineBlock {
    pub fn is(&self, kind: &str) -> bool {
        self.class_name.contains(&format!("block-{kind}"))
    }
}

pub async fn get_timeline_blocks(
    page: &Page,
) -> Result<Vec<TimelineBlock>, Box<dyn std::error::Error>> {
    let js = "Array.from(document.querySelectorAll('.timeline > *')).map(el => ({ \
        class_name: el.className, \
        text: el.textContent, \
        body_html: el.querySelector('.block-body')?.innerHTML ?? null, \
    }))";
    Ok(page.evaluate(js).await?.into_value()?)
}

pub async fn is_tool_enabled(
    page: &Page,
    row_index: usize,
) -> Result<bool, Box<dyn std::error::Error>> {
    let js = format!("document.querySelectorAll('.tool-row input')[{row_index}]?.checked ?? false");
    Ok(page.evaluate(js).await?.into_value()?)
}
