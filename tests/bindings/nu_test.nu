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
if ($help_output | str contains "wt add") and ($help_output | str contains "Subcommands") {
    pass "wt --help: root command help available"
} else {
    fail "wt --help: missing expected command help output"
}

# ── wt add ───────────────────────────────────────────────────────────
wt add feat-one
if ($env.PWD | str contains ".worktrees") and ($env.PWD | str contains "feat-one") {
    pass "wt add: cd into new worktree"
} else {
    fail $"wt add: expected cwd inside .worktrees/…feat-one…, got ($env.PWD)"
}

let wt_path = $env.PWD

# ── wt list ──────────────────────────────────────────────────────────
let output = (wt list | str join "\n")
if ($output | str contains "feat-one") {
    pass "wt list: output contains branch name"
} else {
    fail "wt list: 'feat-one' not found in output"
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

cd /tmp
^rm -rf $work
print "All nu binding tests passed."
