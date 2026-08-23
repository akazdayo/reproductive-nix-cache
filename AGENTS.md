# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 workspace contains five crates:

- `crates/cli`: the `reproductive-nix-cache` command. It is a thin wrapper around `builder-core` with trust calculation and human/JSON output.
- `crates/builder-core`: Nix build, evidence generation, and commit-reveal orchestration shared by the CLI and builder node.
- `crates/builder-node`: the authenticated, single-flight HTTP daemon that executes build commands.
- `crates/server`: the Axum registry and binary-cache gateway. API routes, SQLite persistence, configuration, and HTTP upstream access are split by module.
- `crates/shared`: evidence and HTTP wire types shared across binaries.

Tests are colocated with implementation code in `#[cfg(test)]` modules. Root files include the workspace manifests, Nix flake, licenses, and a short usage-oriented `README.md`.

## Build, Test, and Development Commands

Enter the reproducible shell with `direnv allow` or `nix develop`. The shell supplies stable Rust, Nix tooling, and formatting hooks.

- `cargo build --workspace`: compile every crate.
- `cargo test --workspace`: run unit and async integration-style tests.
- `cargo clippy --workspace --all-targets -- -D warnings`: reject Clippy warnings.
- `nix fmt`: format Rust and Nix files through treefmt.
- `cargo run -p server -- --help`: inspect or launch the registry server.
- `cargo run -p cli -- build nixpkgs#hello --builder-id builder-a --server 127.0.0.1:51337`: run the client against a local server.

The CLI shells out to `nix`; commands exercising real builds require `nix` on `PATH`.

## Coding Style & Naming Conventions

Use rustfmt defaults (four-space indentation). Follow standard Rust naming: `snake_case` for modules, functions, and tests; `PascalCase` for types and enums; `SCREAMING_SNAKE_CASE` for constants. Keep CLI/server-specific behavior in its crate and move wire-format types into `shared`. Add context to fallible operations with `anyhow` and avoid exposing credentials in errors or logs.

## Testing Guidelines

Add focused tests beside changed code. Use descriptive behavior names such as `build_accepts_s3_binary_cache`; use `#[tokio::test]` for async paths. Keep unit tests deterministic and mock or isolate storage/network boundaries. No coverage threshold is enforced, but regressions should include a test. Run formatting, Clippy, and the full workspace suite before opening a pull request.

## Commit & Pull Request Guidelines

History follows Conventional Commit-style prefixes, chiefly `feat:`, `fix:`, and `chore:`. Use a short, imperative subject describing one logical change. Pull requests should explain user-visible behavior, configuration or API changes, verification commands, and linked issues. Include sample CLI/HTTP output when interfaces change; screenshots are only useful for rendered output.

## Security & Local Configuration

Do not commit real secrets, access tokens, databases, or generated cache data. Keep an upstream binary cache network-private when it should only be reachable through the consensus gateway.
