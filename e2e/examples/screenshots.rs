//! Generates the README screenshots into `docs/screenshots/`, reusing the
//! e2e harness: the real wasm build driven by headless Chromium, with the
//! in-process mock server scripting deterministic model responses (the tool
//! calls themselves — Pyodide, chart rendering — run for real in the
//! browser, so what's pictured is what the app actually does).
//!
//! ```sh
//! wasm-pack build --target web --out-dir www/pkg web/   # if not already built
//! cargo run -p cooper-e2e --example screenshots
//! ```

use std::time::Duration;

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use cooper_e2e::*;

const WIDTH: i64 = 1280;
const HEIGHT: i64 = 800;

async fn set_viewport(page: &Page) -> Result<(), Box<dyn std::error::Error>> {
    page.execute(
        SetDeviceMetricsOverrideParams::builder()
            .width(WIDTH)
            .height(HEIGHT)
            .device_scale_factor(2.0)
            .mobile(false)
            .build()?,
    )
    .await?;
    Ok(())
}

async fn save(page: &Page, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Let any in-flight rendering (charts, markdown) settle first.
    tokio::time::sleep(Duration::from_millis(500)).await;
    page.save_screenshot(
        ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .build(),
        path,
    )
    .await?;
    println!("wrote {}", path.display());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let out_dir = repo_root().join("docs/screenshots");
    std::fs::create_dir_all(&out_dir)?;

    let static_server = start_static_server().await?;
    let browser_handle = launch_browser().await?;
    let browser = &browser_handle.browser;

    // --- 1. Agent run: reasoning + run_python (real Pyodide) + response ---
    let fixture = r#"
responses:
  - reasoning: "A sieve of Eratosthenes up to 10,000 gives both answers in one pass."
    tool_calls:
      - id: call-1
        name: run_python
        arguments:
          code: |-
            n = 10_000
            sieve = [True] * n
            sieve[0] = sieve[1] = False
            for i in range(2, int(n ** 0.5) + 1):
                if sieve[i]:
                    sieve[i * i::i] = [False] * len(sieve[i * i::i])
            primes = [i for i, is_p in enumerate(sieve) if is_p]
            gap, lo = max((b - a, a) for a, b in zip(primes, primes[1:]))
            (len(primes), gap, lo, lo + gap)
    usage: { prompt_tokens: 120, completion_tokens: 90, total_tokens: 210 }
  - text: |-
      There are **1,229** primes below 10,000.

      The largest gap between consecutive primes in that range is **36**, between 9,551 and 9,587.
    usage: { prompt_tokens: 260, completion_tokens: 42, total_tokens: 302 }
"#
    .to_string();
    let mock_server = start_mock_server(MockFixture::Yaml(fixture)).await?;
    let page = open_app(browser, &static_server.base_url, &mock_server.base_url, &[]).await?;
    set_viewport(&page).await?;
    run_prompt_with_timeout(
        &page,
        "How many primes are there below 10,000, and what's the largest gap between consecutive ones?",
        Duration::from_secs(120), // room for Pyodide to boot
    )
    .await?;
    save(&page, &out_dir.join("agent-run.png")).await?;
    drop(mock_server);

    // --- 2. Inline chart: render_chart drawn client-side from the call ---
    let data = r#"[{"month":"Jan","revenue":48.2},{"month":"Feb","revenue":52.7},{"month":"Mar","revenue":61.9},{"month":"Apr","revenue":58.4},{"month":"May","revenue":67.3},{"month":"Jun","revenue":74.1}]"#;
    let spec = r#"{"type":"bar","x":"month","y":"revenue","title":"Revenue by month (H1 2026)","y_label":"$k"}"#;
    let fixture = format!(
        r#"
responses:
  - tool_calls:
      - id: call-1
        name: render_chart
        arguments:
          data: '{data}'
          spec: '{spec}'
    usage: {{ prompt_tokens: 150, completion_tokens: 80, total_tokens: 230 }}
  - text: "Revenue grew from $48.2k in January to $74.1k in June — up 54% over the half, with April the only down month."
    usage: {{ prompt_tokens: 280, completion_tokens: 35, total_tokens: 315 }}
"#
    );
    let mock_server = start_mock_server(MockFixture::Yaml(fixture)).await?;
    let page = open_app(browser, &static_server.base_url, &mock_server.base_url, &[]).await?;
    set_viewport(&page).await?;
    run_prompt(
        &page,
        "Chart our monthly revenue for the first half of 2026.",
    )
    .await?;
    save(&page, &out_dir.join("chart.png")).await?;
    drop(mock_server);

    // --- 3. Settings: provider CRUD + the local (in-browser) model catalog ---
    let mock_server = start_mock_server(MockFixture::File("simple_reply.yaml")).await?;
    let page = open_app(browser, &static_server.base_url, &mock_server.base_url, &[]).await?;
    set_viewport(&page).await?;
    page.evaluate("document.querySelector('[data-view=\"settings\"]').click()")
        .await?;
    save(&page, &out_dir.join("settings.png")).await?;

    Ok(())
}
