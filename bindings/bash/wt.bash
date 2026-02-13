# wt — Git worktree manager (Bash binding)
# Source this file in your .bashrc:
#   source path/to/bindings/bash/wt.bash

wt() {
    local cmd="${1:-}"

    case "$cmd" in
        add)
            shift
            local target
            target=$(wt-core add "$@" --print-cd-path 2>/dev/null)
            if [ $? -eq 0 ] && [ -n "$target" ]; then
                cd "$target" || return 1
            else
                # Re-run without --print-cd-path to show the error message
                wt-core add "$@"
                return $?
            fi
            ;;
        go)
            shift
            local target
            target=$(wt-core go "$@" --print-cd-path 2>/dev/null)
            if [ $? -eq 0 ] && [ -n "$target" ]; then
                cd "$target" || return 1
            else
                wt-core go "$@"
                return $?
            fi
            ;;
        remove)
            shift
            # Detect if the caller explicitly asked for --json
            local want_json=false
            for arg in "$@"; do
                case "$arg" in --json) want_json=true ;; esac
            done

            if [ "$want_json" = true ]; then
                wt-core remove "$@"
                return $?
            fi

            local cwd_before
            cwd_before=$(pwd)
            # --print-paths outputs three lines: removed_path, repo_root, branch
            local result
            result=$(wt-core remove "$@" --print-paths 2>/dev/null)
            local rc=$?
            if [ $rc -eq 0 ]; then
                local removed_path repo_root branch
                removed_path=$(printf '%s\n' "$result" | sed -n '1p')
                repo_root=$(printf '%s\n' "$result" | sed -n '2p')
                branch=$(printf '%s\n' "$result" | sed -n '3p')
                case "$cwd_before" in
                    "${removed_path}"*)
                        cd "$repo_root" || true
                        ;;
                esac
                echo "Removed worktree and branch '${branch}'"
            else
                wt-core remove "$@"
                return $?
            fi
            ;;
        "")
            wt-core --help
            ;;
        *)
            wt-core "$@"
            ;;
    esac
}
