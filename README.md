# uniskill

Wire named skill groups into multiple agent harnesses. A bundle is a routing
layer: it names a set of skills and says which harnesses receive them. A bundle
can be pulled whole from a remote, composed from individual skills, or both.

## What It Does

uniskill reads a config file, assembles each declared bundle into a local cache,
and creates symlinks at every target harness path. A single bundle can install to
the Pi harness (`~/.agents/skills/`), Claude Code (`~/.claude/skills/`), or any
custom harness you define.

A bundle draws skills from two composable layers:

- **Whole-bundle source** — put `source` or `repo` + `path` on the bundle to
  point at a bundle directory (one holding a `skills/` folder) and pull every
  skill it declares as a unit. Add a skill upstream and the next sync picks it up.
- **Explicit skills** — `[bundles.<name>.skills.<skill>]` entries that add to, or
  override by name, whatever the bundle source provided.

The same source vocabulary applies to a whole bundle or a single skill:

- `source`: local directory
- `repo` (+ optional `ref`, `path`): git repository, optionally narrowed
- `url`: HTTP(S) URL for a single `SKILL.md` (skills only)

A local `source` is **linked live** by default (`link = true`): the harness
symlinks straight to your working tree, so edits from any harness land in the
source and `git pull` is live — re-sync only to add or remove a skill. Set
`link = false` on a bundle to copy instead. Remote `repo` and `url` sources are
always copied.

## What It Does Not Do

uniskill does not publish skills, handle semver dependency solving, or modify
harness configuration beyond creating and updating symlinks.

## Quick Start

### Install

**From source (requires [rustup](https://rustup.rs/)):**

```bash
make install
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
install -m 755 uniskill-darwin-arm64-v<version> ~/.local/bin/uniskill

# Or use the tarball:
tar xzf uniskill-darwin-arm64-v<version>.tar.gz
mv uniskill ~/.local/bin/
```

### Configure

Create a global config at `~/.config/uniskill/config.toml`:

```toml
[bundles.generic]
harnesses = ["pi", "claude-code"]

[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

Or a project config at `<repo-root>/uniskill.toml`:

```toml
[harnesses.local-agent]
pattern = ".agents/skills/{name}"

[bundles.project-tools]
harnesses = ["local-agent"]

[bundles.project-tools.skills.release-helper]
source = "./skills/release-helper"
```

### Sync

```bash
uniskill sync
```

Running `sync` twice reports "ok" for every already-correct skill. Exit code 1
indicates conflicts or broken symlinks.

## Skill Structure

Local and git-backed skills point at a skill directory:

```text
skill-name/
├── SKILL.md
└── agents/
    └── openai.yaml
```

Only `SKILL.md` is required. Extra files are copied into the cache and exposed to
the harness through the symlink.

URL-backed skills fetch a single `SKILL.md` into the cache.

## Config Reference

### Global Config

Global config lives at `~/.config/uniskill/config.toml` unless `--config` is
provided.

| Key | Type | Description |
|-----|------|-------------|
| `bundles.<bundle>.harnesses` | array | Harness names that receive this bundle |
| `bundles.<bundle>.skills.<skill>.source` | string | Local skill directory |
| `bundles.<bundle>.skills.<skill>.url` | string | HTTP(S) URL for `SKILL.md` |
| `bundles.<bundle>.skills.<skill>.repo` | string | Git repository containing the skill |
| `bundles.<bundle>.skills.<skill>.ref` | string | Optional branch, tag, or commit |
| `bundles.<bundle>.skills.<skill>.path` | string | Skill directory path inside `repo` |
| `harnesses.<name>.pattern` | string | Target pattern containing `{name}` |
| `harnesses.<name>.label` | string | Optional display label |

Each skill must declare exactly one source kind: `source`, `url`, or `repo`.
Git-backed skills must also declare `path`.

GitHub shorthands such as `owner/repo`, `gh:owner/repo`, and
`github:owner/repo` resolve to SSH URLs. Plain SSH, HTTPS, and local git paths
are passed through.

### Project Config

Project config is `uniskill.toml` in the current working directory. Relative
`source`, local `repo`, and custom harness paths resolve against the project
root.

Built-in harnesses can be referenced directly by name. User-defined harnesses
override built-ins with the same name.

## Environment Variables

Paths support `$VAR` and `${VAR}` expansion. Unresolvable variables are left
unchanged, so use variables that exist on every target machine.

## Built-In Harnesses

| Name | Pattern | Scope |
|------|---------|-------|
| `pi` | `$HOME/.agents/skills/{name}` | global |
| `claude-code` | `$HOME/.claude/skills/{name}` | global |

## Release Process

`make release` is the local release gate. It checks formatting, runs clippy,
runs tests, builds the release binary, and writes distributable assets to
`dist/`:

```bash
make release
```

Tag releases with `v<version>` matching `Cargo.toml`. The GitHub release
workflow runs the same gate for each supported target, publishes raw binaries,
tarballs, and `checksums-sha256.txt`.

## Sync Behavior

- **Idempotent**: existing symlinks report "ok" on re-run
- **Updated**: symlink replaced because it points to the wrong source or is broken
- **Conflict**: non-symlink file exists at target, so the skill is skipped
- Unmanaged symlinks remain untouched

## Design Docs

- [Design overview](docs/DESIGN.md)
- [User journeys](docs/USER_JOURNEY.md)
