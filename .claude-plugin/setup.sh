#!/usr/bin/env bash
# Post-install setup for apx plugin
# Installs the apx CLI if not already present, then verifies it works.

set -euo pipefail

if command -v apx &>/dev/null; then
  echo "apx CLI found: $(apx --version)"
  exit 0
fi

echo "apx CLI not found — installing..."
echo ""

if ! command -v curl &>/dev/null; then
  echo "ERROR: curl is required but not found."
  echo "Install curl, then re-run this setup."
  exit 1
fi

curl -fsSL https://databricks-solutions.github.io/apx/install.sh | sh

# Verify installation succeeded
if ! command -v apx &>/dev/null; then
  # The installer may have added to PATH in a shell profile but the current
  # shell doesn't see it yet. Check common install locations directly.
  for dir in "$HOME/.local/bin" "${XDG_BIN_HOME:-}" "${APX_INSTALL_DIR:-}"; do
    if [ -n "$dir" ] && [ -x "$dir/apx" ]; then
      echo "apx installed to $dir/apx (restart your shell to add it to PATH)"
      exit 0
    fi
  done
  echo "WARNING: apx was installed but is not on PATH yet."
  echo "Restart your shell or add the install directory to your PATH."
  exit 1
fi

echo "apx CLI installed: $(apx --version)"
