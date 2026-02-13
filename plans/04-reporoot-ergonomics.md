# Plan: Improve RepoRoot and BranchName type ergonomics

## 1. Summary

`RepoRoot` is accessed via `.0` everywhere (`repo.0.clone()`, `repo.0.join(...)`). It should implement `AsRef<Path>` and `Deref<Target = Path>` so callsites read naturally. Similarly, `BranchName` is defined as a newtype but the codebase passes raw `&str` for branches throughout `worktree.rs` and `git.rs`, defeating the type safety the plan designed. This issue tightens both types.

## 2. Branching Strategy

`refactor/domain-type-ergonomics`

## 3. Investigation

Audit all usages of `repo.0` and raw branch `&str` to understand the scope:

- `repo.0` is used in: `git.rs` (every `git()` call uses `&repo.0`), `worktree.rs` (result structs clone `repo.0`), `main.rs` (never — goes through worktree results).
- Raw `&str` for branches: `worktree::add()`, `worktree::go()`, `worktree::remove()`, `git::add_worktree()`, `git::delete_branch()`, `git::branch_exists()`.

## 4. Implementation Steps

### Part A: RepoRoot ergonomics

1. **Add `AsRef<Path>` and `Deref` impls to `RepoRoot` in `domain.rs`.**
   ```rust
   impl AsRef<Path> for RepoRoot {
       fn as_ref(&self) -> &Path { &self.0 }
   }

   impl std::ops::Deref for RepoRoot {
       type Target = Path;
       fn deref(&self) -> &Path { &self.0 }
   }

   impl fmt::Display for RepoRoot {
       fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
           write!(f, "{}", self.0.display())
       }
   }
   ```

2. **Replace `&repo.0` with `repo.as_ref()` (or just `&repo` via Deref) in `git.rs`.**
   - Every `git(&[...], &repo.0)` becomes `git(&[...], &repo)`.

3. **Replace `repo.0.clone()` with `repo.to_path_buf()` in `worktree.rs` result structs.**
   - Via `Deref`, `repo.to_path_buf()` calls `Path::to_path_buf()`.

### Part B: BranchName follow-through

4. **Change `worktree::add`, `go`, `remove` signatures to accept `&BranchName` instead of `&str`.**
   - This is the biggest change. The conversion from raw string to `BranchName` moves to the command layer (where user input enters the system).

5. **Change `git::add_worktree`, `delete_branch`, `branch_exists` to accept `&BranchName`.**
   - These functions use the branch as a string in git args — they call `&branch_name.0` or `branch_name.as_str()`.
   - Add `BranchName::as_str(&self) -> &str` for this.

6. **Update `commands.rs` / `main.rs` to construct `BranchName` at the boundary.**
   - `cmd_add`: `let branch = BranchName::new(&cli_branch);` then pass `&branch`.
   - `cmd_go`: same.
   - `cmd_remove`: same (after inferring from cwd, wrap in `BranchName`).

7. **Update result structs to store `BranchName` instead of `String`.**
   - `AddResult`, `GoResult`, `RemoveResult` change `branch: String` → `branch: BranchName`.
   - Output formatting uses `result.branch.as_str()` or `Display`.

8. **Verify all pre-commit gates pass.**

## 5. Testing Approach

- No new tests needed. This is a type-level refactor — all existing tests exercise the same behavior through the CLI binary.
- Compile-time verification: if a callsite passes a raw `&str` where `&BranchName` is expected, it won't compile. That's the point.

## 6. Review and Merge

- Confirm zero `.0` accesses remain on `RepoRoot`.
- Confirm zero raw `&str` branch parameters remain in `worktree.rs` and `git.rs` public APIs.
- Squash merge with message: `refactor: improve RepoRoot and BranchName type ergonomics`.
