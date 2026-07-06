---
name: uniskill-dev
description: Workflow and conventions for developing uniskill — the skill bundle wiring tool. Covers build, test, feature flow, and project architecture.
---

# Uniskill Development

You are working on `uniskill`, a Rust CLI that wires skill bundles from source directories into multiple agent harnesses via symlinks.

## Quick Commands

```bash
cargo test               # run all tests (Rust installed natively)
cargo build --release    # build binary in target/release/uniskill
cargo clippy -- -D warnings
cargo fmt
./target/release/uniskill sync          # sync global config (~/.config/uniskill/)
./target/release/uniskill --config /path/to/uniskill.toml sync  # project-level sync
```

If Docker is preferred: `make test`, `make build`, `make lint`. The dev container runs on `/src` with Cargo registry cached in a volume.

## Project Structure

| Module | Responsibility |
|--------|---------------|
| `cli.rs` | CLI parsing (clap), config discovery (global vs project), orchestration |
| `config.rs` | TOML deserialization, env var expansion, path resolution |
| `harnesses.rs` | Built-in harness registry — the single source of default harness data |
| `linker.rs` | Symlink creation: detect existing links, handle conflicts, idempotent updates |
| `error.rs` | Custom error types (mostly reserved for future `status` command) |

**Key contract**: `harnesses::default_harnesses()` is the one source of truth for default harness data. Config parsing always returns an empty map and lets CLI merge defaults at runtime.

## Adding a Feature

1. Write a test first (S4: meaningful unit tests — one coherent behavior per test)
2. Implement against failing test
3. Run `cargo clippy -- -D warnings` — zero warnings required
4. Confirm idempotency if the feature affects filesystem state

### Config changes

New config fields must be optional with sensible defaults (`#[serde(default)]`). Never break an existing config file for new features. If adding a top-level key, document it in DESIGN.md.

### Harness additions

New built-in harnesses go in `harnesses.rs::default_harnesses()` only (S12: one source of truth). No code changes needed in cli or linker — the registry is consumed dynamically.

## Sync Behavior

- **Idempotent**: running sync twice on the same state reports all "ok"
- **Updated** status: existing symlink replaced because it points to wrong source or was broken
- **Conflict**: non-symlink file exists at target path — skipped with warning
- **Exit 1**: any conflicts or broken links present (designed for scripted use)

## Design Principles

- DRY: one source of truth per contract — no duplicate default data across modules
- Name states: use enum variants, not numbers, for SyncStatus
- Separated resp: each module owns exactly one concern; no cross-module logic leaks
- New variants add code: new harness types = config changes only, zero code edits

## Configuration Files

| File | Purpose |
|------|---------|
| `config.toml.example` | Global config template — place at `~/.config/uniskill/config.toml` |
| `uniskill.toml` | Project-level config — auto-discovered in CWD, installs skills locally |
| `Cargo.toml` | Package metadata and dependencies |
| `Makefile` | Docker-based build/test targets |
