# wt — Git worktree manager (Fish binding)
# Source this file or place in ~/.config/fish/conf.d/wt.fish

function wt --description "Git worktree manager"
    set -l cmd $argv[1]

    switch "$cmd"
        case add
            set -e argv[1]

            # Preserve native help/version output.
            for arg in $argv
                if test "$arg" = "-h" -o "$arg" = "--help" -o "$arg" = "-V" -o "$arg" = "--version"
                    wt-core add $argv
                    return $status
                end
            end

            set -l target (wt-core add $argv --print-cd-path 2>/dev/null)
            if test $status -eq 0 -a -n "$target"
                cd "$target"
            else
                wt-core add $argv
                return $status
            end

        case go
            set -e argv[1]

            # Preserve native help/version output.
            for arg in $argv
                if test "$arg" = "-h" -o "$arg" = "--help" -o "$arg" = "-V" -o "$arg" = "--version"
                    wt-core go $argv
                    return $status
                end
            end

            # Detect if the caller explicitly asked for --json
            set -l want_json false
            for arg in $argv
                if test "$arg" = "--json"
                    set want_json true
                end
            end

            if test "$want_json" = true
                wt-core go $argv
                return $status
            end

            # --print-cd-path works with the interactive picker:
            # the picker UI renders on stderr/tty, the path goes to stdout.
            set -l target (wt-core go $argv --print-cd-path)
            set -l rc $status
            if test $rc -eq 0 -a -n "$target"
                cd "$target"
            else
                return $rc
            end

        case remove
            set -e argv[1]

            # Preserve native help/version output.
            for arg in $argv
                if test "$arg" = "-h" -o "$arg" = "--help" -o "$arg" = "-V" -o "$arg" = "--version"
                    wt-core remove $argv
                    return $status
                end
            end

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
            # --print-paths outputs three lines: removed_path, repo_root, branch.
            # stderr is left connected to the terminal so the interactive picker
            # (if triggered) renders correctly and errors are visible.
            set -l lines (wt-core remove $argv --print-paths)
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
                return $rc
            end

        case merge
            set -e argv[1]

            # Preserve native help/version output.
            for arg in $argv
                if test "$arg" = "-h" -o "$arg" = "--help" -o "$arg" = "-V" -o "$arg" = "--version"
                    wt-core merge $argv
                    return $status
                end
            end

            # Detect if the caller explicitly asked for --json
            set -l want_json false
            for arg in $argv
                if test "$arg" = "--json"
                    set want_json true
                end
            end

            if test "$want_json" = true
                set -l cwd_before (pwd)
                set -l output (wt-core merge $argv)
                set -l rc $status
                if test $rc -eq 0
                    set -l repo_root (printf '%s\n' $output | sed -n 's/.*"repo_root": *"\([^"]*\)".*/\1/p')
                    set -l cleaned_up (printf '%s\n' $output | sed -n 's/.*"cleaned_up": *\(true\|false\).*/\1/p')
                    if test "$cleaned_up" = "true" -a -n "$repo_root"
                        if string match -q "$repo_root/.worktrees/*" "$cwd_before"
                            cd "$repo_root"; or true
                        end
                    end
                end
                printf '%s\n' $output
                return $rc
            end

            set -l cwd_before (pwd)
            # --print-paths outputs: repo_root, branch, mainline, cleaned_up, pushed
            set -l lines (wt-core merge $argv --print-paths)
            set -l rc $status
            if test $rc -eq 0
                set -l repo_root $lines[1]
                set -l branch $lines[2]
                set -l mainline $lines[3]
                set -l cleaned_up $lines[4]
                set -l pushed $lines[5]
                if test "$cleaned_up" = "true"
                    if string match -q "$repo_root/.worktrees/*" "$cwd_before"
                        cd "$repo_root"; or true
                    end
                end
                echo "Merged '$branch' into $mainline"
                if test "$cleaned_up" = "true"
                    echo "Removed worktree and branch '$branch'"
                end
                if test "$pushed" = "true"
                    echo "Pushed $mainline to origin"
                end
            else
                return $rc
            end

        case ''
            wt-core --help

        case '*'
            wt-core $argv  # $argv still includes the subcommand
    end
end
