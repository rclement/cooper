//! Builds the wasm package `cooper web` serves (`web/pkg`) via `wasm-pack`,
//! so `cargo build`/`cargo run` on the CLI keeps it up to date without a
//! separate manual step. Non-fatal if `wasm-pack` isn't installed — the CLI
//! itself (`prompt`/`chat`/`sessions`) doesn't need it, only `cooper web`
//! does, and `src/web.rs` checks for `web/pkg` again at runtime and tells
//! the user how to build it if this step was skipped.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/Cargo.toml");
    println!("cargo:rerun-if-changed=core/src");
    println!("cargo:rerun-if-changed=core/Cargo.toml");
    // Also watch our own output: Cargo only reruns a build script when a
    // watched source changes, never just because a declared output went
    // missing. Without this, a `web/pkg` deleted by something other than
    // this script (e.g. a CI cache restoring `target/`'s build-script
    // fingerprint without restoring `web/pkg`, which it doesn't cache)
    // would leave it missing and never get rebuilt.
    println!("cargo:rerun-if-changed=web/pkg/cooper_web.js");
    println!("cargo:rerun-if-env-changed=COOPER_SKIP_WASM_BUILD");

    if std::env::var_os("COOPER_SKIP_WASM_BUILD").is_some() {
        return;
    }

    if Command::new("wasm-pack").arg("--version").output().is_err() {
        println!(
            "cargo:warning=wasm-pack not found; skipping web/pkg build — `cooper web` will need \
             `wasm-pack build --target web web/` run manually (or install wasm-pack; set \
             COOPER_SKIP_WASM_BUILD=1 to silence this)"
        );
        return;
    }

    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let status = Command::new("wasm-pack")
        .args(["build", "--target", "web"])
        .arg(&web_dir)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("wasm-pack build failed with {s}"),
        Err(e) => panic!("failed to run wasm-pack: {e}"),
    }
}
