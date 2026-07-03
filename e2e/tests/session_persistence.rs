//! A session must survive a page reload: it's saved to IndexedDB after each
//! turn, listed in the sessions panel, and loading it replays the exact
//! same timeline the live run produced.
use cooper_e2e::*;

#[tokio::test]
async fn a_session_survives_a_page_reload_and_replays_its_timeline()
-> Result<(), Box<dyn std::error::Error>> {
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

    reload_app(&page, &static_server.base_url).await?;
    open_first_saved_session(&page).await?;

    let blocks = get_timeline_blocks(&page).await?;

    let prompt_block = blocks
        .iter()
        .find(|b| b.is("prompt"))
        .expect("expected the replayed Prompt block");
    assert!(prompt_block.text.contains("ping"));

    let response_block = blocks
        .iter()
        .find(|b| b.is("response"))
        .expect("expected the replayed Response block");
    assert!(
        response_block
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("PONG")
    );

    Ok(())
}
