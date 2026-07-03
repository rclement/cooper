//! Persisted sessions store the core agent's `Vec<Message>` history (one
//! entry per system/user/assistant/tool message), not a per-SSE-chunk event
//! log. A response streamed as 25+ small deltas (mock-server splits text
//! into 8-char chunks) must still persist as exactly 3 messages: system,
//! user, assistant — never one per chunk. This is what a previous,
//! since-removed design (a separately maintained live-event log) got wrong.
use cooper_e2e::*;

#[tokio::test]
async fn a_streamed_reply_persists_as_one_message_regardless_of_chunk_count()
-> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;
    // 25+ SSE deltas for both reasoning and response text.
    let long_text = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis nostrud exercitation ullamco laboris";
    let fixture_yaml = format!(
        r#"
responses:
  - reasoning: "{long_text}"
    text: "{long_text}"
    usage:
      prompt_tokens: 40
      completion_tokens: 200
      total_tokens: 240
"#
    );
    let mock_server = start_mock_server(MockFixture::Yaml(fixture_yaml)).await?;
    let browser_handle = launch_browser().await?;

    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        &mock_server.base_url,
        &[],
    )
    .await?;
    run_prompt(&page, "ping").await?;

    // Give the "done" handler's saveSession() a moment to complete.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let lengths = get_saved_session_history_lengths(&page).await?;
    assert_eq!(lengths.len(), 1, "expected exactly 1 saved session");
    // System + User + Assistant == 3, regardless of how many SSE deltas
    // made up the reasoning/response text.
    assert_eq!(
        lengths[0], 3,
        "expected the persisted history to be message-level (3 entries), got {}",
        lengths[0]
    );

    Ok(())
}
