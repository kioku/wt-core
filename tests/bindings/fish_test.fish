#!/usr/bin/env fish
# Integration tests for the Fish shell binding.
# Requires wt-core on PATH.

set SCRIPT_DIR (status dirname)
set REPO_ROOT (realpath "$SCRIPT_DIR/../..")

source "$REPO_ROOT/bindings/fish/wt.fish"

set WORK (mktemp -d)

function cleanup --on-event fish_exit
    rm -rf "$WORK"
end

function pass
    printf '  ✓ %s\n' $argv[1]
end

function fail
    printf '  ✗ %s\n' $argv[1]
    exit 1
end

# ── Setup ────────────────────────────────────────────────────────────
git init "$WORK/repo" >/dev/null 2>&1
cd "$WORK/repo"
set REPO_PATH (realpath "$PWD")
git config user.name  "test"
git config user.email "test@test.com"
git commit --allow-empty -m "initial" >/dev/null 2>&1

echo "Running fish binding tests..."

# ── wt add ───────────────────────────────────────────────────────────
wt add feat-one >/dev/null 2>&1
if string match -q "*.worktrees/*feat-one*" "$PWD"
    pass "wt add: cd into new worktree"
else
    fail "wt add: expected cwd inside .worktrees/…feat-one…, got $PWD"
end

set WT_PATH (realpath "$PWD")

# ── wt list ──────────────────────────────────────────────────────────
set output (wt list 2>&1)
if string match -q "*feat-one*" "$output"
    pass "wt list: output contains branch name"
else
    fail "wt list: 'feat-one' not found in output"
end

# ── wt go ────────────────────────────────────────────────────────────
cd "$REPO_PATH"
wt go feat-one >/dev/null 2>&1
if test (realpath "$PWD") = "$WT_PATH"
    pass "wt go: cd into existing worktree"
else
    fail "wt go: expected $WT_PATH, got "(realpath "$PWD")
end

# ── help passthrough safety ─────────────────────────────────────────
set add_help (wt add --help 2>&1)
if string match -q "*Usage: wt-core add*" "$add_help"
    pass "wt add --help: passthrough to core help"
else
    fail "wt add --help: expected core help output"
end
if test (realpath "$PWD") = "$WT_PATH"
    pass "wt add --help: cwd unchanged"
else
    fail "wt add --help: cwd changed unexpectedly"
end

set go_help (wt go --help 2>&1)
if string match -q "*Usage: wt-core go*" "$go_help"
    pass "wt go --help: passthrough to core help"
else
    fail "wt go --help: expected core help output"
end
if test (realpath "$PWD") = "$WT_PATH"
    pass "wt go --help: cwd unchanged"
else
    fail "wt go --help: cwd changed unexpectedly"
end

set rm_help (wt remove --help 2>&1)
if string match -q "*Usage: wt-core remove*" "$rm_help"
    pass "wt remove --help: passthrough to core help"
else
    fail "wt remove --help: expected core help output"
end
if test -d "$WT_PATH"
    pass "wt remove --help: worktree not removed"
else
    fail "wt remove --help: worktree was removed unexpectedly"
end
if test (realpath "$PWD") = "$WT_PATH"
    pass "wt remove --help: cwd unchanged"
else
    fail "wt remove --help: cwd changed unexpectedly"
end

# ── wt remove (from inside worktree) ────────────────────────────────
wt remove feat-one 2>&1
if test (realpath "$PWD") = "$REPO_PATH"
    pass "wt remove: cd back to repo root"
else
    fail "wt remove: expected $REPO_PATH, got "(realpath "$PWD")
end

if not test -d "$WT_PATH"
    pass "wt remove: worktree directory deleted"
else
    fail "wt remove: $WT_PATH still exists"
end

# ── wt merge destination metadata ───────────────────────────────────
wt add feat-merge >/dev/null 2>&1
set MERGE_WT_PATH $PWD
printf 'merge content\n' > merge.txt
git add merge.txt
git commit -m "merge content" >/dev/null 2>&1
set MERGE_OUTPUT "$WORK/merge-output"
wt merge feat-merge > "$MERGE_OUTPUT" 2>&1
if string match -q "*Destination worktree: $REPO_PATH*" (cat "$MERGE_OUTPUT")
    pass "wt merge: human output includes destination path"
else
    fail "wt merge: destination path missing from human output"
end
if test (realpath "$PWD") = "$REPO_PATH"
    pass "wt merge: cd back to destination repository"
else
    fail "wt merge: expected $REPO_PATH, got "(realpath "$PWD")
end
if not test -d "$MERGE_WT_PATH"
    pass "wt merge: source worktree deleted"
else
    fail "wt merge: source worktree still exists"
end

echo "All fish binding tests passed."
