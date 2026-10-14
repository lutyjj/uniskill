---
name: uniskill-dev
description: Workflow and conventions for developing uniskill, the skill bundle wiring tool. Covers build, test, feature flow, and project architecture.
---

# Uniskill development

You are working on `uniskill`, a Rust CLI that wires skill bundles from source directories into multiple agent harnesses via symlinks.

## Quick commands

```bash
cargo test               # run all tests (Rust installed natively)
cargo build --release    # build binary in target/release/uniskill
cargo clippy --all-features -- -D warnings
cargo fmt -- --check
./target/release/uniskill sync          # sync global config (~/.config/uniskill/)
./target/release/uniskill --config /path/to/uniskill.toml sync  # project-level sync
```

If Docker is preferred: `make test`, `make build`, `make lint`. The dev container runs on `/src` with Cargo registry cached in a volume.

## Project structure

| Module | Responsibility |
|--------|---------------|
| `cli.rs` | CLI parsing, config discovery, report printing, exit codes |
| `config.rs` | TOML deserialization, env var expansion, path resolution |
| `fetcher.rs` | Materialize local, URL, and git sources into the bundle cache |
| `harnesses.rs` | Built-in harness registry, the source of default harness data |
| `linker.rs` | Symlink creation: detect existing links, handle conflicts, idempotent updates |
| `skill.rs` | Shared skill-directory predicate |
| `state.rs` | Manifest load/save and managed-link ownership checks |
| `sync.rs` | Sync orchestration and structured reports |
| `error.rs` | Custom CLI error types |

**Key contract**: `harnesses::default_harnesses()` is the source of truth for
default harness data. Config parsing returns an empty map when no user
`[harnesses]` section exists; runtime wiring merges built-ins and user
overrides.

## Adding a feature

1. Write a test first (S4: meaningful unit tests, one coherent behavior per test)
2. Implement against failing test
3. Run `cargo clippy --all-features -- -D warnings`; warnings fail the build
4. Confirm idempotency if the feature affects filesystem state

### Config changes

New config fields must be optional with sensible defaults (`#[serde(default)]`).
Never break an existing config file for new features. If adding a top-level key,
document it in DESIGN.md.

### Harness additions

New built-in harnesses go in `harnesses.rs::default_harnesses()` only. No code
changes are needed in `cli.rs`, `sync.rs`, or `linker.rs`; those modules consume
the registry.

## Sync behavior

- **Idempotent**: running sync twice on the same state reports all "ok"
- **Updated** status: existing symlink replaced because it points to the wrong
  source or was broken
- **Conflict**: non-symlink file exists at target path; skipped with warning
- **Exit 1**: any conflicts or broken links present (designed for scripted use)

## Design principles

- DRY: one source of truth per contract; do not duplicate default data across
  modules
- Name states: use enum variants, not numbers, for SyncStatus
- Separated resp: each module owns exactly one concern; no cross-module logic leaks
- New variants add code: new harness types = config changes only, zero code edits

## Configuration files

| File | Purpose |
|------|---------|
| `config.toml.example` | Global config template; place at `~/.config/uniskill/config.toml` |
| `uniskill.toml` | Project-level config; auto-discovered in CWD, installs skills locally |
| `Cargo.toml` | Package metadata and dependencies |
| `Makefile` | Docker-based build/test targets |
