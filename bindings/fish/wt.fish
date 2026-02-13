# wt — Git worktree manager (Fish binding)
# Source this file or place in ~/.config/fish/conf.d/wt.fish

function wt --description "Git worktree manager"
    set -l cmd $argv[1]

    switch "$cmd"
        case add
            set -e argv[1]
            set -l target (wt-core add $argv --print-cd-path 2>/dev/null)
            if test $status -eq 0 -a -n "$target"
                cd "$target"
            else
                wt-core add $argv
                return $status
            end

        case go
            set -e argv[1]
            set -l target (wt-core go $argv --print-cd-path 2>/dev/null)
            if test $status -eq 0 -a -n "$target"
                cd "$target"
            else
                wt-core go $argv
                return $status
            end

        case remove
            set -e argv[1]
            # Detect if the caller explicitly asked for --json
            set -l want_json false
            for arg in $argv
                if test "$arg" = "--json"
                    set want_json true
                end
            end

            if test "$want_json" = true
                set -l cwd_before (pwd)
                set -l output (wt-core remove $argv)
                set -l rc $status
                if test $rc -eq 0
                    # Extract paths from JSON for cd-out-of-removed-worktree logic
                    set -l removed_path (printf '%s\n' $output | sed -n 's/.*"removed_path": *"\([^"]*\)".*/\1/p')
                    set -l repo_root (printf '%s\n' $output | sed -n 's/.*"repo_root": *"\([^"]*\)".*/\1/p')
                    if test -n "$removed_path" -a -n "$repo_root"
                        if string match -q "$removed_path*" "$cwd_before"
                            cd "$repo_root"; or true
                        end
                    end
                end
                printf '%s\n' $output
                return $rc
            end

            set -l cwd_before (pwd)
            # --print-paths outputs three lines: removed_path, repo_root, branch
            set -l lines (wt-core remove $argv --print-paths 2>/dev/null)
            set -l rc $status
            if test $rc -eq 0
                set -l removed_path $lines[1]
                set -l repo_root $lines[2]
                set -l branch $lines[3]
                # Check if cwd is under the removed worktree path
                if string match -q "$removed_path*" "$cwd_before"
                    cd "$repo_root"; or true
                end
                echo "Removed worktree and branch '$branch'"
            else
                wt-core remove $argv
                return $status
            end

        case ''
            wt-core --help

        case '*'
            wt-core $argv  # $argv still includes the subcommand
    end
end
