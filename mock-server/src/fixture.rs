use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// A scripted sequence of OpenAI-chat-completions-compatible responses,
/// served one per request in order. Add a fixture by dropping a YAML file
/// under `fixtures/` — no Rust changes needed.
#[derive(Debug, Deserialize)]
pub struct Fixture {
    pub responses: Vec<FixtureResponse>,
}

#[derive(Debug, Deserialize)]
pub struct FixtureResponse {
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<FixtureToolCall>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<FixtureUsage>,
}

#[derive(Debug, Deserialize)]
pub struct FixtureToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Fixture {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Self::from_yaml_str(&content)
    }

    /// Parses a fixture directly from a YAML string, for callers that need
    /// to build one at runtime (e.g. e2e tests interpolating a dynamic URL)
    /// rather than loading a file from `fixtures/`.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let fixture: Fixture = serde_yaml::from_str(yaml)?;
        Ok(fixture)
    }
}

impl FixtureResponse {
    /// `stop` unless overridden or tool calls are present, mirroring what a
    /// real provider would infer.
    pub fn finish_reason(&self) -> &str {
        self.finish_reason
            .as_deref()
            .unwrap_or(if self.tool_calls.is_empty() {
                "stop"
            } else {
                "tool_calls"
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_parses_minimal_fixture() {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmpfile.path(),
            r#"
responses:
  - text: "PONG"
"#,
        )
        .unwrap();

        let fixture = Fixture::load(tmpfile.path()).unwrap();

        assert_eq!(fixture.responses.len(), 1);
        assert_eq!(fixture.responses[0].text.as_deref(), Some("PONG"));
        assert_eq!(fixture.responses[0].finish_reason(), "stop");
    }

    #[test]
    fn from_yaml_str_parses_minimal_fixture() {
        let fixture = Fixture::from_yaml_str("responses:\n  - text: \"PONG\"\n").unwrap();

        assert_eq!(fixture.responses.len(), 1);
        assert_eq!(fixture.responses[0].text.as_deref(), Some("PONG"));
    }

    #[test]
    fn load_parses_tool_call_fixture() {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmpfile.path(),
            r#"
responses:
  - reasoning: "I should check."
    tool_calls:
      - id: call-1
        name: exec_cmd
        arguments:
          command: "echo PONG"
"#,
        )
        .unwrap();

        let fixture = Fixture::load(tmpfile.path()).unwrap();
        let response = &fixture.responses[0];

        assert_eq!(response.tool_calls[0].name, "exec_cmd");
        assert_eq!(
            response.tool_calls[0].arguments.get("command"),
            Some(&"echo PONG".to_string())
        );
        assert_eq!(response.finish_reason(), "tool_calls");
    }

    #[test]
    fn load_fails_on_missing_file() {
        let err = Fixture::load(Path::new("/nonexistent/fixture.yaml")).unwrap_err();

        assert!(err.to_string().contains("No such file or directory"));
    }
}
