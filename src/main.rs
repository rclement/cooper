mod agent;
mod cli;
mod config;
mod providers;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
