//! `cooper chat`: the interactive loop — how sessions start, get named,
//! persist, resume, and end.

use crate::support::*;

fn saved_sessions(cli: &Cli) -> Vec<serde_json::Value> {
    let Ok(entries) = std::fs::read_dir(cli.sessions_dir()) else {
        return Vec::new();
    };
    entries
        .map(|e| {
            let content = std::fs::read_to_string(e.unwrap().path()).unwrap();
            serde_json::from_str(&content).unwrap()
        })
        .collect()
}

#[test]
fn a_chat_turn_answers_and_saves_the_session() {
    let base_url = start_mock_provider(&reply_fixture("hello to you"));
    let cli = Cli::with_provider(&base_url);

    let output = cli.run_with_stdin(&["chat"], "hi cooper\nexit\n");

    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.contains("Chat session started"));
    assert!(stdout.contains("hello to you"));

    let sessions = saved_sessions(&cli);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["title"], "hi cooper");
    assert!(!sessions[0]["history"].as_array().unwrap().is_empty());
}

#[test]
fn the_first_message_names_the_session_capped_at_80_chars() {
    let base_url = start_mock_provider(&reply_fixture("ok"));
    let cli = Cli::with_provider(&base_url);
    let long_line = "x".repeat(100);

    cli.run_with_stdin(&["chat"], &format!("{long_line}\nexit\n"));

    let sessions = saved_sessions(&cli);
    assert_eq!(sessions[0]["title"], "x".repeat(80));
}

#[test]
fn quit_an_empty_line_or_closing_stdin_all_end_the_chat() {
    let base_url = start_mock_provider(&reply_fixture("ok"));
    let cli = Cli::with_provider(&base_url);

    for input in ["quit\n", "\n", ""] {
        let output = cli.run_with_stdin(&["chat"], input);
        assert!(output.status.success());
    }

    // None of those inputs sent a message, so nothing was saved.
    assert!(saved_sessions(&cli).is_empty());
}

#[test]
fn resuming_replays_the_saved_transcript_before_continuing() {
    let base_url = start_mock_provider(&reply_fixture("welcome back"));
    let cli = Cli::with_provider(&base_url);
    cli.write_session(
        "1234",
        &serde_json::json!({
            "id": "1234",
            "title": "damn fine coffee",
            "created_at": 0,
            "updated_at": 0,
            "provider": "mock",
            "model": "mock-model",
            "history": [
                { "User": "hi" },
                { "Assistant": { "text": "hello", "reasoning": null, "tool_calls": [] } },
            ],
        }),
    );

    let output = cli.run_with_stdin(&["chat", "-r", "1234"], "exit\n");

    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.contains("Resuming session \"damn fine coffee\" (2 messages so far)"));
    assert!(stdout.contains("[you] hi"));
    assert!(stdout.contains("[response] hello"));
}

#[test]
fn resuming_an_unknown_session_fails_up_front() {
    let base_url = start_mock_provider(&reply_fixture("unreached"));
    let cli = Cli::with_provider(&base_url);

    let output = cli.run_with_stdin(&["chat", "-r", "no-such-id"], "exit\n");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("error loading session 'no-such-id'"));
}

#[test]
fn a_turn_still_completes_when_the_session_cannot_be_saved() {
    let base_url = start_mock_provider(&reply_fixture("answered anyway"));
    let cli = Cli::with_provider(&base_url);
    // A file where the sessions *directory* should be makes every save fail.
    std::fs::create_dir_all(cli.home().join(".cooper")).unwrap();
    std::fs::write(cli.sessions_dir(), "not a directory").unwrap();

    let output = cli.run_with_stdin(&["chat"], "hi\nexit\n");

    assert!(output.status.success());
    assert!(stdout(&output).contains("answered anyway"));
    assert!(stderr(&output).contains("warning: failed to save session"));
}

#[test]
fn a_provider_error_ends_the_chat_instead_of_looping() {
    let cli = Cli::with_provider("http://127.0.0.1:1/v1");

    let output = cli.run_with_stdin(&["chat"], "hi\nthis line is never read\n");

    assert!(output.status.success());
    assert!(stderr(&output).contains("error:"));
}
