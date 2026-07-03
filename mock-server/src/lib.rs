mod fixture;
mod server;
mod wire;

pub use fixture::Fixture;

use std::net::SocketAddr;

/// Serves `fixture` as an OpenAI-chat-completions-compatible SSE endpoint at
/// `http://{addr}/v1/chat/completions`, one scripted response per request in
/// order. Runs until the process is killed.
pub async fn run(fixture: Fixture, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "cooper-mock-server listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, server::app(fixture)).await?;
    Ok(())
}
