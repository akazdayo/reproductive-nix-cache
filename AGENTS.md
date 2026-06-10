# AGENTS.md — reproductive-nix-cache

Rust CLI that collects structured build evidence from a Nix package. Calls `nix flake metadata`, `nix path-info`, `nix derivation show`, builds the package, then emits a JSON blob with resolved metadata, derivation hash, and output NAR hash.

## Dev environment

- **Nix flake** with `direnv` (`use flake`). Run `direnv allow` (once) to enter.
- **Rust edition 2024** (`Cargo.toml`). Stable toolchain via fenix overlay in `flake.nix`.
- Standard cargo commands: `cargo build`, `cargo test`, `cargo clippy`.
- `flake.nix` provides: `nixfmt` as the formatter, `cargo-deny`, `cargo-watch`, `cargo-edit`, `nix-output-monitor`.
- Nix code format: `nix fmt` (backed by `nixfmt`).

## Architecture

4 source files in `src/`, single crate:

| File | Role |
|---|---|
| `main.rs` | CLI entrypoint (clap). One subcommand: `build <package_ref> [--full-rebuild]`. Serializes `NixEvidence` to JSON stdout. |
| `models.rs` | All serde structs: `NixEvidence`, `FlakeMetadata`, `Derivation`, `PathInfo`, `PackageRef`. Tests with JSON fixtures live here. |
| `nix.rs` | Thin wrappers around the `nix` binary via `std::process::Command`. |
| `evidence.rs` | Orchestration: parse → resolve → derive → build → collect. |

## Key gotchas

- **`nix` must be on PATH.** The tool shells out to the `nix` binary. Without it every function in `nix.rs` returns an error.
- **Build prefers `nom` over `nix`.** `nix::run_build` tries `nom build` first; falls back to `nix build` if `nom` isn't found. This is not a bug — it's intentional for nicer terminal output when `nom` is installed.
- **`--no-link` on build.** The tool passes `--no-link` so the result goes into `/nix/store` but doesn't create a GC-root symlink in the working directory.
- **Derivation lookup fallback.** `nix::select_derivation` looks up by exact `.drv` path, but if it finds only one entry in the map it returns that entry regardless of path match. This handles cases where `nix derivation show` returns a different key than `nix path-info --derivation`.
- **flake.lock `narHash` fallback for `rev`.** In `build_evidence`, if `locked.rev` is `None`, it falls back to `locked.nar_hash`. Some flakes don't have a rev (e.g., tarball inputs).
- **Rust edition 2024** — newer syntax/conventions apply. `ProcessCommand` is aliased because `Command` is shadowed by clap.

## Testing

- Unit tests live in the same files (`models.rs`, `evidence.rs`) under `#[cfg(test)]`.
- Tests are pure Rust — no external process dependency. They test JSON parsing/serialization with string fixtures.
- Run with: `cargo test`

## Package reference format

Input must be `<flake>#<attr>` (e.g. `nixpkgs#hello`). Missing `#`, empty flake name, or empty attr path all produce errors.
