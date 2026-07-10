//! `cooper prompt`: one-shot prompts — what gets streamed to the terminal,
//! which files feed the agent, and how misconfiguration is reported.

use crate::support::*;

#[test]
fn a_prompt_streams_the_reply_and_reports_usage_and_timing() {
    let base_url = start_mock_provider(
        r#"
responses:
  - text: "PONG"
    usage:
      prompt_tokens: 42
      completion_tokens: 3
      total_tokens: 45
"#,
    );
    let cli = Cli::with_provider(&base_url);

    let output = cli.run(&["prompt", "ping"]);

    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.contains("provider: mock"));
    assert!(stdout.contains("model: mock-model"));
    assert!(stdout.contains("PONG"));
    assert!(
        stdout.contains("[usage] prompt tokens = 42, completion tokens = 3, total tokens = 45")
    );
    assert!(stdout.contains("[timing]"));
}

#[test]
fn reasoning_is_shown_before_the_response_each_under_its_own_marker() {
    let base_url = start_mock_provider(
        r#"
responses:
  - reasoning: "the user wants a pong"
    text: "PONG"
"#,
    );
    let cli = Cli::with_provider(&base_url);

    let stdout = stdout(&cli.run(&["prompt", "ping"]));

    let reasoning = stdout.find("[reasoning] ").expect("reasoning marker");
    let response = stdout.find("[response] ").expect("response marker");
    assert!(reasoning < response);
    assert!(stdout.contains("the user wants a pong"));
}

#[test]
fn requested_tools_are_run_and_their_results_shown() {
    let base_url = start_mock_provider(
        r#"
responses:
  - tool_calls:
      - id: call-1
        name: exec_cmd
        arguments:
          command: "echo hello-from-tool"
  - text: "the command printed hello-from-tool"
"#,
    );
    let cli = Cli::with_provider(&base_url);

    let output = cli.run(&["prompt", "run echo"]);

    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.contains("[tool call] exec_cmd"));
    assert!(stdout.contains("[tool result]"));
    assert!(stdout.contains("hello-from-tool"));
    assert!(stdout.contains("the command printed hello-from-tool"));
}

#[test]
fn agent_instructions_and_context_files_are_read_before_the_turn() {
    let base_url = start_mock_provider(&reply_fixture("understood"));
    let cli = Cli::with_provider(&base_url);
    let instructions = cli.home().join("instructions.md");
    let context = cli.home().join("notes.md");
    std::fs::write(&instructions, "be terse").unwrap();
    std::fs::write(&context, "the sky is green").unwrap();

    let output = cli.run(&[
        "prompt",
        "hi",
        "-i",
        instructions.to_str().unwrap(),
        "-c",
        context.to_str().unwrap(),
    ]);

    assert!(output.status.success());
    assert!(stdout(&output).contains("understood"));
}

#[test]
fn a_missing_instructions_file_stops_the_run() {
    let base_url = start_mock_provider(&reply_fixture("unreached"));
    let cli = Cli::with_provider(&base_url);

    let output = cli.run(&["prompt", "hi", "-i", "/nonexistent/instructions.md"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("failed to read agent instructions file"));
}

#[test]
fn an_unreadable_context_file_is_a_warning_not_a_failure() {
    let base_url = start_mock_provider(&reply_fixture("still-answered"));
    let cli = Cli::with_provider(&base_url);

    let output = cli.run(&["prompt", "hi", "-c", "/nonexistent/notes.md"]);

    assert!(output.status.success());
    assert!(stderr(&output).contains("failed to read context file"));
    assert!(stdout(&output).contains("still-answered"));
}

#[test]
fn without_a_settings_file_the_cli_explains_and_exits() {
    let cli = Cli::without_config();

    let output = cli.run(&["prompt", "hi"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("error loading config"));
}

#[test]
fn asking_for_an_unknown_provider_fails_before_any_request() {
    let cli = Cli::with_provider("http://127.0.0.1:1/v1");

    let output = cli.run(&["prompt", "hi", "-p", "nope"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("provider 'nope' not found in config"));
}

#[test]
fn asking_for_a_model_the_provider_does_not_have_fails() {
    let cli = Cli::with_provider("http://127.0.0.1:1/v1");

    let output = cli.run(&["prompt", "hi", "-m", "nope"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("model 'nope' not found in provider 'mock'"));
}

#[test]
fn a_provider_type_the_binary_does_not_speak_is_rejected() {
    let cli = Cli::with_provider_type("http://127.0.0.1:1/v1", "carrier-pigeon");

    let output = cli.run(&["prompt", "hi"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown provider type: carrier-pigeon"));
}

#[test]
fn an_unreachable_provider_is_reported_as_an_error() {
    let cli = Cli::with_provider("http://127.0.0.1:1/v1");

    let output = cli.run(&["prompt", "hi"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("error:"));
}
