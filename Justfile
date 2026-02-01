

fmt:
    uv tool run ruff format .
    bun x prettier --write .
    cargo fmt --all
    

lint:
    uv tool run ruff check .
    bun x prettier --check .

build *args:
    uvx maturin build {{args}}

types:
    cargo check
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    uv tool run ty check

    

check: lint types

develop:
    uv tool run maturin develop

test *args: develop
    uv run --no-sync pytest tests/ -s -v -n 4 --html=.reports/report.html {{args}}

# Run Rust tests
# VIRTUAL_ENV is set in .cargo/config.toml for pyo3-build-config
rust-test *args:
    cargo test --lib {{args}} 

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push


gen folder profile *args: uv-sync
    rm -rf /tmp/{{folder}}
    RUST_LOG=DEBUG APX_DEV_PATH="{{justfile_directory()}}" uv run --no-sync apx init /tmp/{{folder}} -p {{profile}}  {{args}}
    cd /tmp/{{folder}} && uv run apx dev check

[working-directory: "docs"]
docs *args:
    bun {{args}}

# Build complete static site (docs + simple package index)
pages:
    rm -rf .pages
    cd docs && bun run build
    uv run python scripts/generate_registry.py

# Serve the built pages locally
serve-pages: pages
    uv run python -m http.server -d .pages

release *tag:
    #!/usr/bin/env bash
    # Update Cargo.toml with the tag version (remove 'v' prefix)
    VERSION=$(echo "{{tag}}" | sed 's/^v//')
    cargo set-version $VERSION
    cargo check # ensure the version is set correctly in lockfile
    git commit -am "Release {{tag}}"
    git tag {{tag}}
    git push origin main --tags

sync:
    cargo check

uv-sync:
    RUST_LOG=debug uv sync

release-registry:
    gh workflow run deploy-registry.yml