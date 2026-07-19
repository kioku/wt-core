# wt — Git worktree manager (Bash binding)
# Source this file in your .bashrc:
#   source path/to/bindings/bash/wt.bash

# Match complete path components, not merely a textual prefix. This keeps a
# worktree named /repo/.worktrees/app from matching /repo/.worktrees/app-copy.
wt__path_is_within() {
    [[ "$1" == "$2" || "$1" == "$2"/* ]]
}

wt() {
    local cmd="${1:-}"

    case "$cmd" in
        add)
            shift

            # Preserve native help/version output.
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core add "$@"
                        return $?
                        ;;
                esac
            done

            # JSON is a caller-selected machine format. Do not append a
            # path-only flag, which would otherwise change the JSON stream.
            local want_json=false
            for arg in "$@"; do
                case "$arg" in --json) want_json=true ;; esac
            done
            if [ "$want_json" = true ]; then
                wt-core add "$@"
                return $?
            fi

            local target rc
            # Keep stdout private for the path while leaving stderr inherited so
            # setup recommendations and warnings remain visible on success.
            target=$(wt-core add "$@" --print-cd-path)
            rc=$?
            if [ $rc -eq 0 ] && [ -n "$target" ]; then
                cd "$target" || return 1
            else
                return $rc
            fi
            ;;
        go)
            shift

            # Preserve native help/version output.
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core go "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            local want_json=false
            for arg in "$@"; do
                case "$arg" in --json) want_json=true ;; esac
            done

            if [ "$want_json" = true ]; then
                wt-core go "$@"
                return $?
            fi

            local target rc
            # --print-cd-path works with the interactive picker:
            # the picker UI renders on stderr/tty, the path goes to stdout.
            target=$(wt-core go "$@" --print-cd-path)
            rc=$?
            if [ $rc -eq 0 ] && [ -n "$target" ]; then
                cd "$target" || return 1
            else
                return $rc
            fi
            ;;
        remove)
            shift

            # Preserve native help/version output.
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core remove "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            local want_json=false
            for arg in "$@"; do
                case "$arg" in --json) want_json=true ;; esac
            done

            if [ "$want_json" = true ]; then
                local cwd_before nav_file output rc
                cwd_before=$(pwd -P)
                nav_file=$(mktemp "${TMPDIR:-/tmp}/wt-core-nav.XXXXXX") || return 1
                output=$(wt-core remove "$@" --navigation-file "$nav_file")
                rc=$?
                if [ $rc -eq 0 ]; then
                    local -a navigation
                    mapfile -d '' -t navigation < "$nav_file"
                    if [ "${navigation[0]-}" = reset ] \
                        && [ -n "${navigation[1]-}" ] \
                        && [ -n "${navigation[2]-}" ] \
                        && wt__path_is_within "$cwd_before" "${navigation[1]}"; then
                        cd "${navigation[2]}" || true
                    fi
                fi
                rm -f "$nav_file"
                printf '%s\n' "$output"
                return $rc
            fi

            local cwd_before
            cwd_before=$(pwd -P)
            # --print-paths is the stable legacy three-line protocol:
            # removed_path, repo_root, branch. Lifecycle status is explicit in
            # --json; the binding also knows whether --keep-branch was requested.
            local keep_branch=false
            for arg in "$@"; do
                [ "$arg" = "--keep-branch" ] && keep_branch=true
            done
            # stderr is left connected to the terminal so the interactive picker
            # (if triggered) renders correctly and errors are visible.
            local result
            result=$(wt-core remove "$@" --print-paths)
            local rc=$?
            if [ $rc -eq 0 ]; then
                local removed_path repo_root branch
                removed_path=$(printf '%s\n' "$result" | sed -n '1p')
                repo_root=$(printf '%s\n' "$result" | sed -n '2p')
                branch=$(printf '%s\n' "$result" | sed -n '3p')
                if wt__path_is_within "$cwd_before" "$removed_path"; then
                    cd "$repo_root" || true
                fi
                if [ "$keep_branch" = true ]; then
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
            for arg in "$@"; do
                case "$arg" in
                    -h|--help|-V|--version)
                        wt-core merge "$@"
                        return $?
                        ;;
                esac
            done

            # Status, continue, and abort are lifecycle reports, not the
            # legacy navigation protocol or path-only output protocol.
            for arg in "$@"; do
                case "$arg" in
                    --status|--continue|--abort)
                        wt-core merge "$@"
                        return $?
                        ;;
                esac
            done

            # Detect if the caller explicitly asked for --json
            local want_json=false
            for arg in "$@"; do
                case "$arg" in --json) want_json=true ;; esac
            done

            if [ "$want_json" = true ]; then
                local cwd_before nav_file output rc
                cwd_before=$(pwd -P)
                nav_file=$(mktemp "${TMPDIR:-/tmp}/wt-core-nav.XXXXXX") || return 1
                output=$(wt-core merge "$@" --navigation-file "$nav_file")
                rc=$?
                if [ $rc -eq 0 ]; then
                    local -a navigation
                    mapfile -d '' -t navigation < "$nav_file"
                    if [ "${navigation[0]-}" = reset ] \
                        && [ -n "${navigation[1]-}" ] \
                        && [ -n "${navigation[2]-}" ] \
                        && wt__path_is_within "$cwd_before" "${navigation[1]}"; then
                        cd "${navigation[2]}" || true
                    fi
                fi
                rm -f "$nav_file"
                printf '%s\n' "$output"
                return $rc
            fi

            for arg in "$@"; do
                if [ "$arg" = "--inspect" ]; then
                    wt-core merge "$@"
                    return $?
                fi
            done

            local cwd_before
            cwd_before=$(pwd -P)
            # --print-paths-v2 preserves the six legacy fields and appends
            # destination_path as field seven.
            local result
            result=$(wt-core merge "$@" --print-paths-v2)
            local rc=$?
            if [ $rc -eq 0 ]; then
                local repo_root branch mainline cleaned_up removed_path pushed destination_path
                repo_root=$(printf '%s\n' "$result" | sed -n '1p')
                branch=$(printf '%s\n' "$result" | sed -n '2p')
                mainline=$(printf '%s\n' "$result" | sed -n '3p')
                cleaned_up=$(printf '%s\n' "$result" | sed -n '4p')
                removed_path=$(printf '%s\n' "$result" | sed -n '5p')
                pushed=$(printf '%s\n' "$result" | sed -n '6p')
                destination_path=$(printf '%s\n' "$result" | sed -n '7p')
                if [ "$cleaned_up" = "true" ] && [ -n "$removed_path" ]; then
                    if wt__path_is_within "$cwd_before" "$removed_path"; then
                        cd "$repo_root" || true
                    fi
                fi
                echo "Merged '${branch}' into ${mainline}"
                echo "Destination worktree: ${destination_path}"
                if [ "$cleaned_up" = "true" ]; then
                    echo "Removed worktree and branch '${branch}'"
                fi
                if [ "$pushed" = "true" ]; then
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
