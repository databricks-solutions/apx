#!/usr/bin/env bash
# Post-install verification for apx plugin
# Checks that the apx binary is available on the system

set -euo pipefail

if ! command -v apx &>/dev/null; then
  echo "WARNING: apx CLI is not installed or not on PATH."
  echo ""
  echo "The apx plugin requires the apx CLI to function."
  echo "Install it with:"
  echo ""
  echo "  uv tool install apx"
  echo ""
  echo "Or see: https://github.com/databricks-solutions/apx"
  exit 1
fi

echo "apx CLI found: $(apx --version)"
