# Design - uniskill

uniskill wires named groups of skills into agent harnesses by symlinking from an
assembled cache. The config is explicit: bundles choose destinations, and skills
choose sources.

> See [USER_JOURNEY.md](USER_JOURNEY.md) for concrete global, project-level, and
> custom harness workflows.

## Scope

uniskill manages skill installation paths. It does not publish skills, solve
versions, infer a repository layout, or edit harness configuration files.

## Core Model

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
   bundle directory — one containing a `skills/` folder — and pulls every skill
   under it as a unit.
2. **Explicit skill entries** under `[bundles.<name>.skills.<skill>]` add to, or
   override by name, whatever the bundle source provided.

A bundle needs at least one layer. The bundle key is stable identity for logging
and cache paths, not a source path, so a single bundle can mix local, URL, and
git-backed skills while retaining one destination policy.

### Source

The same source vocabulary describes where a whole bundle or a single skill
comes from:

- `source`: local directory
- `repo` (+ optional `ref`, `path`): git repository, optionally narrowed to a
  subdirectory at a branch, tag, or commit
- `url`: HTTP(S) URL to one `SKILL.md` (skills only — a url is a single file,
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

## Source Types

### Local Skill

```toml
[bundles.project.skills.release-helper]
source = "./skills/release-helper"
```

The source directory must contain `SKILL.md`. Relative paths resolve against the
config file's directory.

### URL Skill

```toml
[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

The URL body is cached as `SKILL.md`. URL skills cannot carry companion files
unless those files are later represented as a richer source type.

### Git Skill

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

### Whole Bundle

```toml
[bundles.generic]
harnesses = ["pi", "claude-code"]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic"
```

The `source`/`repo` sits on the bundle itself and points at a bundle directory —
one containing a `skills/` folder (a `meta.toml` alongside it is allowed and
ignored). Every immediate subdirectory of `skills/` that contains a `SKILL.md`
is copied into the assembled bundle. A `url` is not a valid whole-bundle source
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

## Config Resolution

Global config is read from `~/.config/uniskill/config.toml`, unless `--config`
is provided. Project config is `uniskill.toml` in the current working directory.

Relative paths resolve against the config that declared them:

- `source` for local skills
- local `repo` values such as `../agent-skills`
- project harness patterns such as `.agents/skills/{name}`

Environment variables use `$VAR` and `${VAR}` syntax. Unresolvable variables are
left unchanged.

## Data Flow

1. Load built-in harnesses.
2. Load config and merge custom harnesses.
3. For each bundle, clear and recreate its assembled cache directory.
4. If the bundle has a whole-bundle source, copy every skill under its `skills/`
   folder into the bundle cache.
5. For each explicit skill entry, fetch or copy its source into the bundle
   cache, adding to or overriding the whole-bundle skills by name.
6. For each bundle-harness pair, create or update symlinks to cached skills.

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

## Symlink Strategy

Symlinks point at absolute paths inside the assembled cache. A sync run:

- creates missing symlinks
- keeps symlinks that already point at the expected source
- updates symlinks pointing elsewhere
- refuses to replace non-symlink files or directories

Unmanaged symlinks remain untouched unless they conflict with a declared target.
