

fmt:
    uv run ruff format .
    bun x prettier --write .

lint:
    uv run ruff check .
    bun x prettier --check .

build *args:
    uvx maturin build {{args}}

types:
    uv run mypy .
    cargo check
    uv run ty check

sync:
    RUST_LOG=debug uv sync
    

check: lint types

test *args:
    uv run pytest tests/ -s -v -n 4 --html=.reports/report.html {{args}} 

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push


gen folder profile *args: sync
    rm -rf /tmp/{{folder}}
    RUST_LOG=DEBUG APX_DEV_PATH="{{justfile_directory()}}" uv run --no-sync apx init /tmp/{{folder}} -p {{profile}}  {{args}}
    cd /tmp/{{folder}} && uv run apx dev check

[working-directory: "docs"]
docs *args:
    bun run {{args}}

# Build complete static site (docs + simple package index)
pages:
    rm -rf .pages
    cd docs && bun run build
    uv run python scripts/generate_registry.py

# Serve the built pages locally
serve-pages: pages
    uv run python -m http.server -d .pages