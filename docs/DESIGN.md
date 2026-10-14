# Design - uniskill

uniskill routes skills and MCP definitions into configurable agent harnesses.
Bundles choose skill destinations and sources; MCP servers choose native
configuration destinations. Harness identities never select engine behavior.

> See [USER_JOURNEY.md](USER_JOURNEY.md) for concrete global, project-level, and
> custom harness workflows.

## Scope

uniskill manages skill links and owned MCP entries. Native configuration
contracts, ownership, and recovery are documented in [MCP.md](MCP.md).

## Core model

### Bundle

A bundle is a routing layer that assembles skills from two composable layers
into one destination policy:

```toml
[bundles.generic]
harnesses = ["pi", "claude-code"]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic"

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

1. An optional **whole-bundle source** (`source`/`repo`+`path`) points at a
   bundle directory, one containing a `skills/` folder, and pulls every skill
   under it as a unit.
2. **Explicit skill entries** under `[bundles.<name>.skills.<skill>]` add to, or
   override by name, whatever the bundle source provided.

A bundle needs at least one layer. The bundle key is stable identity for logging
and cache paths, not a source path, so a single bundle can mix local, URL, and
git-backed skills while retaining one destination policy.

### Link vs copy

`link` (default `true`) controls how a **local** `source` reaches the assembled
bundle:

- `link = true`: the assembled skill is a symlink at the source working tree, so
  edits made through a harness land in the source and `git pull` is live. Run
  `sync` again only to add or remove a skill.
- `link = false`: the local source is copied, snapshotting it.

Remote `repo` and `url` sources are always copied regardless of `link`: the git
cache and a downloaded file are not working trees to edit against.

### Source

The same source vocabulary describes where a whole bundle or a single skill
comes from:

- `source`: local directory
- `repo` (+ optional `ref`, `path`): git repository, optionally narrowed to a
  subdirectory at a branch, tag, or commit
- `url`: HTTP(S) URL to one `SKILL.md` (skills only; a url is a single file,
  not a bundle)

Exactly one of `source`, `repo`, or `url` may be set. `ref`/`path` require a
`repo`. Git `path` is relative to the repository root; absolute paths or ones
that escape with `..` are rejected. When a git source omits `path`, it resolves
to the repository root.

### Skill

A skill entry is keyed by the installed skill name and carries one source:

```toml
[bundles.generic.skills.code-architecture]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-architecture"
```

## Source types

### Local skill

```toml
[bundles.project.skills.release-helper]
source = "./skills/release-helper"
```

The source directory must contain `SKILL.md`. Relative paths resolve against the
config file's directory.

### URL skill

```toml
[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

The URL body is cached as `SKILL.md`. URL skills cannot carry companion files
unless those files are later represented as a richer source type.

### Git skill

```toml
[bundles.generic.skills.technical-writing]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/technical-writing"
```

uniskill clones or fetches the repository into the cache, checks out `ref` when
provided, and copies the selected skill directory into the assembled bundle.

GitHub shorthands (`owner/repo`, `gh:owner/repo`, `github:owner/repo`) resolve
to SSH URLs. Plain SSH, HTTPS, and local git paths are passed through.

### Whole bundle

```toml
[bundles.generic]
harnesses = ["pi", "claude-code"]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic"
```

The `source`/`repo` sits on the bundle itself and points at a bundle directory,
one containing a `skills/` folder. A `meta.toml` alongside it is allowed and
ignored. Every immediate subdirectory of `skills/` that contains a `SKILL.md` is
copied into the assembled bundle. A `url` is not a valid whole-bundle source
because a url is a single file, not a directory tree.

## Harnesses

A harness defines where an agent expects a skill directory to exist:

```toml
[harnesses.company-agent]
label = "Company Agent"
pattern = "$HOME/.company-agent/skills/{name}"
```

`{name}` is replaced with the skill key. Built-in harnesses are loaded first and
user config can override them.

[src/harnesses.toml](../src/harnesses.toml) owns the built-in destinations.
Custom harnesses may provide a skill pattern, an MCP target, or both.
The registry resolves built-in paths only for destinations used by the config
and not overridden by a custom definition. Unused defaults require no environment
variables.

## Git worktrees

A linked git worktree is a fresh checkout of tracked files only. The skill
symlinks `sync` installs live in ignored directories (`.agents/`,
`.claude/skills/`), so they never appear in a new worktree. The trigger is git's own
`post-checkout` hook, which fires in the new worktree on `git worktree add` (and
clone) whatever tool ran it.

`uniskill hook install` writes that hook; `--global` installs a `core.hooksPath`
dispatcher that shims every client hook name and chains to a repo's own hooks so
none are shadowed. The hook runs `uniskill sync --worktree`.

`sync --worktree` links the cache a normal sync already assembled into the
current worktree. It resolves the same bundles and harnesses, then for each
harness whose target lies inside the repository's main worktree it rewrites that
prefix onto the worktree root (`worktree.rs`). Harnesses outside the main
worktree (a machine-global path or another repo) are skipped, since a global
skills directory is already visible from every worktree. It never fetches, which
keeps the hook fast and offline-safe; a bundle the main sync has not assembled
yet is reported and skipped.

Retargeted skill and MCP destinations must remain inside the physical worktree
after resolving symlink parents. The same boundary applies to loaded ownership,
transaction recovery, and state directories; an outside path fails the sync.

## Config resolution

Global config is read from `~/.config/uniskill/config.toml`, unless `--config`
is provided. Project config is `uniskill.toml` in the current working directory.

Relative paths resolve against the config that declared them:

- `source` for local skills
- local `repo` values such as `../agent-skills`
- harness patterns such as `.agents/skills/{name}`
- native MCP file paths
- included config files

Included files share the global/project shape and retain their own path bases.
Duplicate bundle, harness, or server identities across includes are rejected.
See [README.md](../README.md#environment-variables) for variable expansion.

## Data flow

1. Load the built-in destination registry and resolve the config/includes.
2. Validate and reconcile MCP entries as a separate journaled transaction.
3. Lock the skill cache and load the previous link manifest. Normalize loaded
   destinations with the same parent resolution used for planned routes.
4. Validate bundle sources and skill destinations, and restore interrupted
   cache publications before downloading.
5. Deduplicate remote sources and fetch them with bounded concurrency. Jobs
   report durations; failed jobs retain each affected bundle's prior cache.
6. Stage bundles in sorted order. Local sources link live or copy according
   to `link`; explicit skills override whole-bundle entries by name.
7. Plan every skill destination before publication. Equivalent routes to one
   cache entry coalesce; competing entries and nested skill roots fail their
   bundles before ownership writes or cache publication. Existing roots also
   participate in overlap checks.
8. Persist ownership intent for existing and planned links. Compare staged and
   published trees, retaining identical caches and publishing accepted changes.
9. Reconcile each harness link once, preserve failed bundles' ownership, and
   prune recorded links that still point into the cache.
10. Persist the resulting ownership manifest. Unlink failures retain their
    records for retry. Every manifest write is atomic and skips identical bytes.

Skill and MCP operations have separate ownership and failure boundaries. A
skill fetch failure does not roll back a completed MCP transaction.

Skill reconciliation permits incremental progress across bundles. Its durable
ownership intent is a conservative superset until application finishes, so an
interrupted apply can reconcile or prune partially installed links on retry.
Unreadable or malformed ownership state stops skill sync before cache or link
mutation. State persistence failures return a failed sync. Both ownership engines
use bounded regular-file reads and reject symlinked state files.

MCP requires a journal because it edits shared native documents and must restore
their before-images. Skills retain their own cache/link lifecycle and share only
bounded reads, atomic file replacement, and destination path resolution with MCP.
A transaction spanning caches and links would also need durable link and directory
before-images;
revisit that boundary if all-or-nothing skill application becomes a requirement.

For a linked local source the harness symlink resolves through the cache entry
to the working tree, so the cache is an index of live links rather than copies.

## State and pruning

uniskill records every link it installs in `state.toml` next to the assembled
bundles. On the next sync it removes links absent from the config, such as
a skill dropped from a bundle or a whole bundle removed from the config. Config is
the source of truth for what stays installed.

Pruning is deliberately conservative. A link is removed only when it is still a
symlink pointing into uniskill's own cache, so hand-placed directories and
foreign symlinks in a harness are never touched. A link whose bundle failed to
build this run is kept, not pruned: a build error is a failure, not a removal.

A configured bundle reserves its recorded destinations until it successfully
retires those links. Another bundle cannot take them during that sync, including
when the owner fails after planning. To hand off a destination in one apply,
remove its former bundle from the config. If that bundle remains configured,
first sync its removal of the skill, then sync the new owner. Nested destinations
require the old root to be pruned before applying the new route, even within
one bundle.

Overlap checks include each harness entry, its full symlink chain, its publication
directory, and the directory the skill exposes. Every skill reserves its future
cache entry before publication. Live skills also expose their source directories;
staged copies expose their published cache paths. Publication waits for installed
harness claims at or below that directory to retire.
Resolved destination ancestors must not enter those roots, even through another
alias. Multiple harness entries may expose the same directory. Ownership and
pruning still use entry paths without following the final symlink.
Missing directories retain reservations from their existing symlink chains.

`cache::Layout` resolves the cache root and bundles storage directory before
deriving publication paths. Staging, worktree readers, recovery, and pruning share
that layout, so directory aliases retain one storage identity. Ownership checks
preserve the final skill entry when it links to a live source. Keeping layout
separate from materialization lets ownership checks use storage identity without
depending on source-fetching code.

Applied artifacts and ownership manifests remain unchanged on a converged
sync. Temporary staging and mutable remote Git metadata can change during
source checks; fetch timing varies between runs.

Global cache uses an absolute `$XDG_CACHE_HOME` when set, otherwise the platform
cache directory (`~/.cache` on Linux). `./.uniskill-cache` is the fallback when
no platform cache directory is available. Project config uses `.uniskill-cache/` next to the project
config.

A worktree sync writes its manifest to `.uniskill-cache/state.toml` inside the
worktree, not the shared `state.toml`. Main and worktree runs therefore prune
only their own links: a main sync never removes a worktree's links, and a
worktree sync never removes the main tree's. Removing a worktree also removes
its manifest.

Assembled bundle layout:

```text
cache/
└── bundles/
    └── generic/
        └── skills/
            └── code-architecture/
                └── SKILL.md
```

Git repositories are cached separately under `cache/repos/`. One normalized
repository/ref identity owns both download deduplication and checkout naming;
an omitted ref is distinct from every explicit ref. Cache directories are
derived data and may be rebuilt when their naming scheme changes.
Staging and recovery names use bounded sibling components, so accepted bundle
keys through 255 bytes retain the same install, update, and recovery lifecycle.

## Symlink strategy

Symlinks point at absolute paths inside the assembled cache. A sync run:

- creates missing symlinks
- keeps symlinks that already point at the expected source
- updates symlinks pointing elsewhere
- refuses to replace non-symlink files or directories

Unmanaged symlinks remain untouched unless they conflict with a declared target.
