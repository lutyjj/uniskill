# uniskill user journeys

These examples show the intended config shape: bundles group skills for a set of
harnesses. A bundle can be pulled whole from a remote directory, composed from
individual skills, or both.

## Journey 1: Whole bundle from a remote

**Goal**: Point at one bundle in a git repo and get all of its skills, with an
extra external skill layered on top.

```toml
# ~/.config/uniskill/config.toml

[bundles.generic]
harnesses = ["pi", "claude-code"]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic"

# caveman is not vendored in the repo. Layer it on from upstream.
[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

`uniskill sync` clones the repo, copies every skill under `bundles/generic/skills/`
into the cache, adds `caveman`, then links each into `$HOME/.agents/skills/{name}`
and `$HOME/.claude/skills/{name}`. Adding a skill to the bundle upstream needs no
config change. The next sync picks it up.

## Journey 1b: Live editing from a local clone

**Goal**: Clone the skills repo, run uniskill against the clone, and edit skills
from any harness. Commit and push from the repo when changes are ready.

```toml
# agent-skills/configs/global.toml (skills fragment)

[bundles.generic]
harnesses = ["pi", "claude-code"]
source = "../bundles/generic"   # relative to this config; linked live by default

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

Because a local `source` links straight to the working tree, editing
`~/.agents/skills/code-architecture/SKILL.md` edits `agent-skills/bundles/generic/skills/code-architecture/SKILL.md`
directly. `git pull` is live in every harness; you only re-run `sync` when a
skill is added or removed. When changes are ready, commit and push from the repo.
`caveman` stays a copied `url` skill because only local sources link.

On a skills-only machine, point `~/.config/uniskill/config.toml` at this file.
With MCP catalogs, include it in the [complete machine wrapper](#journey-7-one-private-mcp-catalog-for-several-agents).
Use that complete config when adding or removing skills:

```bash
uniskill --config "$HOME/.config/uniskill/config.toml" sync
```

## Journey 2: Custom global harness

**Goal**: Add an experimental agent framework without changing the binary.

```toml
[harnesses.alpha-agent]
pattern = "$HOME/.config/alpha-agent/plugins/{name}"

[bundles.generic]
harnesses = ["alpha-agent", "pi"]

[bundles.generic.skills.technical-writing]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/technical-writing"
```

The custom harness becomes another destination for the same bundle.

## Journey 3: Project-level skills

**Goal**: Add skills only for a repository-local agent harness.

```toml
# /workspace/monorepo/uniskill.toml

[harnesses.local-claude]
pattern = ".claude/skills/{name}"

[bundles.project-tools]
harnesses = ["local-claude"]

[bundles.project-tools.skills.release-helper]
source = "./scripts/agent-skills/release-helper"
```

Running `uniskill sync` inside the project resolves the harness and `source`
relative to the project root. Global harnesses remain untouched.

## Journey 4: Global skills in a project harness

**Goal**: Use a global skill repository inside a scoped project harness.

```toml
# /workspace/isolated-project/uniskill.toml

[harnesses.local-agent]
pattern = ".agents/skills/{name}"

[bundles.generic]
harnesses = ["local-agent"]

[bundles.generic.skills.code-architecture]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-architecture"
```

The project receives the selected global skills without touching global agent
directories.

## Journey 5: Overriding a built-in harness

**Goal**: Use a non-standard Claude Code skills directory.

```toml
[harnesses.claude-code]
pattern = "/opt/claude/skills/{name}"

[bundles.generic]
harnesses = ["claude-code"]

[bundles.generic.skills.code-architecture]
source = "$HOME/workplace/agent-skills/bundles/generic/skills/code-architecture"
```

User-defined harnesses override built-in harnesses with the same name.

## Journey 6: Mixed skill sources

**Goal**: Keep a single bundle layer while sourcing skills from different places.

```toml
[bundles.generic]
harnesses = ["pi"]

[bundles.generic.skills.code-architecture]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-architecture"

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"

[bundles.generic.skills.local-experiment]
source = "./skills/local-experiment"
```

The bundle remains the policy layer. Individual skill entries retain full source
control.

## Journey 7: One private MCP catalog for several agents

**Goal**: Keep service definitions in a private local file while reusing a
versioned skills config.

```toml
# ~/.config/uniskill/config.toml
includes = ["skills.toml", "mcp-private.toml"]
```

```toml
# ~/.config/uniskill/mcp-private.toml
[mcp.docs]
harnesses = ["pi", "claude-code", "codex"]
url = "https://example.com/mcp"
bearer_token_env_var = "DOCS_TOKEN"

[mcp.docs.options.pi]
exposure = "direct"
```

Select the complete wrapper explicitly, including from a project directory:

```bash
uniskill --config "$HOME/.config/uniskill/config.toml" sync --check-mcp
uniskill --config "$HOME/.config/uniskill/config.toml" sync
```

Matching existing entries require `--adopt-mcp` once.
Keep private files outside Git. See [MCP.md](MCP.md) for ownership conflicts,
reference handling, and native config precedence.
