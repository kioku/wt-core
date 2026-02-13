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
                wt-core remove $argv
                return $status
            end

            set -l cwd_before (pwd)
            # --print-paths outputs two lines: removed_path then repo_root
            set -l lines (wt-core remove $argv --print-paths 2>/dev/null)
            set -l rc $status
            if test $rc -eq 0
                set -l removed_path $lines[1]
                set -l repo_root $lines[2]
                # Check if cwd is under the removed worktree path
                if string match -q "$removed_path*" "$cwd_before"
                    cd "$repo_root"; or true
                end
                echo "Removed worktree and branch "(basename "$removed_path" | string replace -r -- '--[0-9a-f]*$' '')"'"
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
