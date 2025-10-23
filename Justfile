pack-plugin:
    rm -rf src/apx/dist/*
    uv build --wheel


fmt:
    uv run ruff format .
    bun x prettier --write .