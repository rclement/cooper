//! Agent instructions set in the right-hand Context panel must actually flow
//! into the rendered system prompt sent to the model.
use cooper_e2e::*;
use serde_json::json;

#[tokio::test]
async fn custom_agent_instructions_appear_in_the_rendered_system_prompt()
-> Result<(), Box<dyn std::error::Error>> {
    assert_prerequisites_built()?;

    let static_server = start_static_server().await?;
    let mock_server = start_mock_server(MockFixture::File("simple_reply.yaml")).await?;
    let browser_handle = launch_browser().await?;

    let context = json!({
        "enabledTools": { "fetch_url": true },
        "systemPromptTemplate": "",
        "agentInstructions": "Always answer in pirate speak.",
        "contextFiles": [],
    });
    let page = open_app(
        &browser_handle.browser,
        &static_server.base_url,
        &mock_server.base_url,
        &[("cooper.context.v1", context)],
    )
    .await?;

    run_prompt(&page, "ping").await?;

    let blocks = get_timeline_blocks(&page).await?;
    let context_block = blocks
        .iter()
        .find(|b| b.is("context"))
        .expect("expected a Context block");
    let text = &context_block.text;

    assert!(text.contains("agent-instructions"));
    assert!(text.contains("Always answer in pirate speak."));

    Ok(())
}
