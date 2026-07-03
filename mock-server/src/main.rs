use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use cooper_mock_server::Fixture;

/// Serves canned OpenAI-chat-completions-compatible SSE responses from a YAML
/// fixture file, for deterministic local and end-to-end testing.
#[derive(clap::Parser)]
#[command(version, about)]
struct Args {
    /// Path to a fixture YAML file (see mock-server/fixtures/ for examples)
    fixture: PathBuf,
    /// Port to listen on
    #[arg(long, short = 'p', default_value_t = 4300)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let fixture = match Fixture::load(&args.fixture) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to load fixture '{}': {e}", args.fixture.display());
            std::process::exit(1);
        }
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    if let Err(e) = cooper_mock_server::run(fixture, addr).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
