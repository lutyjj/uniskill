# uniskill

Sync reusable skills and MCP server definitions into agent harnesses. A bundle is a routing
layer: it names a set of skills and says which harnesses receive them. A bundle
can be pulled whole from a remote, composed from individual skills, or both.

## What it does

uniskill reads a config file, assembles each declared bundle into a local cache,
and creates symlinks at every target harness path. A single bundle can install to
Pi, Claude Code, Codex, or any custom harness you define.

MCP definitions use the same routing model. Each server chooses harnesses; each
harness declares a native file, format, table path, and field mappings. Sync
edits only managed server entries and preserves unrelated settings. See the
[MCP reference](docs/MCP.md) for configuration, adoption, and recovery.

A bundle draws skills from two composable layers:

- **Whole-bundle source**: put `source` or `repo` + `path` on the bundle to
  point at a bundle directory (one holding a `skills/` folder) and pull every
  skill it declares as a unit. Add a skill upstream and the next sync picks it up.
- **Explicit skills**: `[bundles.<name>.skills.<skill>]` entries that add to, or
  override by name, whatever the bundle source provided.

The same source vocabulary applies to a whole bundle or a single skill:

- `source`: local directory
- `repo` (+ optional `ref`, `path`): git repository, optionally narrowed
- `url`: HTTP(S) URL for a single `SKILL.md` (skills only)

A local `source` is **linked live** by default (`link = true`): the harness
symlinks straight to your working tree, so edits from any harness land in the
source and `git pull` is live. Run `sync` again only to add or remove a skill. Set
`link = false` on a bundle to copy instead. Remote `repo` and `url` sources are
always copied.

`sync` is deterministic and conservative. Bundles are processed in sorted order.
A bundle that fails to build is reported and skipped. Links from a previous sync
absent from the config are pruned. Only uniskill-owned symlinks are
removed; hand-placed files are left alone.

## What it does not do

uniskill reads text sources and writes links or configuration. It does not run
MCP commands, install MCP packages, connect to servers, or manage OAuth tokens.
Skill publishing and dependency solving belong to their source tools.

## Quick start

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

**From a GitHub release:**

Download the latest asset for your platform from [Releases](../../releases).
Release assets include raw binaries for direct installs and tarballs that
preserve executable metadata:

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

[bundles.generic.skills.code-architecture]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-architecture"

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

Running `sync` twice reports "ok" for already-correct entries and preserves
applied file contents, symlinks, bundle caches, manifests, and modification
times. Mutable remote sources are checked for updates. Distinct downloads run
concurrently (default four); `sync --jobs 1` fetches serially. Progress shows the
plan, remote job durations, and assembly/link stages. Cached full commit IDs
require no network fetch. Exit code 1 indicates a failed sync.

## Git worktrees

A git worktree is a fresh checkout of tracked files only, so the skill symlinks
`sync` installs into a project (`.claude/skills/`, `.agents/skills/`) never
appear in a new worktree; every worktree starts without them. The trigger lives in git.

Install a `post-checkout` hook once. Every worktree created afterward by Claude
Code, Codex, `git worktree add`, or another tool then syncs itself:

```bash
# This repo only:
uniskill hook install

# Or machine-wide, for every repo and every future clone:
uniskill --config <global-config> hook install --global
```

`--global` sets git's `core.hooksPath` to a uniskill dispatcher and shims the
standard hook names so each repo's own hooks still run. It refuses to overwrite a
`core.hooksPath` you already set, and per-repo install refuses to overwrite a
`post-checkout` hook you wrote yourself.

The hook runs `uniskill sync --worktree`, which links the already-assembled cache
into the current worktree and retargets each repo-scoped harness path onto it
without refetching. Machine-global harness paths (`$HOME/.claude/skills`) are left
alone; they are already visible from every worktree. Worktree links are tracked
in a separate manifest, so a later `uniskill sync` in the main tree never prunes
them. You can also run `uniskill sync --worktree` by hand from inside a worktree.

## Skill structure

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

## Config reference

### Global config

Global config lives at `~/.config/uniskill/config.toml` unless `--config` is
provided.

| Key | Type | Description |
|-----|------|-------------|
| `bundles.<bundle>.harnesses` | array | Harness names that receive this bundle |
| `bundles.<bundle>.source` | string | Local bundle directory containing `skills/` |
| `bundles.<bundle>.repo` | string | Git repository containing a bundle directory |
| `bundles.<bundle>.ref` | string | Optional branch, tag, or commit for `repo` |
| `bundles.<bundle>.path` | string | Optional bundle path inside `repo` |
| `bundles.<bundle>.link` | boolean | Link local sources live when `true`; defaults to `true` |
| `bundles.<bundle>.skills.<skill>.source` | string | Local skill directory |
| `bundles.<bundle>.skills.<skill>.url` | string | HTTP(S) URL for `SKILL.md` |
| `bundles.<bundle>.skills.<skill>.repo` | string | Git repository containing the skill |
| `bundles.<bundle>.skills.<skill>.ref` | string | Optional branch, tag, or commit |
| `bundles.<bundle>.skills.<skill>.path` | string | Optional skill path inside `repo` |
| `harnesses.<name>.pattern` | string | Target pattern containing `{name}` |
| `harnesses.<name>.label` | string | Optional display label |
| `includes` | array of strings | Additional config files, resolved at the declaring file |
| `harnesses.<name>.mcp` | table | Native MCP destination; see [MCP reference](docs/MCP.md) |
| `mcp.<server>` | table | Shared MCP server definition and destinations |

Each skill must declare exactly one source kind: `source`, `url`, or `repo`.
Git-backed skills use the repository root when `path` is omitted. `ref` and
`path` are valid only with `repo`.

GitHub shorthands such as `owner/repo`, `gh:owner/repo`, and
`github:owner/repo` resolve to SSH URLs. Plain SSH, HTTPS, and local git paths
are passed through.

### Project config

Project config is `uniskill.toml` in the current working directory. Relative
`source`, local `repo`, and custom harness paths resolve against the project
root.

Built-in harnesses can be referenced directly by name. User-defined harnesses
override built-ins with the same name.

## Environment variables

Paths support `$VAR`, `${VAR}`, and `${VAR:-fallback}` expansion. Fallbacks may
contain `$VAR` references, but nested braces are unsupported. Unresolved MCP
target variables are rejected; use variables available on the target machine.

## Built-in harnesses

`pi`, `claude-code`, and `codex` provide skill and MCP destinations. Their paths
and native formats live in [src/harnesses.toml](src/harnesses.toml). Config can
override either destination while retaining the other.

## Release process

`make release` is the local release gate. It checks formatting, runs clippy,
runs tests, builds the release binary, and writes distributable assets to
`dist/`:

```bash
make release
```

Tag releases with `v<version>` matching `Cargo.toml`. Use a conventional commit
subject for the release commit so the changelog generator has content. The
GitHub release workflow runs the same gate for each supported target and
publishes raw binaries, tarballs, and `checksums-sha256.txt`.

## Sync behavior

- **Idempotent**: converged skills, native MCP files, and ownership state stay unchanged
- **Updated**: symlink replaced because it points to the wrong source or is broken
- **Conflict**: a non-symlink file or directory exists at the target, so the
  skill is skipped
- Unmanaged symlinks remain untouched unless a declared skill target requires replacement

## Design docs

- [Design overview](docs/DESIGN.md)
- [User journeys](docs/USER_JOURNEY.md)
- [MCP configuration and ownership](docs/MCP.md)
