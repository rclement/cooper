//! Shared harness: an isolated HOME with `~/.cooper/settings.yml` pointing
//! at an in-process mock provider, plus helpers to run the real `cooper`
//! binary inside it.

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use cooper_mock_server::Fixture;

/// Starts the mock OpenAI-compatible provider on its own thread (so plain
/// `#[test]`s can block on the spawned binary while it serves) and returns
/// the `/v1` base URL to put in settings.
pub fn start_mock_provider(fixture_yaml: &str) -> String {
    let fixture = Fixture::from_yaml_str(fixture_yaml).expect("invalid fixture yaml");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _ = runtime.block_on(cooper_mock_server::run(fixture, addr));
    });
    wait_until_listening(addr);

    format!("http://{addr}/v1")
}

/// A port that was free a moment ago — good enough for handing to a child
/// process that binds it immediately.
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

pub fn wait_until_listening(addr: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on {addr} never came up");
}

/// An isolated home directory for one test run of the CLI.
pub struct Cli {
    home: tempfile::TempDir,
}

impl Cli {
    /// A home whose settings declare a single `mock` provider with a single
    /// `mock-model`, served at `base_url`.
    pub fn with_provider(base_url: &str) -> Self {
        Self::with_provider_type(base_url, "openai-completions")
    }

    pub fn with_provider_type(base_url: &str, provider_type: &str) -> Self {
        let cli = Self::without_config();
        let cooper_dir = cli.home.path().join(".cooper");
        std::fs::create_dir_all(&cooper_dir).unwrap();
        std::fs::write(
            cooper_dir.join("settings.yml"),
            format!(
                r#"
default_provider: mock
default_model: mock-model
providers:
  mock:
    provider_type: {provider_type}
    base_url: {base_url}
    api_key: test-key
    models:
      - id: mock-model
"#
            ),
        )
        .unwrap();
        cli
    }

    pub fn without_config() -> Self {
        Cli {
            home: tempfile::tempdir().unwrap(),
        }
    }

    pub fn home(&self) -> &Path {
        self.home.path()
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.home.path().join(".cooper/sessions")
    }

    /// Drops a session file into `~/.cooper/sessions`, as a previous `chat`
    /// run would have left it.
    pub fn write_session(&self, id: &str, json: &serde_json::Value) {
        std::fs::create_dir_all(self.sessions_dir()).unwrap();
        std::fs::write(
            self.sessions_dir().join(format!("{id}.json")),
            serde_json::to_string_pretty(json).unwrap(),
        )
        .unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cooper"));
        command
            .args(args)
            .env("HOME", self.home.path())
            // Run from the temp home so the binary never picks up the
            // repo's .env file.
            .current_dir(self.home.path());
        command
    }

    /// Runs `cooper <args>` to completion with no stdin.
    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).stdin(Stdio::null()).output().unwrap()
    }

    /// Runs `cooper <args>` typing `input` on stdin (for `chat`).
    pub fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Spawns `cooper <args>` and leaves it running (for `web`); the child
    /// is killed when the returned guard drops.
    pub fn spawn(&self, args: &[&str], env: &[(&str, &str)]) -> RunningChild {
        let mut command = self.command(args);
        for (name, value) in env {
            command.env(name, value);
        }
        RunningChild(command.stdin(Stdio::null()).spawn().unwrap())
    }
}

pub struct RunningChild(std::process::Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A one-reply fixture: the provider answers every prompt with `text`.
pub fn reply_fixture(text: &str) -> String {
    format!("responses:\n  - text: \"{text}\"\n")
}
