# wt — Git worktree manager (Zsh binding)
# Source this file in your .zshrc:
#   source path/to/bindings/zsh/wt.zsh

wt() {
    emulate -L zsh

    local cmd="${1:-}"

    case "$cmd" in
        add)
            shift

            # Preserve native help/version output.
            local arg
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core add "$@"
                        return $?
                        ;;
                esac
            done

            local target
            target=$(wt-core add "$@" --print-cd-path 2>/dev/null)
            if [[ $? -eq 0 ]] && [[ -n "$target" ]]; then
                cd "$target" || return 1
            else
                wt-core add "$@"
                return $?
            fi
            ;;
        go)
            shift

            # Preserve native help/version output.
            local want_json=false
            local arg
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core go "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            for arg in "$@"; do
                [[ "$arg" == "--json" ]] && want_json=true
            done

            if [[ "$want_json" == true ]]; then
                wt-core go "$@"
                return $?
            fi

            local target rc
            # --print-cd-path works with the interactive picker:
            # the picker UI renders on stderr/tty, the path goes to stdout.
            target=$(wt-core go "$@" --print-cd-path)
            rc=$?
            if [[ $rc -eq 0 ]] && [[ -n "$target" ]]; then
                cd "$target" || return 1
            else
                return $rc
            fi
            ;;
        remove)
            shift

            # Preserve native help/version output.
            local want_json=false
            local arg
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core remove "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            for arg in "$@"; do
                [[ "$arg" == "--json" ]] && want_json=true
            done

            if [[ "$want_json" == true ]]; then
                local cwd_before="${PWD}"
                local output
                output=$(wt-core remove "$@")
                local rc=$?
                if [[ $rc -eq 0 ]]; then
                    # Extract paths from JSON for cd-out-of-removed-worktree logic
                    local removed_path repo_root
                    removed_path=$(printf '%s\n' "$output" | sed -n 's/.*"removed_path": *"\([^"]*\)".*/\1/p')
                    repo_root=$(printf '%s\n' "$output" | sed -n 's/.*"repo_root": *"\([^"]*\)".*/\1/p')
                    if [[ -n "$removed_path" ]] && [[ -n "$repo_root" ]]; then
                        if [[ "$cwd_before" == "${removed_path}"* ]]; then
                            cd "$repo_root" || true
                        fi
                    fi
                fi
                printf '%s\n' "$output"
                return $rc
            fi

            local cwd_before="${PWD}"
            # --print-paths is the stable legacy three-line protocol:
            # removed_path, repo_root, branch. Lifecycle status is explicit in
            # --json; the binding also knows whether --keep-branch was requested.
            local keep_branch=false
            for arg in "$@"; do
                [[ "$arg" == "--keep-branch" ]] && keep_branch=true
            done
            # stderr is left connected to the terminal so the interactive picker
            # (if triggered) renders correctly and errors are visible.
            local result
            result=$(wt-core remove "$@" --print-paths)
            local rc=$?
            if [[ $rc -eq 0 ]]; then
                local removed_path repo_root branch
                removed_path=$(printf '%s\n' "$result" | sed -n '1p')
                repo_root=$(printf '%s\n' "$result" | sed -n '2p')
                branch=$(printf '%s\n' "$result" | sed -n '3p')
                if [[ "$cwd_before" == "${removed_path}"* ]]; then
                    cd "$repo_root" || true
                fi
                if [[ "$keep_branch" == true ]]; then
                    echo "Removed worktree and kept branch '${branch}'"
                else
                    echo "Removed worktree and branch '${branch}'"
                fi
            else
                return $rc
            fi
            ;;
        merge)
            shift

            # Preserve native help/version output.
            local arg
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core merge "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            local want_json=false
            for arg in "$@"; do
                [[ "$arg" == "--json" ]] && want_json=true
            done

            if [[ "$want_json" == true ]]; then
                local cwd_before="${PWD}"
                local output
                output=$(wt-core merge "$@")
                local rc=$?
                if [[ $rc -eq 0 ]]; then
                    local removed_path
                    removed_path=$(printf '%s\n' "$output" | sed -n 's/.*"removed_path": *"\([^"]*\)".*/\1/p')
                    if [[ -n "$removed_path" ]] && [[ "$cwd_before" == "${removed_path}"* ]]; then
                        local repo_root
                        repo_root=$(printf '%s\n' "$output" | sed -n 's/.*"repo_root": *"\([^"]*\)".*/\1/p')
                        cd "$repo_root" || true
                    fi
                fi
                printf '%s\n' "$output"
                return $rc
            fi

            local cwd_before="${PWD}"
            # --print-paths-v2 preserves the six legacy fields and appends
            # destination_path as field seven.
            local result
            result=$(wt-core merge "$@" --print-paths-v2)
            local rc=$?
            if [[ $rc -eq 0 ]]; then
                local repo_root branch mainline cleaned_up removed_path pushed destination_path
                repo_root=$(printf '%s\n' "$result" | sed -n '1p')
                branch=$(printf '%s\n' "$result" | sed -n '2p')
                mainline=$(printf '%s\n' "$result" | sed -n '3p')
                cleaned_up=$(printf '%s\n' "$result" | sed -n '4p')
                removed_path=$(printf '%s\n' "$result" | sed -n '5p')
                pushed=$(printf '%s\n' "$result" | sed -n '6p')
                destination_path=$(printf '%s\n' "$result" | sed -n '7p')
                if [[ "$cleaned_up" == "true" ]] && [[ -n "$removed_path" ]]; then
                    if [[ "$cwd_before" == "${removed_path}"* ]]; then
                        cd "$repo_root" || true
                    fi
                fi
                echo "Merged '${branch}' into ${mainline}"
                echo "Destination worktree: ${destination_path}"
                if [[ "$cleaned_up" == "true" ]]; then
                    echo "Removed worktree and branch '${branch}'"
                fi
                if [[ "$pushed" == "true" ]]; then
                    echo "Pushed ${mainline} to origin"
                fi
            else
                return $rc
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
