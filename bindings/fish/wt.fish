# wt — Git worktree manager (Fish binding)
# Source this file or place in ~/.config/fish/conf.d/wt.fish

# Match complete path components without treating path characters as a glob.
# Call this directly in conditions so Fish preserves its exit status.
function wt__path_is_within
    set -l child $argv[1]
    set -l parent $argv[2]
    if test "$child" = "$parent"
        return 0
    end
    set -l prefix "$parent/"
    test (string sub -s 1 -l (string length -- "$prefix") -- "$child") = "$prefix"
end

function wt__navigation_file
    set -l tmpdir /tmp
    if set -q TMPDIR
        set tmpdir $TMPDIR
    end
    mktemp "$tmpdir/wt-core-nav.XXXXXX"
end

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

            # JSON is a caller-selected machine format. Do not append a
            # path-only flag, which would otherwise change the JSON stream.
            set -l want_json false
            for arg in $argv
                if test "$arg" = "--json"
                    set want_json true
                end
            end
            if test "$want_json" = true
                wt-core add $argv
                return $status
            end

            # Keep stdout private for the path while leaving stderr inherited so
            # setup recommendations and warnings remain visible on success.
            # Run wt-core as a simple command: Fish command substitutions do
            # not inherit a caller's stderr redirection.
            set -l path_file (wt__navigation_file)
            if test $status -ne 0
                return 1
            end
            wt-core add $argv --print-cd-path >"$path_file"
            set -l rc $status
            set -l target (cat "$path_file")
            rm -f -- "$path_file"
            if test $rc -eq 0 -a -n "$target"
                cd "$target"
            else
                return $rc
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
            # Run wt-core as a simple command for caller stderr redirections.
            set -l path_file (wt__navigation_file)
            if test $status -ne 0
                return 1
            end
            wt-core go $argv --print-cd-path >"$path_file"
            set -l rc $status
            set -l target (cat "$path_file")
            rm -f -- "$path_file"
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
                set -l nav_file (wt__navigation_file)
                if test $status -ne 0
                    return 1
                end
                set -l output (wt-core remove $argv --navigation-file "$nav_file")
                set -l rc $status
                if test $rc -eq 0
                    set -l navigation (string split0 < "$nav_file")
                    if test "$navigation[1]" = reset \
                        -a -n "$navigation[2]" \
                        -a -n "$navigation[3]"
                        if wt__path_is_within "$cwd_before" "$navigation[2]"
                            cd "$navigation[3]"; or true
                        end
                    end
                end
                rm -f -- "$nav_file"
                printf '%s\n' $output
                return $rc
            end

            set -l cwd_before (pwd)
            # --print-paths is the stable legacy three-line protocol:
            # removed_path, repo_root, branch. Branch cleanup status is private
            # navigation metadata so partial cleanup is not reported as complete.
            set -l keep_branch false
            for arg in $argv
                if test "$arg" = "--keep-branch"
                    set keep_branch true
                end
            end
            # stderr is left connected to the terminal so the interactive picker
            # (if triggered) renders correctly and errors are visible.
            set -l nav_file (wt__navigation_file)
            set -l lines (wt-core remove $argv --print-paths --navigation-file "$nav_file")
            set -l rc $status
            if test $rc -eq 0
                set -l removed_path $lines[1]
                set -l repo_root $lines[2]
                set -l branch $lines[3]
                set -l navigation (string split0 < "$nav_file")
                set -l branch_deleted false
                if test (count $navigation) -ge 4
                    set branch_deleted $navigation[4]
                end
                rm -f -- "$nav_file"
                # Check if cwd is under the removed worktree path.
                if wt__path_is_within "$cwd_before" "$removed_path"
                    cd "$repo_root"; or true
                end
                if test "$keep_branch" = true -o "$branch_deleted" != true
                    echo "Removed worktree and kept branch '$branch'"
                else
                    echo "Removed worktree and branch '$branch'"
                end
            else
                rm -f -- "$nav_file"
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

            # Status and abort do not remove a worktree. Continue can finish
            # source cleanup, so consume the navigation side channel even in
            # its lifecycle output modes.
            for arg in $argv
                if test "$arg" = "--status" -o "$arg" = "--abort"
                    wt-core merge $argv
                    return $status
                else if test "$arg" = "--continue"
                    set -l cwd_before (pwd)
                    set -l nav_file (wt__navigation_file)
                    if test $status -ne 0
                        return 1
                    end
                    set -l output (wt-core merge $argv --navigation-file "$nav_file")
                    set -l rc $status
                    if test $rc -eq 0 -a -f "$nav_file"
                        set -l navigation (string split0 < "$nav_file")
                        if test "$navigation[1]" = reset \
                            -a -n "$navigation[2]" \
                            -a -n "$navigation[3]"
                            if wt__path_is_within "$cwd_before" "$navigation[2]"
                                cd "$navigation[3]"; or true
                            end
                        end
                    end
                    rm -f -- "$nav_file"
                    printf '%s\n' $output
                    return $rc
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
                set -l nav_file (wt__navigation_file)
                if test $status -ne 0
                    return 1
                end
                set -l output (wt-core merge $argv --navigation-file "$nav_file")
                set -l rc $status
                if test $rc -eq 0
                    set -l navigation (string split0 < "$nav_file")
                    if test "$navigation[1]" = reset \
                        -a -n "$navigation[2]" \
                        -a -n "$navigation[3]"
                        if wt__path_is_within "$cwd_before" "$navigation[2]"
                            cd "$navigation[3]"; or true
                        end
                    end
                end
                rm -f -- "$nav_file"
                printf '%s\n' $output
                return $rc
            end

            for arg in $argv
                if test "$arg" = "--inspect"
                    wt-core merge $argv
                    return $status
                end
            end

            set -l cwd_before (pwd)
            # --print-paths-v2 preserves the six legacy fields and appends
            # destination_path as field seven.
            set -l lines (wt-core merge $argv --print-paths-v2)
            set -l rc $status
            if test $rc -eq 0
                set -l repo_root $lines[1]
                set -l branch $lines[2]
                set -l mainline $lines[3]
                set -l cleaned_up $lines[4]
                set -l removed_path $lines[5]
                set -l pushed $lines[6]
                set -l destination_path $lines[7]
                # A worktree may be gone even when branch cleanup is pending;
                # never leave the caller inside that deleted directory.
                if test -n "$removed_path"
                    if wt__path_is_within "$cwd_before" "$removed_path"
                        cd "$repo_root"; or true
                    end
                end
                echo "Merged '$branch' into $mainline"
                echo "Destination worktree: $destination_path"
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
