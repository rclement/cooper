mod agent;
mod cli;
mod config;
mod providers;

#[tokio::main]
async fn main() {
    cli::run().await;
}
