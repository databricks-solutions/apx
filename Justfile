
re-init:
    rm -rf sample/
    uv run apx init sample sample
    # remove dist/.gitignore and dist/apx-*
    # as they're automatically generated
    rm -rf dist/.gitignore
    rm -rf dist/apx-*

[working-directory: 'sample']
build-sample:
    bun run vite build

fmt:
    uv run ruff format .
    bun x prettier --write .

release:
    rm -rf dist/.gitignore
    rm -rf dist/apx-*
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