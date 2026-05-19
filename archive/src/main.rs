mod session;
mod cli;
mod config;
mod skills;
mod tools;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
