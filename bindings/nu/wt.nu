# wt — Git worktree manager (Nushell binding)
# Source this file in your config.nu:
#   source path/to/bindings/nu/wt.nu

# Root entrypoint (for `wt` / `wt --help` ergonomics).
#
# Intentionally non-exported: Nushell disallows exporting a command with the
# same name as the module (`wt`). This still works when sourced, which is the
# intended integration path (`wt-core init nu`).
def wt [
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
# Note: Nu uses --json internally with `from json` which is safe (no grep/sed).
# Other shells use --print-paths to avoid fragile JSON parsing.
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

    # Always use --json internally so we can inspect removed_path for cd
    let full_args = (build-args $args $repo true false)

    let result = (^wt-core ...$full_args | from json)

    # If we were inside the removed worktree, cd to repo root
    if ($result.ok) and ($result.removed_path? != null) {
        if ($cwd_before | str starts-with $result.removed_path) {
            cd $result.repo_root
        }
    }

    if $json {
        $result
    } else {
        print $"Removed worktree and branch '($result.branch)'"
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
