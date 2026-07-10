use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cooper_core::agent::Message;
use serde::{Deserialize, Serialize};

/// A saved `chat` conversation: metadata plus the exact `Vec<Message>`
/// history `agent_loop_stream` threads through the loop — the same
/// structure the web UI persists (via `WasmAgent::export_history`). Reusing
/// it here means a session is always message-level (one entry per
/// system/user/assistant/tool message), never per streamed chunk — there's
/// no separate log to accidentally store at the wrong granularity.
#[derive(Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub provider: String,
    pub model: String,
    pub history: Vec<Message>,
}

impl SessionRecord {
    pub fn new(provider: String, model: String) -> Self {
        let now = now_unix();
        SessionRecord {
            id: now.to_string(),
            title: String::new(),
            created_at: now,
            updated_at: now,
            provider,
            model,
            history: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_unix();
    }
}

/// Milliseconds since the Unix epoch — used both as a session's default id
/// (unique enough for interactive CLI use) and for `updated_at`/sorting.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = dirs::home_dir()
        .ok_or("could not determine home directory")?
        .join(".cooper/sessions");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn session_path(id: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(sessions_dir()?.join(format!("{id}.json")))
}

pub fn save(session: &SessionRecord) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(session_path(&session.id)?, json)?;
    Ok(())
}

pub fn load(id: &str) -> Result<SessionRecord, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(session_path(id)?)?;
    Ok(serde_json::from_str(&content)?)
}

/// Returns saved sessions, most-recently-updated first. Skips any file that
/// isn't a valid session record (e.g. from a future, incompatible version)
/// rather than failing the whole listing.
pub fn list() -> Result<Vec<SessionRecord>, Box<dyn std::error::Error>> {
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(sessions_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(session) = serde_json::from_str(&content)
        {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a: &SessionRecord, b: &SessionRecord| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HomeEnvGuard;

    #[test]
    fn save_and_load_round_trips_a_session() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let mut session = SessionRecord::new("openai".to_string(), "gpt-4".to_string());
        session.title = "hello there".to_string();
        session.history.push(Message::User("hi".to_string()));
        save(&session).unwrap();

        let loaded = load(&session.id).unwrap();

        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.title, "hello there");
        assert_eq!(loaded.provider, "openai");
        assert_eq!(loaded.model, "gpt-4");
        match &loaded.history[0] {
            Message::User(text) => assert_eq!(text, "hi"),
            _ => panic!("expected a user message"),
        }
    }

    #[test]
    fn load_fails_for_unknown_id() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        assert!(load("does-not-exist").is_err());
    }

    #[test]
    fn list_returns_empty_when_no_sessions_saved() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        assert!(list().unwrap().is_empty());
    }

    #[test]
    fn list_sorts_most_recently_updated_first() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let mut older = SessionRecord::new("openai".to_string(), "gpt-4".to_string());
        older.id = "1".to_string();
        older.updated_at = 1000;
        save(&older).unwrap();

        let mut newer = SessionRecord::new("openai".to_string(), "gpt-4".to_string());
        newer.id = "2".to_string();
        newer.updated_at = 2000;
        save(&newer).unwrap();

        let sessions = list().unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "2");
        assert_eq!(sessions[1].id, "1");
    }

    #[test]
    fn list_ignores_files_that_are_not_session_records() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let session = SessionRecord::new("openai".to_string(), "gpt-4".to_string());
        save(&session).unwrap();
        let dir = tmp_home.path().join(".cooper/sessions");
        std::fs::write(dir.join("notes.txt"), "not a session").unwrap();
        std::fs::write(dir.join("corrupt.json"), "{ not json").unwrap();

        let sessions = list().unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id);
    }

    #[test]
    fn touch_updates_updated_at() {
        let mut session = SessionRecord::new("openai".to_string(), "gpt-4".to_string());
        session.updated_at = 0;

        session.touch();

        assert!(session.updated_at > 0);
    }
}
