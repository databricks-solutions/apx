pack-plugin:
    rm -rf ./dist
    bun run build
    bun pm pack --filename ./src/apx/dist/apx-plugin.tgz


fmt:
    uv run ruff format .
    bun x prettier --write .