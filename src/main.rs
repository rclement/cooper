mod cli;
mod config;
mod sessions;
#[cfg(test)]
mod test_support;
mod tools;
mod web;

#[tokio::main]
async fn main() {
    // Optional .env from the current directory (e.g. the OAuth client
    // credentials `cooper web` reads); real env vars take precedence and a
    // missing file is fine.
    dotenvy::dotenv().ok();
    cli::run().await;
}
