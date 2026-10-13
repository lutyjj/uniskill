---
name: uniskill-dev
description: Workflow and conventions for developing uniskill, the skills and MCP configuration CLI. Covers build, test, feature flow, and project architecture.
---

# uniskill development

Read `AGENTS.md`, `README.md`, and `docs/DESIGN.md` before changing uniskill.
`docs/MCP.md` owns native MCP routing, ownership, adoption, and recovery.

Keep fetching, linking, native document writes, and CLI reporting in their
own modules. Define harness destinations as data in `src/harnesses.toml` or
user configuration. Avoid product-specific branches in the sync engine.

Prove changed behavior through its production boundary, including failure and
repeated-apply cases. Run the checks required by `AGENTS.md`; `make release`
is the aggregate gate and owns packaging. Use Conventional Commits.
