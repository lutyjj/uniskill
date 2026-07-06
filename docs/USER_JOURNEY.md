# uniskill User Journeys

These examples show the intended config shape: bundles group skills for a set of
harnesses, while each skill declares its own source.

## Journey 1: Global Skills

**Goal**: Make personal skills available to Pi and Claude Code.

```toml
# ~/.config/uniskill/config.toml

[bundles.generic]
harnesses = ["pi", "claude-code"]

[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"
```

`uniskill sync` assembles the bundle into the cache, then links each skill into
`$HOME/.agents/skills/{name}` and `$HOME/.claude/skills/{name}`.

## Journey 2: Custom Global Harness

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

## Journey 3: Project-Level Skills

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

## Journey 4: Global Skills In A Project Harness

**Goal**: Use a global skill repository inside a scoped project harness.

```toml
# /workspace/isolated-project/uniskill.toml

[harnesses.local-agent]
pattern = ".agents/skills/{name}"

[bundles.generic]
harnesses = ["local-agent"]

[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"
```

The project receives the selected global skills without touching global agent
directories.

## Journey 5: Overriding A Built-In Harness

**Goal**: Use a non-standard Claude Code skills directory.

```toml
[harnesses.claude-code]
pattern = "/opt/claude/skills/{name}"

[bundles.generic]
harnesses = ["claude-code"]

[bundles.generic.skills.code-design]
source = "$HOME/workplace/agent-skills/bundles/generic/skills/code-design"
```

User-defined harnesses override built-in harnesses with the same name.

## Journey 6: Mixed Skill Sources

**Goal**: Keep a single bundle layer while sourcing skills from different places.

```toml
[bundles.generic]
harnesses = ["pi"]

[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"

[bundles.generic.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"

[bundles.generic.skills.local-experiment]
source = "./skills/local-experiment"
```

The bundle remains the policy layer. Individual skill entries retain full source
control.
