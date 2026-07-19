# wt — Git worktree manager (Nushell binding)
# Source this file in your config.nu:
#   source path/to/bindings/nu/wt.nu

# Root entrypoint (for `wt` / `wt --help` ergonomics).
#
# Intentionally non-exported: Nushell disallows exporting a command with the
# same name as the module (`wt`). This still works when sourced, which is the
# intended integration path (`wt-core init nu`).
def --wrapped wt [
    ...args: string  # Optional passthrough args for wt-core
] {
    if ($args | is-empty) {
        ^wt-core --help
    } else {
        ^wt-core ...$args
    }
}

# Return true only when child is the parent itself or a descendant directory.
# `path relative-to` also succeeds for siblings (`../sibling`), so compare
# complete path components instead of treating successful normalization as a
# containment proof.
def path-is-within [child: string parent: string] {
    let child = ($child | path expand)
    let parent = ($parent | path expand)
    if $child == $parent { true } else {
        let prefix = if $parent == "/" { "/" } else { $"($parent)/" }
        $child | str starts-with $prefix
    }
}

# Read the NUL-delimited navigation record written by wt-core. This keeps
# paths out of JSON parsing and preserves quotes and backslashes verbatim.
def read-navigation [file: path] {
    open --raw $file
    | bytes split 0x[00]
    | each { |field| $field | decode utf-8 }
}

def navigation-file [] {
    let tmpdir = ($env.TMPDIR? | default "/tmp")
    ^mktemp $"($tmpdir)/wt-core-nav.XXXXXX" | str trim
}

# Nushell turns a failed external command into a ShellError. Re-raise that
# error instead of returning, so the original stderr remains visible and the
# caller receives wt-core's non-zero exit status.
def run-core [args: list<string>] {
    try { ^wt-core ...$args } catch { |err| error make $err }
}

# List all worktrees
export def "wt list" [
    --repo: path        # Repository path (defaults to cwd)
    --json              # Output as JSON
    --stats             # Include commit and diff stats for each worktree
    --against: string   # Compare stats against this revision (requires --stats)
    --color: string     # When to color stats output: auto, always, never
] {
    mut args = ["list"]
    if $stats { $args = ($args | append "--stats") }
    if $against != null { $args = ($args | append ["--against" $against]) }
    if $color != null { $args = ($args | append ["--color" $color]) }

    let full_args = (build-args $args $repo $json false)
    if $json {
        let output = (run-core $full_args)
        $output
    } else {
        ^wt-core ...$full_args
    }
}

# Create a new worktree and cd into it
export def --env "wt add" [
    branch: string      # Branch name to create
    --base: string      # Base revision (defaults to HEAD)
    --repo: path        # Repository path (defaults to cwd)
    --json              # Output as JSON (no cd)
] {
    # JSON is the canonical machine format; keep it separate from the
    # legacy path-only output used for the parent-shell cd.
    if $json {
        mut args = (build-args ["add" $branch] $repo true false)
        if $base != null { $args = ($args | append ["--base" $base]) }
        let output = (run-core $args)
        $output
    } else {
        mut args = (build-args ["add" $branch] $repo false true)
        if $base != null { $args = ($args | append ["--base" $base]) }
        let target = (^wt-core ...$args | str trim)
        cd $target
    }
}

# Switch to an existing worktree
export def --env "wt go" [
    branch?: string       # Branch name (omit for interactive picker)
    --repo: path          # Repository path (defaults to cwd)
    --json                # Output as JSON (no cd)
    --interactive(-i)     # Force the interactive picker (skip auto-select)
] {
    mut args = ["go"]
    if $branch != null { $args = ($args | append $branch) }
    if $interactive { $args = ($args | append "--interactive") }

    if $json {
        let full_args = (build-args $args $repo true false)
        let output = (run-core $full_args)
        $output
    } else {
        # --print-cd-path works with the interactive picker:
        # the picker UI renders on stderr/tty, the path goes to stdout.
        let full_args = (build-args $args $repo false true)
        let target = (^wt-core ...$full_args | str trim)
        cd $target
    }
}

# Remove a worktree, optionally preserving its local branch
export def --env "wt remove" [
    branch?: string  # Branch name (defaults to current worktree)
    --force          # Force removal even if dirty
    --keep-branch    # Preserve the local branch after removing its worktree
    --repo: path     # Repository path (defaults to cwd)
    --json           # Output as JSON
] {
    let cwd_before = (pwd)

    mut args = ["remove"]
    if $branch != null { $args = ($args | append $branch) }
    if $force { $args = ($args | append "--force") }
    if $keep_branch { $args = ($args | append "--keep-branch") }

    if $json {
        # Run from the repository root so removing the current worktree does
        # not invalidate Nushell's own cwd while it captures stdout.
        let command_repo = if $repo != null {
            $repo | path expand
        } else {
            try {
                ^git rev-parse --path-format=absolute --git-common-dir
                | str trim
                | path dirname
            } catch { |err| error make $err }
        }
        if $branch == null {
            let inferred_branch = try { ^git branch --show-current | str trim } catch { "" }
            if $inferred_branch != "" {
                $args = ($args | append $inferred_branch)
            }
        }
        cd $command_repo
        let effective_repo = if $repo != null { $command_repo } else { null }
        let nav_file = (navigation-file)
        let full_args = (build-args $args $effective_repo true false | append ["--navigation-file" $nav_file])
        let output = try { run-core $full_args } catch { |err|
            cd $cwd_before
            ^rm -f $nav_file
            error make $err
        }
        let navigation = if ($nav_file | path exists) {
            try { read-navigation $nav_file } catch { [] }
        } else {
            []
        }
        if (
            (($navigation | get 0 | default "") == "reset")
            and (($navigation | get 1 | default "") != "")
            and (($navigation | get 2 | default "") != "")
            and (path-is-within $cwd_before ($navigation | get 1))
        ) {
            cd ($navigation | get 2)
        } else {
            cd $cwd_before
        }
        ^rm -f $nav_file
        $output
    } else {
        # --print-paths allows the interactive picker to render on
        # stderr/tty while paths go to stdout (same pattern as `go`
        # with --print-cd-path).
        # Capture stdout separately from the pipeline so that a
        # non-zero exit code raises an error (piping through `| lines`
        # directly would silently swallow the failure). Stderr is inherited,
        # keeping the interactive picker and error messages visible.
        # Resolve the branch before moving to the main worktree. Running the
        # destructive operation from the repository root keeps Nushell's cwd
        # valid even when the selected worktree is removed.
        let command_repo = if $repo != null {
            $repo | path expand
        } else {
            try {
                ^git rev-parse --path-format=absolute --git-common-dir
                | str trim
                | path dirname
            } catch { |err| error make $err }
        }
        if $branch == null {
            let inferred_branch = try { ^git branch --show-current | str trim } catch { "" }
            if $inferred_branch != "" {
                $args = ($args | append $inferred_branch)
            }
        }
        cd $command_repo
        let effective_repo = if $repo != null { $command_repo } else { null }
        # --print-paths is the stable legacy three-line protocol:
        # removed_path, repo_root, branch. Lifecycle status is explicit in
        # --json; the binding already knows whether --keep-branch was requested.
        let full_args = (build-args $args $effective_repo false false | append "--print-paths")
        let output = (run-core $full_args)
        let lines = ($output | lines)
        let removed_path = ($lines | get 0)
        let repo_root = ($lines | get 1)
        let branch_name = ($lines | get 2)

        if (path-is-within $cwd_before $removed_path) {
            cd $repo_root
        }

        if $keep_branch {
            print $"Removed worktree and kept branch '($branch_name)'"
        } else {
            print $"Removed worktree and branch '($branch_name)'"
        }
    }
}

# Merge a worktree's branch into mainline and clean up
export def --env "wt merge" [
    branch?: string  # Branch name (defaults to current worktree)
    --into: string   # Merge into a branch checked out in any worktree
    --inspect        # Inspect topology without mutating the repository
    --push           # Push mainline to origin after merge
    --no-cleanup     # Keep worktree and branch after merge
    --repo: path     # Repository path (defaults to cwd)
    --json           # Output as JSON
] {
    let cwd_before = (pwd)

    mut args = ["merge"]
    if $branch != null { $args = ($args | append $branch) }
    if $into != null { $args = ($args | append ["--into" $into]) }
    if $inspect { $args = ($args | append "--inspect") }
    if $push { $args = ($args | append "--push") }
    if $no_cleanup { $args = ($args | append "--no-cleanup") }

    if $inspect {
        # Inspection is a read-only protocol. Do not add navigation metadata or
        # move cwd; the core command never prunes or mutates in this mode.
        let full_args = (build-args $args $repo $json false)
        if $json {
            run-core $full_args
        } else {
            ^wt-core ...$full_args
        }
    } else if $json {
        # Run from the repository root so cleanup cannot invalidate Nushell's
        # cwd while it captures stdout.
        let command_repo = if $repo != null {
            $repo | path expand
        } else {
            try {
                ^git rev-parse --path-format=absolute --git-common-dir
                | str trim
                | path dirname
            } catch { |err| error make $err }
        }
        if $branch == null {
            let inferred_branch = try { ^git branch --show-current | str trim } catch { "" }
            if $inferred_branch != "" {
                $args = ($args | append $inferred_branch)
            }
        }
        cd $command_repo
        let effective_repo = if $repo != null { $command_repo } else { null }
        let nav_file = (navigation-file)
        let full_args = (build-args $args $effective_repo true false | append ["--navigation-file" $nav_file])
        let output = try { run-core $full_args } catch { |err|
            cd $cwd_before
            ^rm -f $nav_file
            error make $err
        }
        let navigation = if ($nav_file | path exists) {
            try { read-navigation $nav_file } catch { [] }
        } else {
            []
        }
        if (
            (($navigation | get 0 | default "") == "reset")
            and (($navigation | get 1 | default "") != "")
            and (($navigation | get 2 | default "") != "")
            and (path-is-within $cwd_before ($navigation | get 1))
        ) {
            cd ($navigation | get 2)
        } else {
            cd $cwd_before
        }
        ^rm -f $nav_file
        $output
    } else {
        # Resolve the branch before moving to the main worktree. Running the
        # merge from the repository root keeps Nushell's cwd valid when cleanup
        # removes the selected worktree.
        let command_repo = if $repo != null {
            $repo | path expand
        } else {
            try {
                ^git rev-parse --path-format=absolute --git-common-dir
                | str trim
                | path dirname
            } catch { |err| error make $err }
        }
        if $branch == null {
            let inferred_branch = try { ^git branch --show-current | str trim } catch { "" }
            if $inferred_branch != "" {
                $args = ($args | append $inferred_branch)
            }
        }
        cd $command_repo
        let effective_repo = if $repo != null { $command_repo } else { null }
        # --print-paths-v2 preserves the six legacy fields and appends
        # destination_path as field seven. This lets the binding expose
        # linked-worktree merge destinations without changing v1.
        let full_args = (build-args $args $effective_repo false false | append "--print-paths-v2")
        let output = (run-core $full_args)
        let lines = ($output | lines)
        let repo_root = ($lines | get 0)
        let branch_name = ($lines | get 1)
        let mainline = ($lines | get 2)
        let cleaned_up = ($lines | get 3)
        let removed_path = ($lines | get 4)
        let pushed = ($lines | get 5)
        let destination_path = ($lines | get 6)

        if $cleaned_up == "true" and $removed_path != "" {
            if (path-is-within $cwd_before $removed_path) {
                cd $repo_root
            }
        }

        print $"Merged '($branch_name)' into ($mainline)"
        print $"Destination worktree: ($destination_path)"
        if $cleaned_up == "true" {
            print $"Removed worktree and branch '($branch_name)'"
        }
        if $pushed == "true" {
            print $"Pushed ($mainline) to origin"
        }
    }
}

# Diagnose worktree health
export def "wt doctor" [
    --repo: path  # Repository path (defaults to cwd)
    --json        # Output as JSON
] {
    let args = (build-args ["doctor"] $repo $json false)
    if $json {
        let output = (run-core $args)
        $output
    } else {
        ^wt-core ...$args
    }
}

# Build the argument list for wt-core
def build-args [
    base_args: list<string>
    repo: any
    json: bool
    cd_path: bool
] {
    mut args = $base_args
    if $repo != null { $args = ($args | append ["--repo" ($repo | into string)]) }
    if $json { $args = ($args | append "--json") }
    if $cd_path { $args = ($args | append "--print-cd-path") }
    $args
}
