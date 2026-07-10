//! `cooper sessions`: browsing saved chats — the list overview and the full
//! transcript view.

use crate::support::*;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn session(id: &str, title: &str, updated_at: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": title,
        "created_at": updated_at,
        "updated_at": updated_at,
        "provider": "mock",
        "model": "mock-model",
        "history": [{ "User": "hi" }],
    })
}

#[test]
fn with_nothing_saved_the_list_points_at_cooper_chat() {
    let cli = Cli::without_config();

    let output = cli.run(&["sessions", "list"]);

    assert!(output.status.success());
    assert!(stdout(&output).contains("No saved sessions yet. Start one with `cooper chat`."));
}

#[test]
fn the_list_shows_title_age_model_and_length_newest_first() {
    let cli = Cli::without_config();
    cli.write_session("1", &session("1", "older chat", now_ms() - 3 * 3600 * 1000));
    cli.write_session("2", &session("2", "newer chat", now_ms() - 30 * 1000));

    let output = cli.run(&["sessions", "list"]);

    assert!(output.status.success());
    let stdout = stdout(&output);
    let newer = stdout.find("newer chat").expect("newer chat listed");
    let older = stdout.find("older chat").expect("older chat listed");
    assert!(newer < older);
    assert!(stdout.contains("just now"));
    assert!(stdout.contains("3h ago"));
    assert!(stdout.contains("mock-model"));
    assert!(stdout.contains("1 messages"));
}

#[test]
fn a_session_never_named_is_listed_as_untitled() {
    let cli = Cli::without_config();
    cli.write_session("1", &session("1", "", now_ms()));

    let output = cli.run(&["sessions", "list"]);

    assert!(stdout(&output).contains("(untitled)"));
}

#[test]
fn an_unreadable_sessions_store_is_reported() {
    let cli = Cli::without_config();
    // A file where the sessions directory should be makes listing fail.
    std::fs::create_dir_all(cli.home().join(".cooper")).unwrap();
    std::fs::write(cli.sessions_dir(), "not a directory").unwrap();

    let output = cli.run(&["sessions", "list"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("error listing sessions"));
}

#[test]
fn show_renders_the_full_transcript_like_a_live_run() {
    let cli = Cli::without_config();
    cli.write_session(
        "42",
        &serde_json::json!({
            "id": "42",
            "title": "the owls",
            "created_at": 0,
            "updated_at": 0,
            "provider": "mock",
            "model": "mock-model",
            "history": [
                { "System": "be brief" },
                { "User": "list the files" },
                { "Assistant": {
                    "text": "one file: notes.txt",
                    "reasoning": "I should look",
                    "tool_calls": [
                        { "id": "call-1", "name": "list_files", "arguments": {} }
                    ],
                    "reasoning_duration_ms": 1200,
                    "response_duration_ms": 800,
                    "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 },
                }},
                { "Tool": { "call_id": "call-1", "result": { "Ok": "notes.txt" }, "duration_ms": 50 } },
                { "Tool": { "call_id": "call-1", "result": { "Err": "permission denied" } } },
            ],
        }),
    );

    let output = cli.run(&["sessions", "show", "42"]);

    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.contains("Session: the owls"));
    assert!(stdout.contains("Provider: mock  Model: mock-model"));
    assert!(stdout.contains("[system]\nbe brief"));
    assert!(stdout.contains("[you] list the files"));
    assert!(stdout.contains("[reasoning] I should look"));
    assert!(stdout.contains("[response] one file: notes.txt"));
    assert!(stdout.contains("[tool call] list_files"));
    assert!(stdout.contains("[usage] prompt tokens = 1, completion tokens = 2, total tokens = 3"));
    assert!(stdout.contains("[timing] reasoning = 1.2s, response = 800ms"));
    assert!(stdout.contains("[tool result] (50ms)\nnotes.txt"));
    assert!(stdout.contains("[tool error]\npermission denied"));
}

#[test]
fn show_falls_back_to_untitled_for_unnamed_sessions() {
    let cli = Cli::without_config();
    cli.write_session("7", &session("7", "", 0));

    let output = cli.run(&["sessions", "show", "7"]);

    assert!(stdout(&output).contains("Session: (untitled)"));
}

#[test]
fn showing_an_unknown_session_fails_with_its_id() {
    let cli = Cli::without_config();

    let output = cli.run(&["sessions", "show", "no-such-id"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("error loading session 'no-such-id'"));
}
