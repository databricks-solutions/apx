

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

test *args:
    uv run pytest tests/ -s -v --cov=src/apx {{args}} -n auto

# add-commit-push with a message
pm message:
    git add .
    git commit -m "{{message}}"
    git push

get-version:
    uvx uv-dynamic-versioning



# Generate a sample project in /tmp/sample using apx from current directory
gen-sample:
    rm -rf /tmp/sample
    uv run apx init /tmp/sample -p fe-az -a cursor -l basic -t essential -n sample \
        --apx-package="{{justfile_directory()}}" \
        --apx-editable
    cd /tmp/sample && uv run apx dev check

gen folder profile *args:
    rm -rf /tmp/{{folder}}
    uv run apx init /tmp/{{folder}} -p {{profile}} \
        --apx-package="{{justfile_directory()}}" \
        --apx-editable \
        {{args}}
    cd /tmp/{{folder}} && uv run apx dev check
