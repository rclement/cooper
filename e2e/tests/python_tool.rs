//! The run_python tool (enabled by default): the model calls it, Pyodide
//! actually boots and runs the code in the browser, and the result feeds
//! back into the next turn. Exercises the full WasmAgent.register_tool
//! bridge the same way tool_call.rs does for fetch_url, but for a tool that
//! lazily loads a vendored wasm runtime (see web/www/vendor/README.md).
use cooper_e2e::*;

#[tokio::test]
async fn run_python_tool_call_executes_real_code() -> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;

    let fixture_yaml = r#"
responses:
  - reasoning: "I should compute that."
    tool_calls:
      - id: call-1
        name: run_python
        arguments:
          code: "1 + 1"
    usage:
      prompt_tokens: 60
      completion_tokens: 18
      total_tokens: 78
  - text: "The answer is 2."
    usage:
      prompt_tokens: 90
      completion_tokens: 5
      total_tokens: 95
"#
    .to_string();
    let mock_server = start_mock_server(MockFixture::Yaml(fixture_yaml)).await?;
    let browser_handle = launch_browser().await?;

    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        &mock_server.base_url,
        &[],
    )
    .await?;

    run_prompt_with_timeout(&page, "what is 1 + 1?", std::time::Duration::from_secs(60)).await?;

    let blocks = get_timeline_blocks(&page).await?;

    let tool = blocks
        .iter()
        .find(|b| b.is("tool"))
        .expect("expected a Tool block");
    let tool_html = tool.body_html.as_deref().unwrap_or_default();
    assert!(tool_html.contains("run_python"));
    assert!(tool_html.contains("=&gt; 2") || tool_html.contains("=> 2"));

    let response = blocks
        .iter()
        .find(|b| b.is("response"))
        .expect("expected a final Response block");
    assert!(
        response
            .body_html
            .as_deref()
            .unwrap_or_default()
            .contains("The answer is 2.")
    );

    Ok(())
}
