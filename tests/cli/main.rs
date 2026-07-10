//! Product-level specs for the `cooper` CLI: each test runs the real binary
//! in an isolated HOME against a scripted mock provider, and asserts on
//! what a user would actually see on their terminal.

mod support;

mod chat;
mod prompt;
mod sessions;
mod web;
