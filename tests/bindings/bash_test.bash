#!/usr/bin/env bash
# Integration tests for the Bash shell binding.
# Requires wt-core on PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=../../bindings/bash/wt.bash
source "$REPO_ROOT/bindings/bash/wt.bash"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

pass() { printf '  ✓ %s\n' "$1"; }
fail() { printf '  ✗ %s\n' "$1"; exit 1; }

# ── Setup ────────────────────────────────────────────────────────────
git init "$WORK/repo" >/dev/null 2>&1
cd "$WORK/repo"
REPO_PATH="$(pwd -P)"
git config user.name  "test"
git config user.email "test@test.com"
git commit --allow-empty -m "initial" >/dev/null 2>&1

echo "Running bash binding tests..."

# ── wt add ───────────────────────────────────────────────────────────
wt add feat-one >/dev/null 2>&1
[[ "$PWD" == *".worktrees/"*"feat-one"* ]] \
    && pass "wt add: cd into new worktree" \
    || fail "wt add: expected cwd inside .worktrees/…feat-one…, got $PWD"

WT_PATH="$(pwd -P)"

# ── wt list ──────────────────────────────────────────────────────────
output=$(wt list 2>&1)
echo "$output" | grep -q "feat-one" \
    && pass "wt list: output contains branch name" \
    || fail "wt list: 'feat-one' not found in output"

# ── wt go ────────────────────────────────────────────────────────────
cd "$REPO_PATH"
wt go feat-one >/dev/null 2>&1
[[ "$(pwd -P)" == "$WT_PATH" ]] \
    && pass "wt go: cd into existing worktree" \
    || fail "wt go: expected $WT_PATH, got $(pwd -P)"

# ── wt remove (from inside worktree) ────────────────────────────────
wt remove feat-one 2>&1
[[ "$(pwd -P)" == "$REPO_PATH" ]] \
    && pass "wt remove: cd back to repo root" \
    || fail "wt remove: expected $REPO_PATH, got $(pwd -P)"

# Worktree directory should be gone
[[ ! -d "$WT_PATH" ]] \
    && pass "wt remove: worktree directory deleted" \
    || fail "wt remove: $WT_PATH still exists"

echo "All bash binding tests passed."
