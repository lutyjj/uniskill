# AGENTS.md

Rules for AI coding agents and humans working in this repository.

## Understand the product first

`uniskill` is a Rust CLI that wires reusable skill bundles into agent harnesses
by assembling a cache and creating managed symlinks. Before changing behavior,
read [README.md](README.md), [docs/DESIGN.md](docs/DESIGN.md), and
[docs/USER_JOURNEY.md](docs/USER_JOURNEY.md).

## Keep the sync contract safe

`sync` must be deterministic, idempotent, and conservative with user files.
Never delete or replace a user-owned path. Only remove symlinks recorded in the
manifest and still pointing into the uniskill cache.

Failed bundle builds are failures, not removals. Preserve the previous cache
and previous managed links when a source cannot be fetched, a source is invalid,
or a harness is unknown.

A worktree sync (`sync --worktree`) links the already-assembled cache into a
linked git worktree; it never fetches. It keeps a separate, worktree-scoped
manifest so a later main sync never prunes worktree links and a worktree sync
never prunes the main tree's. Installing the git hook (`hook install`) must never
overwrite a `post-checkout` hook uniskill did not write, and the global
dispatcher must chain to a repo's own hooks rather than shadow them.

## Keep responsibilities separated

Modules own narrow jobs:

| Module | Job |
| --- | --- |
| `cli.rs` | CLI parsing, config discovery, report printing, exit codes |
| `config.rs` | TOML shapes, source validation, env var and path resolution |
| `fetcher.rs` | Materializing local, URL, and git sources into the bundle cache |
| `harnesses.rs` | Built-in harness registry |
| `hook.rs` | Installing the `post-checkout` git hook and global dispatcher |
| `linker.rs` | Harness symlink creation and status classification |
| `skill.rs` | Shared skill-directory predicate |
| `state.rs` | Manifest load/save and managed-link ownership checks |
| `sync.rs` | Sync orchestration and structured reports |
| `worktree.rs` | Git worktree topology and harness-path retargeting |

Do not move filesystem mutation, output formatting, and config parsing into the
same function. If a change crosses those boundaries, add a small API between
the modules instead.

## Keep contracts in one place

Each shared fact has one owner:

- Rust version: `rust-toolchain.toml`
- Built-in harnesses: `harnesses::default_harnesses()`
- Source shape: `config::SourceSpec` and `config::Source`
- Skill directory rule: `skill::is_skill_dir`
- Release gate: `make release`
- GitHub release asset naming: `.github/workflows/release.yml`

Do not duplicate those values in Dockerfiles, CI, docs, or tests. Consume the
owner directly, or explain why a temporary copy is unavoidable.

## Validate config at the boundary

Reject invalid config before doing filesystem work. A source must declare
exactly one of `source`, `repo`, or `url`. `ref` and `path` only apply to
`repo`. A whole-bundle source cannot use `url` because a URL source is one
`SKILL.md` file, not a bundle directory.

## Keep source variants local

New source kinds should add a new variant and local materialization logic. They
should not add conditionals through `cli`, `linker`, or tests that do not own
source fetching.

New harnesses should be config-only unless they are built-ins. New built-ins go
only in `harnesses::default_harnesses()`.

## Write meaningful tests

Tests should prove behavior, ownership, and failure modes. Add regression tests
for any change that affects:

- cache replacement
- pruning
- symlink conflict handling
- source validation
- git fetch/update behavior
- release or CI contracts

Do not write tests that only mirror implementation details.

## Run the checks

Before committing code, run:

```bash
cargo fmt -- --check
cargo clippy --all-features -- -D warnings
cargo test --all-features
```

Before tagging a release, run:

```bash
make release
```

`make release` is the local release gate. The GitHub release workflow must use
the same contract.

## Write docs people can use

Lead with the current contract. Use direct sentences, active voice, and exact
file names, commands, paths, and states. Put facts in the document that owns
them and link instead of repeating them.

Do not write history in docs or comments. Use git history and release notes for
history.

## Use Conventional Commits

Use Conventional Commits for every commit: `feat:`, `fix:`, `docs:`, `ci:`,
`refactor:`, `test:`, `build:`, or `chore:`. The release notes generator reads
conventional subjects; an unconventional release commit can produce empty
notes.

## Releases are tag-based

The package version lives in `Cargo.toml` and `Cargo.lock`. A release tag must
match that version as `vX.Y.Z`.

Release flow:

1. Bump `Cargo.toml` and `Cargo.lock`.
2. Run `make release`.
3. Commit with a conventional subject.
4. Create an annotated `vX.Y.Z` tag on the validated commit.
5. Push the commit and tag.
6. Confirm CI and the Release workflow complete successfully.
7. Confirm the GitHub release has non-empty notes and all expected assets.

## Keep generated and local files out of git

Do not commit `target/`, `dist/`, `.uniskill-cache/`, `.agents/`, local config,
editor files, or machine-specific paths. Update `.gitignore` when a new local
artifact appears.

## Docs win on conflict

If this file conflicts with `README.md`, `docs/`, or the code contract, the
more specific source wins. Update the stale file in the same change.
