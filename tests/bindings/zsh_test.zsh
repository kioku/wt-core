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

# ── JSON output selection ────────────────────────────────────────────
cd "$REPO_PATH"
json_add=$(wt add feat-json --json)
echo "$json_add" | grep -q '"cd_path"' \
    && pass "wt add --json: returns one JSON document" \
    || fail "wt add --json: expected JSON with cd_path"
[[ "$(pwd -P)" == "$REPO_PATH" ]] \
    && pass "wt add --json: cwd unchanged" \
    || fail "wt add --json: cwd changed unexpectedly"
wt remove feat-json >/dev/null 2>&1

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

# ── JSON/navigation matrix ───────────────────────────────────────────
MATRIX_REPO="$WORK/repo\"quote\\slash"
git init "$MATRIX_REPO" >/dev/null 2>&1
cd "$MATRIX_REPO"
MATRIX_ROOT="$(pwd -P)"
if ! wt__path_is_within "$MATRIX_ROOT/.worktrees/app-copy" "$MATRIX_ROOT/.worktrees/app"; then
    pass "matrix containment: sibling prefix is not a descendant"
else
    fail "matrix containment: sibling prefix was treated as a descendant"
fi
git config user.name  "test"
git config user.email "test@test.com"
git commit --allow-empty -m "initial" >/dev/null 2>&1
touch pnpm-workspace.yaml

json_add=$(wt add matrix-json --json)
[[ "$(printf '%s\n' "$json_add" | wc -l)" -eq 1 ]] \
    && pass "matrix add --json: one raw stdout document" \
    || fail "matrix add --json: expected one stdout line"
echo "$json_add" | grep -q '"cd_path"' \
    && pass "matrix add --json: command fields preserved" \
    || fail "matrix add --json: missing cd_path"
matrix_add_path=$(printf '%s\n' "$json_add" | jq -er '.cd_path | strings')
expected_matrix_path=$(find "$MATRIX_ROOT/.worktrees" -maxdepth 1 -type d -name 'matrix-json--*' -print -quit)
[[ "$matrix_add_path" == "$expected_matrix_path" ]] \
    && pass "matrix add --json: parsed escaped path preserved" \
    || fail "matrix add --json: parsed path did not match Git path"
[[ "$(pwd -P)" == "$MATRIX_ROOT" ]] \
    && pass "matrix add --json: cwd unchanged" \
    || fail "matrix add --json: cwd changed"

json_go=$(wt go matrix-json --json)
[[ "$(printf '%s\n' "$json_go" | wc -l)" -eq 1 ]] \
    && pass "matrix go --json: one raw stdout document" \
    || fail "matrix go --json: expected one stdout line"
echo "$json_go" | grep -q '"event":"switch"' \
    && pass "matrix go --json: command fields preserved" \
    || fail "matrix go --json: missing switch event"
cd "$MATRIX_ROOT"
wt remove matrix-json >/dev/null 2>&1

legacy_stderr="$WORK/zsh-add.stderr"
wt add matrix-diagnostics >/dev/null 2>"$legacy_stderr"
grep -q 'pnpm install --prefer-offline --frozen-lockfile' "$legacy_stderr" \
    && pass "matrix add: successful stderr diagnostics preserved" \
    || fail "matrix add: stderr diagnostics were discarded"
wt remove matrix-diagnostics >/dev/null 2>&1

wt add matrix-remove >/dev/null 2>&1
expected_matrix_remove_path=$(find "$MATRIX_ROOT/.worktrees" -maxdepth 1 -type d -name 'matrix-remove--*' -print -quit)
json_remove_file="$WORK/zsh-remove.json"
wt remove matrix-remove --json >"$json_remove_file"
json_remove=$(<"$json_remove_file")
[[ "$(printf '%s\n' "$json_remove" | wc -l)" -eq 1 ]] \
    && pass "matrix remove --json: one raw stdout document" \
    || fail "matrix remove --json: expected one stdout line"
echo "$json_remove" | grep -q '"removed_path"' \
    && pass "matrix remove --json: command fields preserved" \
    || fail "matrix remove --json: missing removed_path"
parsed_removed_path=$(printf '%s\n' "$json_remove" | jq -er '.removed_path | strings')
[[ "$parsed_removed_path" == "$expected_matrix_remove_path" ]] \
    && pass "matrix remove --json: parsed escaped path preserved" \
    || fail "matrix remove --json: parsed path did not match Git path"
[[ "$(pwd -P)" == "$MATRIX_ROOT" ]] \
    && pass "matrix remove --json: cwd reset to repository root" \
    || fail "matrix remove --json: cwd was not reset"

wt add matrix-merge >/dev/null 2>&1
printf 'merge\n' > merge.txt
git add merge.txt
git commit -m "merge" >/dev/null 2>&1
json_merge_file="$WORK/zsh-merge.json"
wt merge matrix-merge --json >"$json_merge_file"
json_merge=$(<"$json_merge_file")
[[ "$(printf '%s\n' "$json_merge" | wc -l)" -eq 1 ]] \
    && pass "matrix merge --json: one raw stdout document" \
    || fail "matrix merge --json: expected one stdout line"
echo "$json_merge" | grep -q '"cleaned_up":true' \
    && pass "matrix merge --json: command fields preserved" \
    || fail "matrix merge --json: missing cleanup field"
[[ "$(pwd -P)" == "$MATRIX_ROOT" ]] \
    && pass "matrix merge --json: cwd reset to repository root" \
    || fail "matrix merge --json: cwd was not reset"

cd /tmp

echo "All zsh binding tests passed."
