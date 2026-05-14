mod agent;
mod cli;
mod config;
mod providers;
mod skills;
mod tools;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
