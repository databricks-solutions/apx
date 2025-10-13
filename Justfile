
re-init:
    rm -rf sample/
    uv run apx init --app-path sample --app-name sample

[working-directory: 'sample']
build-sample:
    bun run vite build