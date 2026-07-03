mod cli;
mod config;
mod tools;

#[tokio::main]
async fn main() {
    cli::run().await;
}
