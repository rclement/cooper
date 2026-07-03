mod cli;
mod config;
mod sessions;
#[cfg(test)]
mod test_support;
mod tools;

#[tokio::main]
async fn main() {
    cli::run().await;
}
