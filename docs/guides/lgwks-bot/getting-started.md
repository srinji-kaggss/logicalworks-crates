# Getting started with lgwks_bot

Pages of this guide: [manifest](getting-started.md) · [the program](getting-started-program.md) · [the pieces](getting-started-pieces.md)

One source, one condition, one effect, written as a `script!` flow. The program on
the next page is `crates/lgwks-bot/examples/queue_watch.rs`: it uses only
`lgwks_bot`, `script` is one of the crate's default features, and CI builds every
example in the workspace. `cargo run -p lgwks_bot --example queue_watch` runs it.

## The manifest

```toml
[package]
name = "queue-watch"
version = "0.1.0"
edition = "2024"

[dependencies]
lgwks_bot = "2"
```
