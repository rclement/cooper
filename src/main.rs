mod agent;
mod cli;
mod config;
mod providers;
mod tools;

#[tokio::main]
async fn main() {
    cli::run().await;
}
