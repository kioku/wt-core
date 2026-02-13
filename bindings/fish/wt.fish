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
            set -l cwd_before (pwd)
            set -l result (wt-core remove $argv --json 2>/dev/null)
            set -l rc $status
            if test $rc -eq 0
                # Extract fields with string manipulation (no jq required)
                set -l removed_path (echo $result | string match -r '"removed_path":\s*"([^"]*)"' | tail -1)
                set -l repo_root (echo $result | string match -r '"repo_root":\s*"([^"]*)"' | tail -1)
                set -l branch (echo $result | string match -r '"branch":\s*"([^"]*)"' | tail -1)
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
            wt-core $argv
    end
end
