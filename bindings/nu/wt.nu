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

# List all worktrees
export def "wt list" [
    --repo: path  # Repository path (defaults to cwd)
    --json        # Output as JSON
] {
    let args = (build-args ["list"] $repo $json false)
    if $json {
        ^wt-core ...$args | from json
    } else {
        ^wt-core ...$args
    }
}

# Create a new worktree and cd into it
export def --env "wt add" [
    branch: string      # Branch name to create
    --base: string      # Base revision (defaults to HEAD)
    --repo: path        # Repository path (defaults to cwd)
    --json              # Output as JSON (no cd)
] {
    if $json {
        mut args = (build-args ["add" $branch] $repo true false)
        if $base != null { $args = ($args | append ["--base" $base]) }
        ^wt-core ...$args | from json
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
        ^wt-core ...$full_args | from json
    } else {
        # --print-cd-path works with the interactive picker:
        # the picker UI renders on stderr/tty, the path goes to stdout.
        let full_args = (build-args $args $repo false true)
        let target = (^wt-core ...$full_args | str trim)
        cd $target
    }
}

# Remove a worktree and its local branch
export def --env "wt remove" [
    branch?: string  # Branch name (defaults to current worktree)
    --force          # Force removal even if dirty
    --repo: path     # Repository path (defaults to cwd)
    --json           # Output as JSON
] {
    let cwd_before = (pwd)

    mut args = ["remove"]
    if $branch != null { $args = ($args | append $branch) }
    if $force { $args = ($args | append "--force") }

    if $json {
        # --json: machine output, no interactive picker.
        let full_args = (build-args $args $repo true false)
        let result = (^wt-core ...$full_args | from json)

        if ($result.ok) and ($result.removed_path? != null) {
            if ($cwd_before | str starts-with $result.removed_path) {
                cd $result.repo_root
            }
        }

        $result
    } else {
        # --print-paths: allows the interactive picker to render on
        # stderr/tty while paths go to stdout (same pattern as `go`
        # with --print-cd-path).
        let full_args = (build-args $args $repo false false | append "--print-paths")
        # Capture stdout separately from the pipeline so that a
        # non-zero exit code raises an error (piping through `| lines`
        # directly would silently swallow the failure).  Stderr is
        # inherited, keeping the interactive picker and error messages
        # visible in the terminal.
        let output = try { ^wt-core ...$full_args } catch { return }
        let lines = ($output | lines)
        let removed_path = ($lines | get 0)
        let repo_root = ($lines | get 1)
        let branch_name = ($lines | get 2)

        if ($cwd_before | str starts-with $removed_path) {
            cd $repo_root
        }

        print $"Removed worktree and branch '($branch_name)'"
    }
}

# Merge a worktree's branch into mainline and clean up
export def --env "wt merge" [
    branch?: string  # Branch name (defaults to current worktree)
    --push           # Push mainline to origin after merge
    --no-cleanup     # Keep worktree and branch after merge
    --repo: path     # Repository path (defaults to cwd)
    --json           # Output as JSON
] {
    let cwd_before = (pwd)

    mut args = ["merge"]
    if $branch != null { $args = ($args | append $branch) }
    if $push { $args = ($args | append "--push") }
    if $no_cleanup { $args = ($args | append "--no-cleanup") }

    if $json {
        let full_args = (build-args $args $repo true false)
        let result = (^wt-core ...$full_args | from json)

        if ($result.ok) and ($result.removed_path? != null) {
            if ($cwd_before | str starts-with ($result.removed_path)) {
                cd $result.repo_root
            }
        }

        $result
    } else {
        let full_args = (build-args $args $repo false false | append "--print-paths")
        let output = try { ^wt-core ...$full_args } catch { return }
        let lines = ($output | lines)
        let repo_root = ($lines | get 0)
        let branch_name = ($lines | get 1)
        let mainline = ($lines | get 2)
        let cleaned_up = ($lines | get 3)
        let removed_path = ($lines | get 4)
        let pushed = ($lines | get 5)

        if $cleaned_up == "true" and $removed_path != "" {
            if ($cwd_before | str starts-with $removed_path) {
                cd $repo_root
            }
        }

        print $"Merged '($branch_name)' into ($mainline)"
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
        ^wt-core ...$args | from json
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
