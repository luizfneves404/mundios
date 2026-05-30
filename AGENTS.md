# AGENTS.md

## Project overview

**Mundios** is a single-crate Rust CLI that runs an agent-based economic simulation (settlements, trade routes, risk-averse traders). Output is colored tables in the terminal. There is no web server, database, or Docker stack.

## Common commands

| Task | Command |
|------|---------|
| Run simulation (12 months) | `cargo +nightly run` |
| Release binary | `cargo +nightly build --release` then `./target/release/mundios` |
| Build | `cargo +nightly build` |
| Test | `cargo +nightly test` (0 tests today) |
| Lint | `cargo +nightly clippy` |
| Format | `cargo +nightly fmt` |

See `README.md` for simulation design notes (not a product roadmap).

## Cursor Cloud specific instructions

- **Rust edition 2024**: `Cargo.toml` sets `edition = "2024"`. The default stable toolchain on many images (e.g. 1.83) cannot build this crate. Use **`cargo +nightly`** for build, run, test, clippy, and fmt. If `nightly` is missing: `rustup toolchain install nightly`.
- **No external services**: Nothing to start besides running the binary. End-to-end verification is `cargo +nightly run` and checking stdout for settlement/trader tables through Month 12.
- **Clippy**: `cargo +nightly clippy` passes with warnings (dead code, style). `cargo clippy -- -D warnings` is stricter than the project currently enforces.
- **Tests**: The README notes the project intentionally has almost no automated tests; `cargo +nightly test` only confirms the crate compiles.
