---
name: init-lazy-deps
created: 2026-01-30
tags: [init, preflight, dependencies, lazy-install]
---

# Lazy Dependency Installation in apx init

## Summary

Refactored `apx init` to defer dependency installation (`uv sync`, `bun install`) to first command run instead of during init. This makes init faster and follows lazy installation pattern.

## Context

Previously `apx init` would:

1. Create project files
2. Run `uv add --dev apx==version` (installs Python deps)
3. Run `bun install` (installs frontend deps)
4. Add shadcn components
5. Run `apx build`

Now `apx init` only:

1. Creates project files
2. Configures `pyproject.toml` with apx index/sources/version
3. Initializes git

Dependencies are installed via `run_preflight_checks()` on first run of: `apx build`, `apx dev start`, `apx dev check`, `apx frontend build`, `apx mcp`.

## Relevant Files

- `src/cli/init.rs` - simplified init, removed install steps, added `ensure_apx_uv_config()` to configure pyproject.toml
- `src/common.rs` - `run_preflight_checks()` handles `uv sync` and `bun install`
- `src/cli/dev/check.rs` - added preflight
- `src/cli/dev/mcp.rs` - added preflight
- `src/cli/frontend/build.rs` - added preflight
- `src/cli/build.rs` - already had preflight
- `src/cli/dev/start.rs` - already had preflight

## Notes

- Removed `--skip-frontend-dependencies`, `--skip-backend-dependencies`, `--skip-build` flags from init
- `APX_DEV_PATH` env var still works for editable installs (configured in pyproject.toml, installed on first command)
