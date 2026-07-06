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

A bundle is a routing layer:

```toml
[bundles.generic]
harnesses = ["pi", "claude-code"]
```

The bundle key is stable identity for logging and cache paths. It is not a
source path. This lets a bundle mix local, URL, and git-backed skills while
retaining one destination policy.

### Skill

A skill entry is keyed by the installed skill name:

```toml
[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"
```

Each skill must declare exactly one source kind:

- `source`: local skill directory
- `url`: HTTP(S) URL to one `SKILL.md`
- `repo`: git repository containing the skill directory

Git-backed skills require `path`, relative to the repository root. Paths that
are absolute or escape with `..` are rejected.

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
4. For each skill, fetch or copy its declared source into the bundle cache.
5. For each bundle-harness pair, create or update symlinks to cached skills.

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
