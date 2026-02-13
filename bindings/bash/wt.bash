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

            local cwd_before
            cwd_before=$(pwd)
            local result
            result=$(wt-core remove "$@" --json 2>/dev/null)
            local rc=$?
            if [ $rc -eq 0 ]; then
                local removed_path repo_root branch
                removed_path=$(printf '%s' "$result" | grep '"removed_path"' | sed 's/.*": "//;s/".*//')
                repo_root=$(printf '%s' "$result" | grep '"repo_root"' | sed 's/.*": "//;s/".*//')
                branch=$(printf '%s' "$result" | grep '"branch"' | sed 's/.*": "//;s/".*//')
                case "$cwd_before" in
                    "${removed_path}"*)
                        cd "$repo_root" || true
                        ;;
                esac
                if [ "$want_json" = true ]; then
                    printf '%s\n' "$result"
                else
                    echo "Removed worktree and branch '${branch}'"
                fi
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
