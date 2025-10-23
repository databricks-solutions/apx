pack-plugin:
    rm -rf ./dist
    bun run build
    bun pm pack --filename ./src/apx/__dist__/apx-plugin.tgz


fmt:
    uv run ruff format .
    bun x prettier --write .

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push