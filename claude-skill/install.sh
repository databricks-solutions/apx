#!/usr/bin/env bash
# apx skill installer for Claude Code
# Downloads apx skill files into .claude/skills/apx/
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/databricks-solutions/apx/main/claude-skill/install.sh | bash
#   curl -sSL https://raw.githubusercontent.com/databricks-solutions/apx/main/claude-skill/install.sh | bash -s -- --global

set -euo pipefail

REPO="databricks-solutions/apx"
BRANCH="main"
BASE_URL="https://raw.githubusercontent.com/${REPO}/${BRANCH}/skills/apx"

FILES=(
  "SKILL.md"
  "backend-patterns.md"
  "frontend-patterns.md"
)

# Parse arguments
GLOBAL=false
for arg in "$@"; do
  case "$arg" in
    --global) GLOBAL=true ;;
    --help|-h)
      echo "Usage: install.sh [--global]"
      echo ""
      echo "  --global    Install to ~/.claude/skills/apx/ (all projects)"
      echo "  (default)   Install to .claude/skills/apx/ (current project)"
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg"
      exit 1
      ;;
  esac
done

if [ "$GLOBAL" = true ]; then
  DEST="$HOME/.claude/skills/apx"
  echo "Installing apx skill globally to ${DEST}/"
else
  DEST=".claude/skills/apx"
  echo "Installing apx skill to ${DEST}/"
fi

mkdir -p "$DEST"

FAILED=0
for file in "${FILES[@]}"; do
  echo "  Downloading ${file}..."
  if ! curl -sSfL "${BASE_URL}/${file}" -o "${DEST}/${file}"; then
    echo "  ERROR: Failed to download ${file}"
    FAILED=1
  fi
done

if [ "$FAILED" -ne 0 ]; then
  echo ""
  echo "Some files failed to download. Check your network connection and try again."
  exit 1
fi

echo ""
echo "apx skill installed successfully!"
echo ""
echo "Files:"
for file in "${FILES[@]}"; do
  echo "  ${DEST}/${file}"
done
echo ""
if [ "$GLOBAL" = false ]; then
  echo "Tip: Use --global to install for all projects instead."
fi
