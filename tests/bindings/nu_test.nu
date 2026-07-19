#!/usr/bin/env nu
# Integration tests for the Nushell binding.
# Requires wt-core on PATH.

source ../../bindings/nu/wt.nu

let repo_root = (pwd | path expand)
let binding_path = ($repo_root | path join "bindings/nu/wt.nu")

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

# ── wt merge destination metadata ───────────────────────────────────
# Validate every v2 field directly through Nu; the binding's human output is
# printed rather than returned as pipeline data.
wt add feat-protocol
"protocol content" | save protocol.txt
^git add protocol.txt
^git commit -m "protocol content"
let v2_output = (^wt-core merge feat-protocol --repo $expected_repo --no-cleanup --print-paths-v2 | str trim)
let v2_lines = ($v2_output | lines)
let expected_v2 = [$expected_repo, "feat-protocol", "master", "false", "", "false", $expected_repo]
if $v2_lines == $expected_v2 {
    pass "wt merge: validates exact v2 destination protocol"
} else {
    fail $"wt merge: unexpected v2 protocol: ($v2_output)"
}
wt remove feat-protocol

wt add feat-merge
let merge_wt_path = $env.PWD
"merge content" | save merge.txt
^git add merge.txt
^git commit -m "merge content"
wt merge feat-merge
if $env.PWD == $expected_repo {
    pass "wt merge: cd back to destination repository"
} else {
    fail $"wt merge: expected ($expected_repo), got ($env.PWD)"
}
if not ($merge_wt_path | path exists) {
    pass "wt merge: source worktree deleted"
} else {
    fail $"wt merge: ($merge_wt_path) still exists"
}

# ── wt merge --into forwarding ──────────────────────────────────────
^git checkout -b release/nu-into
^git checkout master
let into_destination = $"($work)/repo/.linked-nu-into"
^git worktree add $into_destination release/nu-into
wt add feat-into
"into content" | save into.txt
^git add into.txt
^git commit -m "into content"
let into_result = (wt merge feat-into --into release/nu-into --no-cleanup --json | from json)
if $into_result.mainline == "release/nu-into" {
    pass "wt merge --into: forwards destination branch"
} else {
    fail $"wt merge --into: unexpected destination ($into_result.mainline)"
}
wt remove feat-into --force
^git worktree remove --force $into_destination

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
let matrix_add_path = (($matrix_add | from json).cd_path)
let expected_matrix_path = (^find ($matrix_root | path join ".worktrees") -maxdepth 1 -type d -name "matrix-json--*" -print | str trim)
if $matrix_add_path == $expected_matrix_path {
    pass "matrix add --json: parsed escaped path preserved"
} else {
    fail "matrix add --json: parsed path did not match Git path"
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
let expected_matrix_remove_path = (^find ($matrix_root | path join ".worktrees") -maxdepth 1 -type d -name "matrix-remove--*" -print | str trim)
let matrix_remove_file = ($work | path join "nu-remove.json")
wt remove matrix-remove --json | save --force $matrix_remove_file
let matrix_remove = (open --raw $matrix_remove_file | decode utf-8)
if ($matrix_remove | str contains '"removed_path"') {
    pass "matrix remove --json: command fields preserved"
} else {
    fail "matrix remove --json: missing removed_path"
}
let parsed_removed_path = (($matrix_remove | from json).removed_path)
if $parsed_removed_path == $expected_matrix_remove_path {
    pass "matrix remove --json: parsed escaped path preserved"
} else {
    fail "matrix remove --json: parsed path did not match Git path"
}
if ($env.PWD | path expand) == $matrix_root {
    pass "matrix remove --json: cwd reset to repository root"
} else {
    fail "matrix remove --json: cwd was not reset"
}

# An inherited GIT_COMMON_DIR must not redirect Nu's repository discovery.
wt add matrix-wrong | ignore
cd $matrix_root
let wrong_repo = ($work | path join "nu-wrong-repo")
let wrong_path = (^find ($matrix_root | path join ".worktrees") -maxdepth 1 -type d -name "matrix-wrong--*" -print | str trim)
^git init -b main $wrong_repo o+e>| ignore
$env.GIT_COMMON_DIR = ($wrong_repo | path join ".git")
let wrong_json = (wt remove matrix-wrong --json)
hide-env GIT_COMMON_DIR
if ($wrong_json | str contains '"branch":"matrix-wrong"') and not ($wrong_path | path exists) and ($wrong_repo | path exists) {
    pass "wt remove --json: sanitized repo resolution ignores GIT_COMMON_DIR"
} else {
    fail $"wt remove --json: wrong-repository mutation: ($wrong_json)"
}

wt add matrix-partial | ignore
"partial" | save --force partial.txt
^git add partial.txt
^git commit -m "partial cleanup" o+e>| ignore
cd $matrix_root
let partial_output = (^nu -c $"source ($binding_path); wt remove matrix-partial" | complete)
if $partial_output.exit_code == 0 and ($partial_output.stdout | str contains "kept branch 'matrix-partial'") {
    pass "matrix remove: partial cleanup reports kept branch"
} else {
    fail $"matrix remove: partial cleanup claimed branch deletion: ($partial_output.stdout)"
}
if (^git show-ref --verify --quiet refs/heads/matrix-partial | complete).exit_code == 0 {
    pass "matrix remove: partial cleanup retains branch"
} else {
    fail "matrix remove: partial cleanup deleted branch"
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

# A failed JSON command must still fail when invoked from a child Nu script.
# Conflict responses are structured on stdout; the binding must forward that
# stream before re-raising the child failure.
wt add matrix-conflict | ignore
"source conflict" | save --force conflict.txt
^git add conflict.txt
^git commit -m "source conflict" o+e>| ignore
cd $matrix_root
"destination conflict" | save --force conflict.txt
^git add conflict.txt
^git commit -m "destination conflict" o+e>| ignore
let conflict_script = $"source ($binding_path); wt merge matrix-conflict --json"
let conflict_output = (^nu -c $conflict_script | complete)
if $conflict_output.exit_code != 0 and ($conflict_output.stdout | str contains '"ok":false') and ($conflict_output.stdout | str contains 'content_conflict') {
    pass "wt merge --json: forwards structured conflict stdout on failure"
} else {
    fail $"wt merge --json: expected structured conflict stdout, got ($conflict_output.exit_code), ($conflict_output.stdout), ($conflict_output.stderr)"
}
^wt-core merge --abort --repo $matrix_root o+e>| ignore
wt remove matrix-conflict --force | ignore

let failure_repo = ($work | path join "repo")
let remove_failure_script = $"source ($binding_path); wt remove missing --json --repo ($failure_repo)"
let remove_failure = (^nu -c $remove_failure_script | complete)
if $remove_failure.exit_code != 0 and ($remove_failure.stderr | str contains "no worktree found") {
    pass "wt remove --json: failure script preserves status and diagnostics"
} else {
    fail $"wt remove --json: expected non-zero status and diagnostics, got ($remove_failure.exit_code)"
}

let remove_failure_cwd_script = $"source ($binding_path); try { wt remove missing --repo ($failure_repo) } catch { |err| print $env.PWD; error make $err }"
let expected_failure_cwd = ($env.PWD | path expand)
let remove_failure_cwd = (^nu -c $remove_failure_cwd_script | complete)
if $remove_failure_cwd.exit_code != 0 and ($remove_failure_cwd.stdout | str contains $expected_failure_cwd) {
    pass "wt remove: non-JSON failure restores caller cwd"
} else {
    fail $"wt remove: expected non-JSON failure to restore cwd, got ($remove_failure_cwd.exit_code), ($remove_failure_cwd.stdout)"
}

let merge_failure_script = $"source ($binding_path); wt merge missing --json --repo ($failure_repo)"
let merge_failure = (^nu -c $merge_failure_script | complete)
if $merge_failure.exit_code != 0 and ($merge_failure.stderr | str contains "no worktree found") {
    pass "wt merge --json: failure script preserves status and diagnostics"
} else {
    fail $"wt merge --json: expected non-zero status and diagnostics, got ($merge_failure.exit_code)"
}

cd /tmp
^rm -rf $work
print "All nu binding tests passed."
