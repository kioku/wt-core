#!/usr/bin/env zsh
# Integration tests for the Zsh shell binding.
# Requires wt-core on PATH.
set -euo pipefail

SCRIPT_DIR="${0:a:h}"
REPO_ROOT="$SCRIPT_DIR/../.."

source "$REPO_ROOT/bindings/zsh/wt.zsh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

pass() { printf '  ✓ %s\n' "$1" }
fail() { printf '  ✗ %s\n' "$1"; exit 1 }

# ── Setup ────────────────────────────────────────────────────────────
git init "$WORK/repo" >/dev/null 2>&1
cd "$WORK/repo"
REPO_PATH="$(pwd -P)"
git config user.name  "test"
git config user.email "test@test.com"
git commit --allow-empty -m "initial" >/dev/null 2>&1

echo "Running zsh binding tests..."

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

# ── help passthrough safety ─────────────────────────────────────────
add_help=$(wt add --help 2>&1)
echo "$add_help" | grep -q "Usage: wt-core add" \
    && pass "wt add --help: passthrough to core help" \
    || fail "wt add --help: expected core help output"
[[ "$(pwd -P)" == "$WT_PATH" ]] \
    && pass "wt add --help: cwd unchanged" \
    || fail "wt add --help: cwd changed unexpectedly"

go_help=$(wt go --help 2>&1)
echo "$go_help" | grep -q "Usage: wt-core go" \
    && pass "wt go --help: passthrough to core help" \
    || fail "wt go --help: expected core help output"
[[ "$(pwd -P)" == "$WT_PATH" ]] \
    && pass "wt go --help: cwd unchanged" \
    || fail "wt go --help: cwd changed unexpectedly"

rm_help=$(wt remove --help 2>&1)
echo "$rm_help" | grep -q "Usage: wt-core remove" \
    && pass "wt remove --help: passthrough to core help" \
    || fail "wt remove --help: expected core help output"
[[ -d "$WT_PATH" ]] \
    && pass "wt remove --help: worktree not removed" \
    || fail "wt remove --help: worktree was removed unexpectedly"
[[ "$(pwd -P)" == "$WT_PATH" ]] \
    && pass "wt remove --help: cwd unchanged" \
    || fail "wt remove --help: cwd changed unexpectedly"

# ── wt remove (from inside worktree) ────────────────────────────────
wt remove feat-one 2>&1
[[ "$(pwd -P)" == "$REPO_PATH" ]] \
    && pass "wt remove: cd back to repo root" \
    || fail "wt remove: expected $REPO_PATH, got $(pwd -P)"

[[ ! -d "$WT_PATH" ]] \
    && pass "wt remove: worktree directory deleted" \
    || fail "wt remove: $WT_PATH still exists"

# ── wt merge destination metadata ───────────────────────────────────
wt add feat-merge >/dev/null 2>&1
MERGE_WT_PATH="$PWD"
printf 'merge content\n' > merge.txt
git add merge.txt
git commit -m "merge content" >/dev/null 2>&1
MERGE_OUTPUT="$WORK/merge-output"
wt merge feat-merge >"$MERGE_OUTPUT" 2>&1
grep -q "Destination worktree: $REPO_PATH" "$MERGE_OUTPUT" \
    && pass "wt merge: human output includes destination path" \
    || fail "wt merge: destination path missing from human output"
[[ "$(pwd -P)" == "$REPO_PATH" ]] \
    && pass "wt merge: cd back to destination repository" \
    || fail "wt merge: expected $REPO_PATH, got $(pwd -P)"
[[ ! -d "$MERGE_WT_PATH" ]] \
    && pass "wt merge: source worktree deleted" \
    || fail "wt merge: source worktree still exists"

echo "All zsh binding tests passed."
