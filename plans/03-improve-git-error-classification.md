# Plan: Improve git error classification

## 1. Summary

`classify_git_error` in `git.rs` only matches three keywords ("unmerged", "modified", "dirty") to produce a `Conflict` exit code. All other git failures fall through to generic exit code 2 ("git error"), even when the stderr message clearly indicates a different category — e.g., "not a git repository" should be exit code 3, and "branch not found" or "already exists" patterns should map to their correct codes. This makes exit codes unreliable for programmatic consumers.

## 2. Branching Strategy

`fix/git-error-classification`

## 3. Investigation

Enumerate the git stderr patterns that should map to each exit code:

- **Exit 3 (NotARepo):** `"not a git repository"`, `"fatal: not a git repository"`
- **Exit 4 (Invariant):** Currently only produced by application-level checks (main worktree), not by git stderr. No git patterns needed.
- **Exit 5 (Conflict):** `"unmerged"`, `"modified"`, `"dirty"`, `"already exists"`, `"checked out"`, `"is not fully merged"`
- **Exit 2 (Git):** Everything else (the default).

Review git source or test with actual git commands to confirm stderr wording for:
- `git worktree remove` on a dirty worktree
- `git branch -d` on an unmerged branch
- `git worktree add` when the path already exists
- `git rev-parse` outside a repo

## 4. Implementation Steps

1. **Extend `classify_git_error` in `git.rs` with additional patterns.**
   ```rust
   fn classify_git_error(msg: String) -> AppError {
       let lower = msg.to_lowercase();

       // Not a repository
       if lower.contains("not a git repository") {
           return AppError::not_a_repo(msg);
       }

       // State conflicts
       if lower.contains("unmerged")
           || lower.contains("modified")
           || lower.contains("dirty")
           || lower.contains("already exists")
           || lower.contains("already checked out")
           || lower.contains("is not fully merged")
       {
           return AppError::conflict(msg);
       }

       AppError::git(msg)
   }
   ```

2. **Add unit tests for `classify_git_error`.**
   - The function is currently private. Keep it private but test via `#[cfg(test)]` in the same module:
     ```rust
     #[test]
     fn classify_not_a_repo() {
         let err = classify_git_error("fatal: not a git repository (or any of the parent directories)".to_string());
         assert_eq!(err.code, ExitCode::NotARepo);
     }

     #[test]
     fn classify_already_exists_is_conflict() {
         let err = classify_git_error("fatal: 'feature/x' already exists".to_string());
         assert_eq!(err.code, ExitCode::Conflict);
     }

     #[test]
     fn classify_not_fully_merged() {
         let err = classify_git_error("error: the branch 'x' is not fully merged".to_string());
         assert_eq!(err.code, ExitCode::Conflict);
     }

     #[test]
     fn classify_unknown_falls_to_git() {
         let err = classify_git_error("fatal: something unexpected".to_string());
         assert_eq!(err.code, ExitCode::Git);
     }
     ```

3. **Derive `PartialEq` on `ExitCode` (if not already) to enable `assert_eq!` in tests.**
   - `ExitCode` already derives `PartialEq`. Good — no change needed.

4. **Verify all pre-commit gates pass.**

## 5. Testing Approach

- New unit tests (step 2) directly test the classification function.
- Existing integration test `not_a_repo_exits_3` already validates the end-to-end exit code for the not-a-repo case (this flows through `repo_root` which has its own error mapping — confirm it still passes).
- No behavior change for existing happy paths.

## 6. Review and Merge

- Confirm the added patterns are based on actual git stderr output, not guesses.
- Confirm no existing test changes exit code expectations.
- Squash merge with message: `fix: improve git error classification for stable exit codes`.
