

fmt:
    uv run ruff format .
    bun x prettier --write .

lint:
    uv run ruff check .
    bun x prettier --check .

build *args:
    uvx maturin build {{args}}

develop *args:
    uvx maturin develop {{args}}

types:
    uv run mypy .
    cargo check
    # uv run ty check 
    

check: lint types

test *args:
    uv run pytest tests/ -s -v --cov=src/apx {{args}} -n auto

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push

get-version:
    uvx uv-dynamic-versioning


gen folder profile *args: develop
    rm -rf /tmp/{{folder}}
    RUST_LOG=DEBUG APX_DEV_PATH="{{justfile_directory()}}" uv run --no-sync apx init /tmp/{{folder}} -p {{profile}}  {{args}}
    cd /tmp/{{folder}} && uv run apx dev check