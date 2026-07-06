# uniskill User Journeys

This document outlines the primary user journeys for `uniskill`, demonstrating how the configuration structure flexibly handles both global and project-level scopes, as well as user-defined custom agent harnesses.

## Journey 1: The Global Setup (Standard)
**Goal**: A user wants to manage their personal collection of skills and make them available to their global agent harnesses (e.g., Pi and Claude Code).

1. **Initialize**: The user creates a skill bundle at `~/.dotfiles/my-skills`.
2. **Configure**: The user edits their global config at `~/.config/uniskill/config.toml`:
   ```toml
   # ~/.config/uniskill/config.toml
   
   [bundles.my-skills]
   source = "$HOME/.dotfiles/my-skills"
   harnesses = ["pi", "claude-code"] # Built-in global harnesses
   ```
3. **Sync**: The user runs `uniskill sync`. The CLI resolves the built-in patterns for `pi` (`$HOME/.agents/skills/{name}`) and `claude-code` (`$HOME/.claude/skills/{name}`) and creates the symlinks.

## Journey 2: Custom Global Harness
**Goal**: The user adopts a brand new experimental agent framework ("AlphaAgent") that `uniskill` doesn't natively know about yet.

1. **Define Harness**: In their global config, the user defines the new harness pattern.
   ```toml
   # ~/.config/uniskill/config.toml

   [harnesses.alpha-agent]
   pattern = "$HOME/.config/alpha-agent/plugins/{name}"

   [bundles.my-skills]
   source = "$HOME/.dotfiles/my-skills"
   harnesses = ["alpha-agent", "pi"]
   ```
2. **Sync**: `uniskill sync` sees the custom `alpha-agent` definition and wires the bundle into it perfectly.

## Journey 3: Project-Level Skills
**Goal**: A user is working on a specific repository (e.g., a massive monorepo) and wants project-specific skills that are only injected into a local agent harness running within that repository.

1. **Create Project Config**: The user creates `uniskill.toml` at the root of the monorepo.
2. **Configure**:
   ```toml
   # /workspace/monorepo/uniskill.toml

   [harnesses.local-claude]
   pattern = ".claude/skills/{name}" # Relative path to project root

   [bundles.local-tools]
   source = "./scripts/agent-skills" # Relative path to bundle in the repo
   harnesses = ["local-claude"]
   ```
3. **Sync**: The user runs `uniskill sync` while inside the monorepo. The CLI detects the local `uniskill.toml`, resolves the relative pattern to `/workspace/monorepo/.claude/skills/...`, and creates the symlinks. The global setup remains untouched and isolated.

## Journey 4: Hybrid - Global Skills in a Project Harness
**Goal**: A user has a great set of global productivity skills, but they want to use them in an isolated, project-specific agent harness so they don't pollute their global agent's context.

1. **Configure**: In the project's `uniskill.toml`, they reference their global bundle but point it to a project harness:
   ```toml
   # /workspace/isolated-project/uniskill.toml

   [harnesses.local-agent]
   pattern = ".agents/skills/{name}"

   [bundles.global-in-project]
   source = "$HOME/.dotfiles/my-skills" # Global source
   harnesses = ["local-agent"]          # Local destination
   ```
2. **Sync**: `uniskill sync` effectively pulls global capabilities into a scoped, local environment.

## Journey 5: Overriding a Built-in Harness
**Goal**: A user installed `claude-code` via a custom package manager and its global skills folder is in a non-standard location (`/opt/claude/skills`).

1. **Override**: The user redefines the built-in `claude-code` harness in their global config.
   ```toml
   # ~/.config/uniskill/config.toml

   [harnesses.claude-code]
   pattern = "/opt/claude/skills/{name}" # Overrides the built-in default

   [bundles.my-skills]
   source = "$HOME/skills"
   harnesses = ["claude-code"]
   ```
2. **Sync**: `uniskill` prioritizes the user-defined harness over the built-in one.

## Journey 6: Virtual Bundles — Remote Skills from URLs
**Goal**: A user wants to import a skill hosted at a public URL (GitHub raw, Pastebin, etc.) into their project without maintaining a local copy.

1. **Create Project Config**: The user creates `uniskill.toml` at the root of the repo.
2. **Configure**:
   ```toml
   # /workspace/my-project/uniskill.toml

   [harnesses.agents]
   pattern = ".agents/skills/{name}"

   # Virtual bundle: fetch caveman from a remote URL
   [bundles.remote-caveman]
   harnesses = ["agents"]

   [bundles.remote-caveman.skills.caveman]
   url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/main/skills/caveman/SKILL.md"
   ```
3. **Sync**: `uniskill sync` downloads the skill into `.uniskill-cache/remote-caveman/skills/caveman/SKILL.md`, then creates `.agents/skills/caveman` as a symlink to the cache.
4. **Re-run**: On subsequent syncs, uniskill compares the cached file content against the remote URL. If unchanged, no download occurs — the symlink is left as-is.

The user can mix local and virtual bundles under the same harness:

```toml
[bundles.dev-tools]
source = "./scripts/agent-skills"
harnesses = ["agents"]

[bundles.remote-caveman]
harnesses = ["agents"]

[bundles.remote-caveman.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/main/skills/caveman/SKILL.md"
```
