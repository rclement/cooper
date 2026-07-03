//! The built-in fetch_url tool (enabled by default): the model calls it, the
//! browser actually fetches a same-origin resource, and the result feeds
//! back into the next turn. Exercises the full WasmAgent.register_tool bridge.
use cooper_e2e::*;

#[tokio::test]
async fn fetch_url_tool_call_round_trips_through_the_wasm_agent()
-> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;

    // Points fetch_url at a real file served by our own static server, so
    // the tool call is a genuine network round trip with a deterministic
    // result.
    let fixture_yaml = format!(
        r#"
responses:
  - reasoning: "I should check that URL."
    tool_calls:
      - id: call-1
        name: fetch_url
        arguments:
          url: "{}/www/vendor/README.md"
    usage:
      prompt_tokens: 60
      completion_tokens: 18
      total_tokens: 78
  - text: "Fetched it."
    usage:
      prompt_tokens: 90
      completion_tokens: 3
      total_tokens: 93
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

    assert!(
        is_tool_enabled(&page, 0).await?,
        "fetch_url should be enabled by default"
    );

    run_prompt(&page, "what does the vendor README say?").await?;

    let blocks = get_timeline_blocks(&page).await?;

    let tool = blocks
        .iter()
        .find(|b| b.is("tool"))
        .expect("expected a Tool block");
    let tool_html = tool.body_html.as_deref().unwrap_or_default();
    assert!(tool_html.contains("fetch_url"));
    assert!(tool_html.contains("Vendored dependencies"));

    let response = blocks
        .iter()
        .find(|b| b.is("response"))
        .expect("expected a final Response block");
    assert!(
        response
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("Fetched it.")
    );

    Ok(())
}
