# Design - uniskill

uniskill wires named groups of skills into agent harnesses by symlinking from an
assembled cache. The config is explicit: bundles choose destinations, and skills
choose sources.

> See [USER_JOURNEY.md](USER_JOURNEY.md) for concrete global, project-level, and
> custom harness workflows.

## Scope

uniskill manages skill installation paths. It does not publish skills, solve
versions, infer a repository layout, or edit harness configuration files.

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
[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"
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

Built-ins:

| Name | Pattern |
|------|---------|
| `pi` | `$HOME/.agents/skills/{name}` |
| `claude-code` | `$HOME/.claude/skills/{name}` |

## Config resolution

Global config is read from `~/.config/uniskill/config.toml`, unless `--config`
is provided. Project config is `uniskill.toml` in the current working directory.

Relative paths resolve against the config that declared them:

- `source` for local skills
- local `repo` values such as `../agent-skills`
- project harness patterns such as `.agents/skills/{name}`

Environment variables use `$VAR` and `${VAR}` syntax. Unresolvable variables are
left unchanged.

## Data flow

1. Load built-in harnesses.
2. Load config and merge custom harnesses.
3. Load the previous run's link manifest.
4. For each bundle **in sorted order**, validate its harnesses before changing
   the bundle cache. A bundle with an unknown harness is reported and skipped.
5. Assemble the bundle in a staging directory. If assembly fails, keep the
   previous bundle cache and preserve its manifest entries.
6. If the bundle has a whole-bundle source, place every skill under its `skills/`
   folder into the staged bundle. Local sources are symlinked when `link = true`
   and copied otherwise.
7. For each explicit skill entry, place its source into the staged bundle
   (clearing any same-named skill first), adding to or overriding the
   whole-bundle skills by name.
8. Promote the staged bundle cache over the previous bundle cache only after
   assembly succeeds.
9. For each bundle-harness pair, create or update symlinks to cached skills,
   recording each in the new manifest.
10. Prune: remove any link from the previous manifest that was not installed
   this run, then write the new manifest.

For a linked local source the harness symlink resolves through the cache entry
to the working tree, so the cache is an index of live links rather than copies.

## State and pruning

uniskill records every link it installs in `state.toml` next to the assembled
bundles. On the next sync it removes links that are no longer declared, such as
a skill dropped from a bundle or a whole bundle removed from the config. Config is
the source of truth for what stays installed.

Pruning is deliberately conservative. A link is removed only when it is still a
symlink pointing into uniskill's own cache, so hand-placed directories and
foreign symlinks in a harness are never touched. A link whose bundle failed to
build this run is kept, not pruned: a build error is a failure, not a removal.

Because output order and the installed set are both derived from sorted config
rather than filesystem iteration, repeated syncs of an unchanged config produce
identical output.

Global cache lives under `$XDG_CACHE_HOME/uniskill/` when available, otherwise
`./.uniskill-cache`. Project config uses `.uniskill-cache/` next to the project
config.

Assembled bundle layout:

```text
cache/
└── bundles/
    └── generic/
        └── skills/
            └── code-design/
                └── SKILL.md
```

Git repositories are cached separately under `cache/repos/`.

## Symlink strategy

Symlinks point at absolute paths inside the assembled cache. A sync run:

- creates missing symlinks
- keeps symlinks that already point at the expected source
- updates symlinks pointing elsewhere
- refuses to replace non-symlink files or directories

Unmanaged symlinks remain untouched unless they conflict with a declared target.
