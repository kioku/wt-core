# Plan: Fix shell binding fragility and bugs

## 1. Summary

The Bash and Zsh `remove` wrappers parse JSON using `grep`+`sed`, which breaks on paths containing double quotes or if field ordering changes. The Fish binding has a likely bug in its `string match` glob comparison for detecting whether cwd is inside the removed worktree. The fix is to add a `--print-removed-path` flag (or a combined machine-output mode) to `wt-core remove`, so POSIX wrappers can avoid JSON parsing entirely, and to correct the Fish glob logic.

## 2. Branching Strategy

`fix/shell-binding-remove-parsing`

## 3. Investigation

1. Confirm the Fish bug: test whether `string match -q "$removed_path*" "$cwd_before"` correctly detects "cwd is under removed_path" or if it's reversed/broken with special characters.
2. Decide between two approaches for the POSIX wrappers:
   - **Option A:** Add two flags to `wt-core remove`: `--print-removed-path` and `--print-repo-root` (each prints one line to stdout).
   - **Option B:** A single `--print-paths` flag that prints two lines: removed_path on line 1, repo_root on line 2.

   Option B is simpler — one flag, two `read` calls in the wrapper.

## 4. Implementation Steps

1. **Add `--print-paths` flag to the `Remove` CLI variant in `cli.rs`.**
   - `#[arg(long, conflicts_with = "json")] print_paths: bool`
   - This flag outputs exactly two lines to stdout: `removed_path\nrepo_root\n`.

2. **Add `OutputFormat::RemovePaths` variant (or reuse CdPath with context).**
   - In `commands.rs` (or `main.rs` pre-refactor), handle the new format in `cmd_remove`:
     ```
     println!("{}", removed_path);
     println!("{}", repo_root);
     ```

3. **Rewrite Bash `remove` handler to use `--print-paths`.**
   - Replace the grep+sed JSON parsing with:
     ```bash
     result=$(wt-core remove "$@" --print-paths 2>/dev/null)
     rc=$?
     if [ $rc -eq 0 ]; then
         removed_path=$(echo "$result" | sed -n '1p')
         repo_root=$(echo "$result" | sed -n '2p')
         # ... cd logic + human message
     fi
     ```
   - For `--json` pass-through, keep the existing direct delegation to `wt-core remove "$@"`.

4. **Apply the same rewrite to Zsh `remove` handler.**
   - Same pattern as Bash but with `[[ ]]` syntax.

5. **Fix Fish `remove` handler.**
   - Replace the `string match -r` regex extraction with `--print-paths`:
     ```fish
     set -l result (wt-core remove $argv --print-paths 2>/dev/null)
     set -l rc $status
     if test $rc -eq 0
         set -l removed_path $result[1]
         set -l repo_root $result[2]
         if string match -q "$removed_path*" "$cwd_before"
             cd "$repo_root"; or true
         end
     end
     ```
   - Fix the `string match` direction: Fish glob matching with `string match -q` matches the *first argument* (pattern) against the *second* (string). The correct form is `string match -q "$removed_path*" "$cwd_before"` — this is actually correct syntactically (pattern first, string second). Verify and add a comment.

6. **Update Nu `remove` handler.**
   - Nu already uses `--json` internally and parses with `from json`, which is safe. No changes needed, but add a comment noting why Nu doesn't need `--print-paths`.

## 5. Testing Approach

1. Add an integration test for `wt-core remove <branch> --print-paths`:
   - Verify stdout is exactly two lines.
   - Verify line 1 is the removed worktree path.
   - Verify line 2 is the repo root path.
   - Verify it conflicts with `--json`.
2. Manual smoke test of each shell binding's remove flow (create worktree, cd into it, remove it, verify shell lands at repo root).

## 6. Review and Merge

- Confirm all 4 shell bindings handle remove without JSON parsing (except Nu which is safe).
- Confirm no regression in existing tests.
- Squash merge with message: `fix: replace fragile json parsing in shell remove wrappers`.
