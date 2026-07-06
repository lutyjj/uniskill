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

A bundle is a directory with this structure:

```
bundle-name/
├── meta.toml
└── skills/
    └── <skill-name>/
        └── SKILL.md
```

`meta.toml` contains the bundle's identity:

```toml
name = "my-skills"
type = "skill"
description = "Personal skill collection for agent harnesses"
```

The `skills/` directory holds individual skills. uniskill auto-discovers all subdirectories as skill sources. No manual listing required.

## Installation model

You declare a bundle and which harnesses it should install to. The harness registry resolves the target paths.

```toml
[[bundles]]
source = "/home/user/repos/my-skills"
harnesses = ["pi", "claude-code"]
```

The tool creates symlinks:

```
/source/bundle/skills/caveman → $HOME/.agents/skills/caveman (for pi)
/source/bundle/skills/caveman → $HOME/.claude/skills/caveman  (for claude-code)
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
# Example uniskill.toml (can be global or project-level)

# Define custom harnesses (optional)
[harnesses.my-custom-harness]
scope = "project"
pattern = ".agents/skills/{name}"

# Wire bundles into harnesses
[[bundles]]
source = "./my-project-skills" # Resolves relative to this config file
harnesses = ["pi", "my-custom-harness"] # 'pi' is a built-in global
```

When `uniskill sync` runs, it:
1. Loads the built-in default harnesses.
2. Loads and merges user-defined harnesses from the Global Config.
3. Loads and merges user-defined harnesses from the Project Config (if present).
4. Resolves the `source` paths (absolute or relative to the defining config).
5. Creates symlinks for all declared bundles across all scopes.

The tool auto-discovers skills from the `skills/` subdirectory of each source. No per-skill configuration needed.

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
