# MCP configuration

MCP servers are independent of skill bundles. A single definition can target
several harnesses:

```toml
[mcp.docs]
harnesses = ["pi", "claude-code", "codex"]
url = "https://example.com/mcp"
bearer_token_env_var = "DOCS_TOKEN"

[mcp.local]
harnesses = ["pi", "codex"]
command = "npx"
args = ["-y", "example-mcp"]
env_vars = ["LOCAL_TOKEN"]

[mcp.docs.options.pi]
exposure = "direct"
```

Every server needs exactly one `command` or HTTP(S) `url`. Command servers
accept `args`, literal `env`, and `env_vars`. URL servers accept literal
`headers`, `env_headers` (header name to environment variable name), and
`bearer_token_env_var`. Literals and references must not overlap. References
remain references in output; uniskill does not read their values.

`options.<harness>` supplies native policy fields such as `enabled`, tool
exposure, and timeouts. Options cannot override connection fields. Native
agents own policy validation, environment resolution, trust, and OAuth state.

## Configurable destinations

Built-in paths and formats are defined in [harnesses.toml](../src/harnesses.toml).
To add an MCP destination, supply data in the config:

```toml
[harnesses.custom.mcp]
path = ".agent/config.toml"
format = "toml"
table = ["mcp_servers"]
references = "fields"
omit = ["type"]
fields = { headers = "http_headers", env_headers = "env_http_headers" }
```

`format` is `json` or `toml`; `table` is a literal key path to the server map.
`fields` maps canonical connection keys to native keys. `omit` can remove the
HTTP `type` marker; omitting populated connection fields is rejected.
`references = "interpolate"` (default) renders environment/header references
as `${VAR}`. `references = "fields"` keeps explicit reference fields; literal
values containing `${` are rejected for these targets. A harness can declare
skills, MCP, or both. Unknown destinations and conflicting routes fail before
native writes.

Global and project configs share this model. `includes = ["private.toml"]`
composes additional files; paths resolve at the file declaring them. Includes
are required, bounded, and cycle checked. Duplicate identities fail. Keep
secret catalogs outside Git with restrictive file and directory permissions.

## Apply and ownership

`uniskill sync` reconciles MCP entries, then skills. `sync --mcp-only` applies
MCP without fetching or linking skills. `sync --check-mcp` performs a read-only
check and exits 1 when changes or conflicts remain; it creates no cache.

Existing unmanaged entries are conflicts. `--adopt-mcp` takes ownership only
when the existing definition exactly matches the rendered definition. Subsequent
syncs update or remove entries only when their value still matches the recorded
hash. Externally edited entries cause the whole MCP transaction to fail; inspect
and reconcile them before applying. Missing owned entries are recreated.
Unrelated entries, agent settings, and TOML comments stay intact. Converged
files and ownership state retain their bytes and modification times.

Ownership lives in `mcp/state.json` under the sync's cache directory. It records
identities and hashes, without server payloads. New native files are mode 0600;
existing modes are preserved. Native symlinks, malformed files, and incompatible
table shapes are rejected. Native files and individual outputs are limited to
8 MiB; the transaction journal is limited to 64 MiB.

All MCP files and ownership state are planned before writing. A private,
mode-0600 journal records before/after contents; atomic file replacement and
fsync protect each write. Sync locks coordinate uniskill writers. A failed or
interrupted apply rolls back its own bytes; a later sync recovers the journal.
Files edited since the interrupted write are preserved and reported. Recovery
I/O failures require inspection before continuing. The journal can contain
secrets temporarily, so its directory is mode 0700.

Native agents do not share uniskill's lock. They can edit a file after the
pre-write comparison; there is no cross-process atomic compare-and-swap.
Run sync while native config writers are idle.

## Native scope and precedence

Built-ins target user config. Project-local destinations require explicit
custom targets: Claude Code uses `.mcp.json`, Codex uses `.codex/config.toml`,
and Pi uses `.pi/mcp.json`. Native trust and override rules still apply.
Claude Code's system-managed MCP configuration is exclusive and overrides
user output. Codex layers system and trusted project settings with user config;
Pi combines user and trusted project config. Check the effective native server
list after adopting existing configurations.

`sync --worktree` retargets repository-owned native paths into the linked
checkout and keeps a separate ownership state there. Global paths are skipped;
worktree runs never fetch skill sources.

Native contracts: [Claude Code](https://code.claude.com/docs/en/mcp),
[Codex](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
[Pi](https://pi.dev/docs/latest/mcp).
