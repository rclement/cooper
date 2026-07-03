//! Golden path: type a prompt, run it, get a rendered response and usage —
//! the single most basic thing the app has to do correctly.
use cooper_e2e::*;

#[tokio::test]
async fn running_a_prompt_renders_response_and_usage() -> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;
    let mock_server = start_mock_server(MockFixture::File("simple_reply.yaml")).await?;
    let browser_handle = launch_browser().await?;

    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        &mock_server.base_url,
        &[],
    )
    .await?;
    run_prompt(&page, "ping").await?;

    let blocks = get_timeline_blocks(&page).await?;

    let response = blocks
        .iter()
        .find(|b| b.is("response"))
        .expect("expected a Response block");
    assert!(
        response
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("PONG")
    );

    let usage = blocks
        .iter()
        .find(|b| b.is("usage"))
        .expect("expected a usage pill");
    assert!(usage.text.contains("45 tokens"));
    assert!(usage.text.contains("42 prompt"));
    assert!(usage.text.contains("3 completion"));

    Ok(())
}
