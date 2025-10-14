
re-init:
    rm -rf sample/
    # uv run apx init --app-path sample --app-name sample
    uvx git+https://github.com/renardeinside/apx.git init --app-path sample --app-name sample   

[working-directory: 'sample']
build-sample:
    bun run vite build

fmt:
    uv run ruff format .
    bun x prettier --write .

release:
    bun run build
    git add .
    git commit -m "release"
    git push

[working-directory: 'sample']
reinstall:
    bun remove apx
    bun add github:renardeinside/apx

[working-directory: 'sample']
sample-dev:
    bun run vite dev