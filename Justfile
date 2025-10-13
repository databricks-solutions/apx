
re-init:
    rm -rf sample/
    apx init --app-path sample

[working-directory: 'sample']
build-sample:
    bun run vite build