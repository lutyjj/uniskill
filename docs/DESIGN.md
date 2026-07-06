# Design — uniskill

uniskill wires skill bundles from their source directory into multiple agent harnesses via symlinks. One bundle, installed to whatever harnesses you declare. The tool handles path resolution automatically.

> See [USER_JOURNEY.md](file:///Users/lutyjj/workplace/i-hate-agents-md/docs/USER_JOURNEY.md) for concrete examples of global, project-level, and custom harness workflows.

## Scope

uniskill manages **skill bundles**. A bundle is a self-contained directory with a `meta.toml` and a `skills/` subdirectory. The tool reads the bundle, discovers its skills, and creates symlinks at each harness's expected location.

uniskill does not:
- Manage individual skill files outside of bundles
- Publish or distribute bundles (though bundles can be shared manually)
- Modify existing harness configuration beyond creating/removing symlinks
- Handle version pinning or semver resolution

## Fundamental entity: bundle

A bundle is a group of skills wired into one or more harnesses. A bundle can be **local** (a source directory on disk) or **virtual** (skills fetched from remote URLs).

### Local bundle

Source directory structure:

```
bundle-name/
├── meta.toml (optional)
└── skills/
    └── <skill-name>/
        └── SKILL.md
```

uniskill auto-discovers all subdirectories of `skills/` as skill sources. No manual listing required.

### Virtual bundle

Skills defined by URL instead of a local directory:

```toml
[bundles.remote-skill]
harnesses = ["pi"]

[bundles.remote-skill.skills.caveman]
url = "https://example.com/skills/caveman/SKILL.md"
```

uniskill downloads each URL into a local cache (`~/.cache/uniskill/` for global config, `.uniskill-cache/` relative to project root for project-level config), then wires the cached files into harnesses exactly like local bundles. Cached skills survive across sync runs — only changed URLs are re-fetched.

### Bundle config key

Bundles use `[bundles.<name>]` (map) rather than `[[bundles]]` (array). Each map key is the bundle name and becomes part of downstream paths (cache directory, log messages). This lets multiple bundles coexist in one config without ambiguity, and keeps skill entries grouped under their bundle.

## Installation model

You declare bundles under `[bundles.<name>]` and specify which harnesses to wire them into. The harness registry resolves target paths.

### Local bundle example

```toml
[bundles.my-skills]
source = "/home/user/repos/my-skills"
harnesses = ["pi", "claude-code"]
```

The tool creates symlinks:

```
/home/user/repos/my-skills/skills/caveman → $HOME/.agents/skills/caveman (for pi)
/home/user/repos/my-skills/skills/caveman → $HOME/.claude/skills/caveman  (for claude-code)
```

### Virtual bundle example

```toml
[bundles.remote-skill]
harnesses = ["pi"]

[bundles.remote-skill.skills.caveman]
url = "https://example.com/caveman.md"
```

uniskill downloads `caveman` into the local cache, then creates:

```
~/.cache/uniskill/remote-skill/skills/caveman → $HOME/.agents/skills/caveman
```

Different harnesses use different conventions. The tool knows each harness's expected path pattern and resolves it at runtime using environment variables like `$HOME`.

## Harness definitions & Scopes

A harness defines where a particular agent expects its skills to live. Instead of a hardcoded registry, harnesses are configured dynamically. `uniskill` ships with built-in defaults for known global harnesses, but users can extend or override them.

The critical concept is **Scope**:
- **Global**: The harness operates system-wide (e.g., its path pattern is absolute, typically rooted in `$HOME`).
- **Project**: The harness operates only within a specific repository (e.g., its path pattern is relative to the project root, like `.claude/skills`).

Users can define custom harnesses directly in their configuration files:

```toml
[harnesses.company-agent]
scope = "global"
pattern = "$HOME/.company-agent/skills/{name}"

[harnesses.local-claude]
scope = "project"
pattern = ".claude/skills/{name}"
```

## Config format & Resolution

Configuration is merged from two layers, allowing seamless interaction between global tools and project-specific agents:

1. **Global Config**: `~/.config/uniskill/config.toml` (or `--config`)
2. **Project Config**: `uniskill.toml` in the current working directory.

```toml
# Project-level example with both local and virtual bundles

# Define custom harnesses (optional)
[harnesses.agents]
scope = "project"
pattern = ".agents/skills/{name}"

# Local bundle: source is a path on disk
[bundles.dev-tools]
source = "./my-project-skills" # Resolves relative to this config file
harnesses = ["pi", "agents"]

# Virtual bundle: skills fetched from URLs
[bundles.remote-caveman]
harnesses = ["agents"]

[bundles.remote-caveman.skills.caveman]
url = "https://raw.githubusercontent.com/example/caveman/main/SKILL.md"
```

When `uniskill sync` runs, it:
1. Loads the built-in default harnesses.
2. Loads and merges user-defined harnesses from the Global Config.
3. Loads and merges user-defined harnesses from the Project Config (if present).
4. Resolves `source` paths for local bundles (absolute or relative to the defining config).
5. For virtual bundles, downloads each skill URL into a local cache, then treats the cache as a normal bundle source.
6. Creates symlinks for all declared bundles across all scopes.

The tool auto-discovers skills from the `skills/` subdirectory of each local bundle source. Virtual bundle skills are downloaded in full and cached; they become real directories once fetched.

## CLI

```bash
uniskill sync           # create/update symlinks for all declared bundles
uniskill status         # show current symlink state vs expected
uniskill init <harness> # detect harness installation and add to registry
```

`sync` creates missing symlinks, updates broken ones, and leaves unmanaged symlinks untouched. Running it twice is idempotent.

`status` reports which skills are linked, which are expected but not linked, and which exist as stale symlinks (source bundle removed from config).

## Data flow

1. User edits `config.toml` to declare bundles and harnesses
2. User runs `uniskill sync`
3. Tool reads config, resolves harness patterns with env vars
4. For each bundle-harness pair, tool creates or updates the symlink
5. The symlink points to the actual skill directory in the bundle
6. Harness reads the skill through the symlink — no duplication

## Example: end-to-end

User has a skill repo at `/home/user/.dotfiles/skills/` with `meta.toml` and several skills. Config declares it for two harnesses:

```toml
[[bundles]]
source = "$HOME/.dotfiles/skills"
harnesses = ["pi", "claude-code"]
```

After `uniskill sync`:

- `/home/user/.agents/skills/caveman` → symlink to `/home/user/.dotfiles/skills/skills/caveman`
- `/home/user/.claude/skills/caveman` → same target
- Editing the skill in one place updates it everywhere

## Environment variable expansion

All `source` paths and harness patterns support `$VAR` and `${VAR}` expansion. The tool resolves variables from the current process environment at runtime. This makes config portable across machines without manual editing.

Supported variables: `$HOME`, `$USER`, `$PATH`, or any other env var. Unresolvable variables cause a clear error during `sync`.

## Symlink strategy

Symlinks are absolute paths. They survive directory moves and work across tool invocations. The tool recreates symlinks on each sync to handle source bundle relocation gracefully.

A symlink is considered valid when:
- Its target exists
- Its target is not broken (source still present in declared bundles)

The tool does not follow or manage symlinks that it did not create.

## Cache lifecycle (virtual bundles)

Downloaded skills live in a project-local cache (`.uniskill-cache/` relative to the project root) for project configs, or `$XDG_CACHE_HOME/uniskill/` for global config. Cached files persist across sync runs; only changed URLs are re-fetched.

If a user deletes the cache directory, cached skills become broken symlinks — the next `sync` re-downloads them. There is no automatic TTL or size limit in v1.
