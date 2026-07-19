#!/usr/bin/env nu
# Integration tests for the Nushell binding.
# Requires wt-core on PATH.

source ../../bindings/nu/wt.nu

def pass [msg: string] { print $"  ✓ ($msg)" }
def fail [msg: string] { print $"  ✗ ($msg)"; exit 1 }

let work = (^mktemp -d | str trim)

^git init $"($work)/repo" o+e>| ignore
cd $"($work)/repo"
^git config user.name "test"
^git config user.email "test@test.com"
^git commit --allow-empty -m "initial" o+e>| ignore

print "Running nu binding tests..."

# ── wt / wt --help ───────────────────────────────────────────────────
let root_output = (wt)
if ($root_output | str contains "Portable Git worktree lifecycle manager") {
    pass "wt: root command available"
} else {
    fail "wt: missing expected core help output"
}

let help_output = (wt --help)
if ($help_output | str contains "Usage: wt-core <COMMAND>") and ($help_output | str contains "Commands:") {
    pass "wt --help: passthrough to core help"
} else {
    fail "wt --help: missing expected core help output"
}

# ── wt add ───────────────────────────────────────────────────────────
wt add feat-one
if ($env.PWD | str contains ".worktrees") and ($env.PWD | str contains "feat-one") {
    pass "wt add: cd into new worktree"
} else {
    fail $"wt add: expected cwd inside .worktrees/…feat-one…, got ($env.PWD)"
}

let wt_path = $env.PWD

# ── JSON output selection ────────────────────────────────────────────
cd $"($work)/repo"
let json_add = (wt add feat-json --json)
if ($json_add | str contains '"cd_path"') {
    pass "wt add --json: returns raw JSON stdout"
} else {
    fail "wt add --json: expected raw JSON with cd_path"
}
if $env.PWD == ($"($work)/repo" | path expand) {
    pass "wt add --json: cwd unchanged"
} else {
    fail "wt add --json: cwd changed unexpectedly"
}
wt remove feat-json | ignore

# ── wt list ──────────────────────────────────────────────────────────
let output = (wt list | str join "\n")
if ($output | str contains "feat-one") {
    pass "wt list: output contains branch name"
} else {
    fail "wt list: 'feat-one' not found in output"
}

let stats_output = (wt list --stats --color never | str join "\n")
if ($stats_output | str contains "COMMITS") and ($stats_output | str contains "feat-one") {
    pass "wt list --stats: forwards stats options"
} else {
    fail "wt list --stats: missing expected stats output"
}

# ── wt go ────────────────────────────────────────────────────────────
cd $"($work)/repo"
wt go feat-one
if $env.PWD == $wt_path {
    pass "wt go: cd into existing worktree"
} else {
    fail $"wt go: expected ($wt_path), got ($env.PWD)"
}

# ── wt remove (from inside worktree) ────────────────────────────────
wt remove feat-one
let expected_repo = ($"($work)/repo" | path expand)
let actual_pwd = ($env.PWD | path expand)
if $actual_pwd == $expected_repo {
    pass "wt remove: cd back to repo root"
} else {
    fail $"wt remove: expected ($expected_repo), got ($actual_pwd)"
}

if not ($wt_path | path exists) {
    pass "wt remove: worktree directory deleted"
} else {
    fail $"wt remove: ($wt_path) still exists"
}

# ── JSON/navigation matrix ───────────────────────────────────────────
# Use a repository path containing JSON-significant characters.
let matrix_repo = ($work | path join 'repo"quote\slash')
^git init $matrix_repo o+e>| ignore
cd $matrix_repo
let matrix_root = ($env.PWD | path expand)
if not (path-is-within ($matrix_root | path join ".worktrees/app-copy") ($matrix_root | path join ".worktrees/app")) {
    pass "matrix containment: sibling prefix is not a descendant"
} else {
    fail "matrix containment: sibling prefix was treated as a descendant"
}
^git config user.name "test"
^git config user.email "test@test.com"
^git commit --allow-empty -m "initial" o+e>| ignore
^touch ($matrix_repo | path join "pnpm-workspace.yaml")

let matrix_add = (wt add matrix-json --json)
if ($matrix_add | str contains '"cd_path"') {
    pass "matrix add --json: raw JSON stdout"
} else {
    fail "matrix add --json: missing cd_path"
}
if ($matrix_add | str contains 'quote\\slash') {
    pass "matrix add --json: escaped path preserved"
} else {
    fail "matrix add --json: escaped path was lost"
}
if ($env.PWD | path expand) == $matrix_root {
    pass "matrix add --json: cwd unchanged"
} else {
    fail "matrix add --json: cwd changed"
}

let matrix_go = (wt go matrix-json --json)
if ($matrix_go | str contains '"event":"switch"') {
    pass "matrix go --json: command fields preserved"
} else {
    fail "matrix go --json: missing switch event"
}
cd $matrix_root
wt remove matrix-json | ignore

let nu_stderr = ($work | path join "nu-add.stderr")
let _ = (wt add matrix-diagnostics err> $nu_stderr)
if (open $nu_stderr | str contains "pnpm install --prefer-offline --frozen-lockfile") {
    pass "matrix add: successful stderr diagnostics preserved"
} else {
    fail "matrix add: stderr diagnostics were discarded"
}
wt remove matrix-diagnostics | ignore

wt add matrix-remove | ignore
let matrix_remove_file = ($work | path join "nu-remove.json")
wt remove matrix-remove --json | save --force $matrix_remove_file
let matrix_remove = (open --raw $matrix_remove_file | decode utf-8)
if ($matrix_remove | str contains '"removed_path"') {
    pass "matrix remove --json: command fields preserved"
} else {
    fail "matrix remove --json: missing removed_path"
}
if ($matrix_remove | str contains 'quote\\slash') {
    pass "matrix remove --json: escaped path preserved"
} else {
    fail "matrix remove --json: escaped path was lost"
}
if ($env.PWD | path expand) == $matrix_root {
    pass "matrix remove --json: cwd reset to repository root"
} else {
    fail "matrix remove --json: cwd was not reset"
}

wt add matrix-merge | ignore
"merge" | save --force merge.txt
^git add merge.txt
^git commit -m "merge" o+e>| ignore
let matrix_merge_file = ($work | path join "nu-merge.json")
wt merge matrix-merge --json | save --force $matrix_merge_file
let matrix_merge = (open --raw $matrix_merge_file | decode utf-8)
if ($matrix_merge | str contains '"cleaned_up":true') {
    pass "matrix merge --json: command fields preserved"
} else {
    fail "matrix merge --json: missing cleanup field"
}
if ($env.PWD | path expand) == $matrix_root {
    pass "matrix merge --json: cwd reset to repository root"
} else {
    fail "matrix merge --json: cwd was not reset"
}

cd /tmp
^rm -rf $work
print "All nu binding tests passed."
