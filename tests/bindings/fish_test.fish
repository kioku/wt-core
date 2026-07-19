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

# ── JSON output selection ────────────────────────────────────────────
cd "$REPO_PATH"
set json_add (wt add feat-json --json)
if string match -q '*"cd_path"*' "$json_add"
    pass "wt add --json: returns one JSON document"
else
    fail "wt add --json: expected JSON with cd_path"
end
if test (realpath "$PWD") = "$REPO_PATH"
    pass "wt add --json: cwd unchanged"
else
    fail "wt add --json: cwd changed unexpectedly"
end
wt remove feat-json >/dev/null 2>&1

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

# ── JSON/navigation matrix ───────────────────────────────────────────
set MATRIX_REPO "$WORK/repo\"quote\\slash"
git init "$MATRIX_REPO" >/dev/null 2>&1
cd "$MATRIX_REPO"
set MATRIX_ROOT (pwd -P)
if not wt__path_is_within "$MATRIX_ROOT/.worktrees/app-copy" "$MATRIX_ROOT/.worktrees/app"
    pass "matrix containment: sibling prefix is not a descendant"
else
    fail "matrix containment: sibling prefix was treated as a descendant"
end
git config user.name  "test"
git config user.email "test@test.com"
git commit --allow-empty -m "initial" >/dev/null 2>&1
touch pnpm-workspace.yaml

set json_add (wt add matrix-json --json)
if test (count $json_add) -eq 1
    pass "matrix add --json: one raw stdout document"
else
    fail "matrix add --json: expected one stdout line"
end
if string match -q '*"cd_path"*' "$json_add"
    pass "matrix add --json: command fields preserved"
else
    fail "matrix add --json: missing cd_path"
end
# Parse the JSON value and compare it with the path Git created. This checks
# the decoded quote/backslash rather than counting JSON escape characters.
set matrix_add_path (printf '%s\n' "$json_add" | jq -er '.cd_path | strings')
set parse_rc $status
set expected_matrix_path (realpath "$MATRIX_ROOT/.worktrees"/matrix-json--*)
set expected_rc $status
if test $parse_rc -ne 0 -o $expected_rc -ne 0
    fail "matrix add --json: could not parse or locate worktree path"
end
if test "$matrix_add_path" = "$expected_matrix_path"
    pass "matrix add --json: parsed escaped path preserved"
else
    fail "matrix add --json: parsed path did not match Git path"
end
if test (pwd -P) = "$MATRIX_ROOT"
    pass "matrix add --json: cwd unchanged"
else
    fail "matrix add --json: cwd changed"
end

set json_go (wt go matrix-json --json)
if test (count $json_go) -eq 1
    pass "matrix go --json: one raw stdout document"
else
    fail "matrix go --json: expected one stdout line"
end
if string match -q '*"event":"switch"*' "$json_go"
    pass "matrix go --json: command fields preserved"
else
    fail "matrix go --json: missing switch event"
end
# Reproduce the sibling-prefix navigation case: the cwd begins with the
# removed path text but is not inside that worktree. The wrapper must not cd.
set sibling_path "$expected_matrix_path-copy"
mkdir -p "$sibling_path"
cd "$sibling_path"
set json_remove_file "$WORK/fish-remove.json"
wt remove matrix-json --json --repo "$MATRIX_ROOT" >"$json_remove_file"
set remove_rc $status
set json_remove (cat "$json_remove_file")
if test $remove_rc -ne 0
    fail "matrix remove --json: sibling-path removal failed"
end
if test (count $json_remove) -eq 1
    pass "matrix remove --json: one raw stdout document"
else
    fail "matrix remove --json: expected one stdout line"
end
if string match -q '*"removed_path"*' "$json_remove"
    pass "matrix remove --json: command fields preserved"
else
    fail "matrix remove --json: missing removed_path"
end
set parsed_removed_path (printf '%s\n' "$json_remove" | jq -er '.removed_path | strings')
set parse_rc $status
if test $parse_rc -eq 0
    if test "$parsed_removed_path" = "$expected_matrix_path"
        pass "matrix remove --json: parsed escaped path preserved"
    else
        fail "matrix remove --json: parsed path did not match Git path"
    end
else
    fail "matrix remove --json: invalid JSON"
end
if test (pwd -P) = "$sibling_path"
    pass "matrix remove --json: sibling cwd was not reset"
else
    fail "matrix remove --json: sibling cwd was reset unexpectedly"
end
cd "$MATRIX_ROOT"
rm -rf -- "$sibling_path"

set legacy_stderr "$WORK/fish-add.stderr"
wt add matrix-diagnostics >/dev/null 2>"$legacy_stderr"
if grep -q 'pnpm install --prefer-offline --frozen-lockfile' "$legacy_stderr"
    pass "matrix add: successful stderr diagnostics preserved"
else
    fail "matrix add: stderr diagnostics were discarded"
end
wt remove matrix-diagnostics >/dev/null 2>&1

wt add matrix-remove >/dev/null 2>&1
set expected_matrix_remove_path (realpath "$MATRIX_ROOT/.worktrees"/matrix-remove--*)
set json_remove_file "$WORK/fish-remove.json"
wt remove matrix-remove --json >"$json_remove_file"
set json_remove (cat "$json_remove_file")
if test (count $json_remove) -eq 1
    pass "matrix remove --json: one raw stdout document"
else
    fail "matrix remove --json: expected one stdout line"
end
if string match -q '*"removed_path"*' "$json_remove"
    pass "matrix remove --json: command fields preserved"
else
    fail "matrix remove --json: missing removed_path"
end
set parsed_removed_path (printf '%s\n' "$json_remove" | jq -er '.removed_path | strings')
set parse_rc $status
if test $parse_rc -eq 0
    if test "$parsed_removed_path" = "$expected_matrix_remove_path"
        pass "matrix remove --json: parsed escaped path preserved"
    else
        fail "matrix remove --json: parsed path did not match Git path"
    end
else
    fail "matrix remove --json: invalid JSON"
end
if test (pwd -P) = "$MATRIX_ROOT"
    pass "matrix remove --json: cwd reset to repository root"
else
    fail "matrix remove --json: cwd was not reset"
end

wt add matrix-merge >/dev/null 2>&1
printf 'merge\n' > merge.txt
git add merge.txt
git commit -m "merge" >/dev/null 2>&1
set json_merge_file "$WORK/fish-merge.json"
wt merge matrix-merge --json >"$json_merge_file"
set json_merge (cat "$json_merge_file")
if test (count $json_merge) -eq 1
    pass "matrix merge --json: one raw stdout document"
else
    fail "matrix merge --json: expected one stdout line"
end
if string match -q '*"cleaned_up":true*' "$json_merge"
    pass "matrix merge --json: command fields preserved"
else
    fail "matrix merge --json: missing cleanup field"
end
if test (pwd -P) = "$MATRIX_ROOT"
    pass "matrix merge --json: cwd reset to repository root"
else
    fail "matrix merge --json: cwd was not reset"
end

cd /tmp

echo "All fish binding tests passed."
