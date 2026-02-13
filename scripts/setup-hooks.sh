#!/usr/bin/env bash

# Setup script for wt-core git hooks

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

REPO_ROOT="$(git rev-parse --show-toplevel)"
GIT_DIR="$(git rev-parse --git-dir)"
HOOKS_DIR="$GIT_DIR/hooks"
SHARED_HOOKS_DIR="$REPO_ROOT/.github/hooks"

echo -e "${BLUE}Setting up wt-core git hooks...${NC}"

if ! git rev-parse --git-dir &>/dev/null; then
    echo -e "${RED}Error: Not in a git repository${NC}"
    exit 1
fi

if [ ! -d "$SHARED_HOOKS_DIR" ]; then
    echo -e "${RED}Error: Shared hooks directory not found at $SHARED_HOOKS_DIR${NC}"
    exit 1
fi

mkdir -p "$HOOKS_DIR"

for hook in pre-commit commit-msg pre-push prepare-commit-msg; do
    if [ -f "$SHARED_HOOKS_DIR/$hook" ]; then
        echo -e "${GREEN}Installing $hook hook...${NC}"
        cp "$SHARED_HOOKS_DIR/$hook" "$HOOKS_DIR/$hook"
        chmod +x "$HOOKS_DIR/$hook"
        echo "  ✓ $hook hook installed"
    fi
done

echo
echo -e "${GREEN}✅ Git hooks setup complete!${NC}"
echo -e "${BLUE}Installed hooks:${NC}"
for hook in "$HOOKS_DIR"/*; do
    if [ -f "$hook" ] && [ -x "$hook" ]; then
        echo "  • $(basename "$hook")"
    fi
done

echo
echo -e "${YELLOW}Hooks run automatically on git operations.${NC}"
echo -e "${YELLOW}Bypass with --no-verify if needed.${NC}"
echo -e "${BLUE}To update, pull latest and re-run this script.${NC}"
