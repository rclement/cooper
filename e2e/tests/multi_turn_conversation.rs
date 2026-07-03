//! Two prompts in the same session (no "New session" click in between) must
//! share conversation history: the system prompt is only reported once, and
//! the timeline accumulates both turns' Prompt/Response blocks instead of
//! the second Run wiping out the first.
use cooper_e2e::*;

#[tokio::test]
async fn a_follow_up_prompt_continues_the_same_session() -> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;
    let fixture_yaml = r#"
responses:
  - text: "first answer"
    usage:
      prompt_tokens: 40
      completion_tokens: 3
      total_tokens: 43
  - text: "second answer"
    usage:
      prompt_tokens: 70
      completion_tokens: 3
      total_tokens: 73
"#;
    let mock_server = start_mock_server(MockFixture::Yaml(fixture_yaml.to_string())).await?;
    let browser_handle = launch_browser().await?;

    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        &mock_server.base_url,
        &[],
    )
    .await?;

    run_prompt(&page, "first question").await?;
    run_prompt(&page, "second question").await?;

    let blocks = get_timeline_blocks(&page).await?;

    let prompt_blocks: Vec<_> = blocks.iter().filter(|b| b.is("prompt")).collect();
    assert_eq!(
        prompt_blocks.len(),
        2,
        "expected 2 Prompt blocks, got blocks: {:?}",
        blocks.iter().map(|b| &b.class_name).collect::<Vec<_>>()
    );
    assert!(prompt_blocks[0].text.contains("first question"));
    assert!(prompt_blocks[1].text.contains("second question"));

    let response_blocks: Vec<_> = blocks.iter().filter(|b| b.is("response")).collect();
    assert_eq!(response_blocks.len(), 2);
    assert!(
        response_blocks[0]
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("first answer")
    );
    assert!(
        response_blocks[1]
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("second answer")
    );

    // The system prompt (Context block) is only built/reported once, on the
    // first turn of the session.
    let context_blocks: Vec<_> = blocks.iter().filter(|b| b.is("context")).collect();
    assert_eq!(context_blocks.len(), 1);

    Ok(())
}
