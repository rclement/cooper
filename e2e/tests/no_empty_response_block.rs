//! Regression test: some providers emit a whitespace-only content delta
//! (e.g. a lone "\n") right before a tool call. core/src/providers/
//! openai_completions.rs must drop that instead of surfacing it as an empty
//! Response block between Reasoning and Tool.
use cooper_e2e::*;

#[tokio::test]
async fn whitespace_only_content_before_a_tool_call_does_not_create_an_empty_response_block()
-> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;

    let fixture_yaml = format!(
        r#"
responses:
  - reasoning: "I should check something first."
    text: "\n"
    tool_calls:
      - id: call-1
        name: fetch_url
        arguments:
          url: "{}/www/vendor/README.md"
    usage:
      prompt_tokens: 60
      completion_tokens: 18
      total_tokens: 78
  - text: "First paragraph.\n\nSecond paragraph."
    usage:
      prompt_tokens: 90
      completion_tokens: 6
      total_tokens: 96
"#,
        static_server.base_url
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
    run_prompt(&page, "hello").await?;

    let blocks = get_timeline_blocks(&page).await?;

    // Exactly one Response block (the real final answer) — not two, with an
    // empty one sandwiched between Reasoning and Tool.
    let response_blocks: Vec<_> = blocks.iter().filter(|b| b.is("response")).collect();
    assert_eq!(
        response_blocks.len(),
        1,
        "expected exactly 1 Response block, got blocks: {:?}",
        blocks.iter().map(|b| &b.class_name).collect::<Vec<_>>()
    );

    // The one Response block preserves the blank line between paragraphs —
    // proving genuine mid-response whitespace isn't collateral damage from
    // the leading-whitespace filter.
    let body = response_blocks[0].body_html.as_deref().unwrap_or_default();
    assert!(body.contains("<p>First paragraph.</p>"));
    assert!(body.contains("<p>Second paragraph.</p>"));

    Ok(())
}
