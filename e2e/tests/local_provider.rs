//! Full local-inference smoke test: the built-in "Local (in-browser)"
//! provider, running real wllama inference on a real LFM2.5-230M GGUF
//! downloaded from Hugging Face (~153 MB, fetched fresh each run — the
//! browser profile is ephemeral, so the OPFS cache doesn't survive).
//!
//! Ignored by default because of the download size and CPU-bound wasm
//! inference time. Run explicitly with:
//!
//! ```sh
//! cargo test -p cooper-e2e --test local_provider -- --ignored --nocapture
//! ```
use std::time::Duration;

use cooper_e2e::*;
use serde_json::json;

#[tokio::test]
#[ignore = "downloads a ~153MB model from Hugging Face and runs real CPU inference"]
async fn local_provider_generates_a_response() -> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;
    let browser_handle = launch_browser().await?;

    // Overrides open_app's seeded remote provider (same localStorage key,
    // written after it) with the built-in local provider as default. No
    // mock server involved: the "base URL" below is never contacted.
    let settings = json!({
        "providers": [],
        "defaultProviderId": "local",
        "defaultModel": "lfm2.5-230m-q4",
        "localModels": [],
    });
    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        "http://unused.invalid",
        &[("cooper.settings.v1", settings)],
    )
    .await?;

    run_prompt_with_timeout(
        &page,
        "Reply with just the word hello.",
        Duration::from_secs(900),
    )
    .await?;

    let blocks = get_timeline_blocks(&page).await?;
    let response = blocks
        .iter()
        .find(|b| b.is("response"))
        .expect("expected a Response block from local inference");
    assert!(
        !response.text.trim().is_empty(),
        "local model produced an empty response"
    );

    Ok(())
}
