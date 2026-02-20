
set unstable

set script-interpreter := ['uv', 'run', '--script']

fmt:
    uv tool run ruff format .
    bun x biome format --write .
    cargo fmt --all


lint:
    uv tool run ruff check .
    bun x biome format .

build *args:
    uvx maturin build {{args}}

types:
    cargo check
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    uv tool run ty check

    

# Verify docs types and build
[working-directory: "docs"]
docs-check:
    bun run types:check
    bun run build

check: lint types docs-check

develop:
    uv tool run maturin develop

test *args: develop
    uv run --no-sync pytest tests/ -s -v -n 4 --html=.reports/report.html {{args}}

# Run Rust tests
rust-test *args:
    cargo test --lib {{args}} 

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push


gen folder profile *args:
    uv run --script scripts/dev/gen.py /tmp/{{folder}} {{profile}} {{args}}

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
    # Push branch and tag separately to trigger GitHub Actions on tag push
    git push origin main
    git push origin {{tag}}

sync:
    cargo check

uv-sync:
    RUST_LOG=debug uv sync

release-registry:
    gh workflow run deploy-registry.yml

[working-directory: "crates/studio"]
studio:
    bun tauri dev