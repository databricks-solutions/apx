# pack-plugin:
#     rm -rf ./dist
#     bun run build
#     bun pm pack --filename ./src/apx/__dist__/apx-plugin.tgz


fmt:
    uv run ruff format .
    bun x prettier --write .

lint:
    uv run ruff check .
    bun x prettier --check .

types:
    uv run mypy .
    uv run basedpyright --level error

check: lint types

test:
    uv run pytest tests/ -v --cov=src/apx -n auto

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push