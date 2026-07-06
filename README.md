# uniskill

Wire skill bundles from one source into multiple agent harnesses via symlinks. One bundle, installed wherever you declare it.

## What it does

uniskill reads a **bundle** (a directory with `meta.toml` and `skills/`) and creates symlinks at each declared harness's expected location. A single bundle can install to the pi harness (`~/.agents/skills/`), claude-code (`~/.claude/skills/`), or any custom harness you define.

Editing a skill in the source bundle updates it everywhere through the symlink. No file copying, no duplication.

## What it does not do

uniskill does not publish bundles, manage individual skill files outside bundles, handle version pinning, or modify harness configuration beyond creating and updating symlinks.

## Quick start

### Install

**From source (requires [rustup](https://rustup.rs/)):**

```bash
make install          # builds release binary and copies to ~/.local/bin/
```

Or manually:

```bash
cargo build --release
cp target/release/uniskill ~/.local/bin/
```

**From a GitHub Release:**

Download the latest asset for your platform from [Releases](../../releases).
Raw binaries are attached for direct installs, and tarballs are attached for
preserving executable metadata:

```bash
mkdir -p ~/.local/bin
install -m 755 uniskill-darwin-arm64-v0.1.0 ~/.local/bin/uniskill

# Or use the tarball:
tar xzf uniskill-darwin-arm64-v0.1.0.tar.gz
mv uniskill ~/.local/bin/
```

### Configure

Create a global config at `~/.config/uniskill/config.toml`:

```toml
[bundles.my-skills]
source = "$HOME/.dotfiles/skills"
harnesses = ["pi", "claude-code"]
```

Or a project config at `<repo-root>/uniskill.toml`:

```toml
[harnesses.agents]
pattern = ".agents/skills/{name}"

[bundles.my-bundle]
source = "./my-bundle"
harnesses = ["agents"]
```

### Sync

```bash
uniskill sync    # wire all declared bundles into their harnesses
```

Running `sync` twice reports "ok" for every skill — the operation is idempotent. Exit code 1 indicates conflicts or broken symlinks.

## Bundle structure

```
my-bundle/
├── meta.toml
└── skills/
    └── skill-name/
        └── SKILL.md
```

The `skills/` directory is the source of truth. uniskill auto-discovers every subdirectory as a skill — no per-skill configuration needed.

## Config reference

### Global config (`~/.config/uniskill/config.toml`)

Defines bundles and custom harnesses for system-wide use.

| Key | Type | Description |
|-----|------|-------------|
| `bundles.<name>` | object | Bundle declaration with `source` and `harnesses` fields |
| `harnesses.<name>` | object | Custom harness definition with `pattern` and optional `label` |

### Project config (`uniskill.toml`)

Same structure as global config. Automatically discovered in the current working directory. Relative `source` paths resolve against the project root.

Built-in harnesses can be referenced directly by name (e.g., `"pi"`). User-defined harnesses from either config file are merged into the registry before sync runs.

### Environment variable expansion

All paths support `$VAR` and `${VAR}` expansion resolved at runtime. Unresolvable variables are left unchanged, so use environment variables that are guaranteed to exist on the target machine.

## Built-in harnesses

| Name | Pattern | Scope |
|------|---------|-------|
| `pi` | `$HOME/.agents/skills/{name}` | global |
| `claude-code` | `$HOME/.claude/skills/{name}` | global |

Override any built-in harness by defining it in your config with the same name.

## CLI reference

| Command | Description |
|---------|-------------|
| `uniskill sync` | Create or update symlinks for all declared bundles |

## Release process

`make release` is the local release gate. It checks formatting, runs clippy,
runs tests, builds the release binary, and writes distributable assets to
`dist/`:

```bash
make release
```

Tag releases with `v<version>` matching `Cargo.toml`. The GitHub release
workflow runs the same gate for each supported target, publishes raw binaries,
tarballs, and `checksums-sha256.txt`.

## Sync behavior

- **Idempotent**: existing symlinks report "ok" on re-run
- **Updated**: symlink replaced because it points to the wrong source or is broken
- **Conflict**: non-symlink file exists at target — skill skipped with warning
- All unmanaged symlinks remain untouched

## Design docs

- [Design overview](docs/DESIGN.md) — architecture, harness scope, data flow
- [User journeys](docs/USER_JOURNEY.md) — global setup, project-level skills, custom harnesses
