#!/usr/bin/env nu
# Integration tests for the Nushell binding.
# Requires wt-core on PATH.

use ../../bindings/nu/wt.nu *

def pass [msg: string] { print $"  ✓ ($msg)" }
def fail [msg: string] { print $"  ✗ ($msg)"; exit 1 }

let work = (^mktemp -d | str trim)

^git init $"($work)/repo" o+e>| ignore
cd $"($work)/repo"
^git config user.name "test"
^git config user.email "test@test.com"
^git commit --allow-empty -m "initial" o+e>| ignore

print "Running nu binding tests..."

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
if $env.PWD == $"($work)/repo" {
    pass "wt remove: cd back to repo root"
} else {
    fail $"wt remove: expected ($work)/repo, got ($env.PWD)"
}

if not ($wt_path | path exists) {
    pass "wt remove: worktree directory deleted"
} else {
    fail $"wt remove: ($wt_path) still exists"
}

cd /tmp
^rm -rf $work
print "All nu binding tests passed."
