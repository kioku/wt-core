#!/usr/bin/env nu
# Native shell contract: only Git, wt-core, and Nushell are required on PATH.
source ../../bindings/nu/wt.nu

use std assert

let binding = ($env.FILE_PWD | path join "../../bindings/nu/wt.nu" | path expand)
let work = (mktemp --directory)
let repo = ($work | path join "repo")
^git init --initial-branch=main $repo o+e>| ignore
cd $repo
^git config user.name "Binding Test"
^git config user.email "binding@example.invalid"
^git commit --allow-empty -m initial o+e>| ignore

assert (path-is-within ($repo | path join "nested/deep") $repo)
assert (path-is-within $repo $repo)
assert (not (path-is-within ($work | path join "repo-copy") $repo))

wt add nested-remove
let source_root = (pwd)
let nested = ($source_root | path join "nested/deep")
mkdir $nested
cd $nested
let removed = (wt remove --json | from json)
assert $removed.ok
assert ((pwd | path expand) == ($repo | path expand))
assert (not ($source_root | path exists))

wt add nested-merge
let merge_root = (pwd)
"merged\n" | save ($merge_root | path join "merged.txt")
^git add merged.txt
^git commit -m merged o+e>| ignore
let nested = ($merge_root | path join "nested/deep")
mkdir $nested
cd $nested
let merged = (wt merge --json | from json)
assert $merged.ok
assert ((pwd | path expand) == ($repo | path expand))
assert (not ($merge_root | path exists))

# Catching a failed command must not discard its structured stdout or change cwd.
let failed_script = $'source "($binding)"; try { wt remove missing --repo "($repo)" --json } catch { print --stderr "expected failure"; exit 1 }'
let failure = (^nu -c $failed_script | complete)
assert ($failure.exit_code != 0)
assert ($failure.stderr | str contains "no worktree found")
assert ((pwd | path expand) == ($repo | path expand))

# Direct forwarding survives error unwinding without external printf.
let forward_script = $'source "($binding)"; try { forward-core-failure { stdout: "structured-output", stderr: "diagnostic", exit_code: 5 }; error make {msg: "expected"} } catch { exit 1 }'
let forwarded = (^nu -c $forward_script | complete)
assert ($forwarded.exit_code != 0)
assert ($forwarded.stdout == "structured-output")
assert ($forwarded.stderr | str contains "diagnostic")

cd $work
rm --recursive --force $repo
print "Portable Nushell navigation and forwarding passed."
