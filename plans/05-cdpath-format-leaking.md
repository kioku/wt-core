# Plan: Clean up OutputFormat::CdPath leaking to non-navigation commands

## 1. Summary

`OutputFormat::CdPath` is only meaningful for `add` and `go` (the commands that produce a path to `cd` into). But `cmd_list`, `cmd_remove`, and `cmd_doctor` all handle it — `list` and `doctor` fall through to JSON, `remove` falls through to JSON. The CLI layer (clap) correctly restricts `--print-cd-path` to add/go, so this isn't user-facing, but the internal handling is sloppy and could mask bugs. The fix is to make the type system enforce which commands support which formats.

## 2. Branching Strategy

`refactor/output-format-per-command`

## 3. Investigation

None. The scope is small and contained in the command dispatch layer.

## 4. Implementation Steps

1. **Split output format enums per command shape.**
   - In `output.rs`, replace the single `OutputFormat` with command-appropriate enums:
     ```rust
     /// For commands that produce a navigable path (add, go).
     pub enum NavigationFormat { Human, Json, CdPath }

     /// For commands that produce status/list output (list, remove, doctor).
     pub enum StatusFormat { Human, Json }
     ```

2. **Update `fmt_flag` (or replace it) in the command layer.**
   - `cmd_add` and `cmd_go` receive `NavigationFormat`.
   - `cmd_list`, `cmd_remove`, `cmd_doctor` receive `StatusFormat`.
   - The `fmt_flag` helper splits into two:
     ```rust
     fn nav_fmt(json: bool, cd_path: bool) -> NavigationFormat { ... }
     fn status_fmt(json: bool) -> StatusFormat { ... }
     ```

3. **Update each `cmd_*` function's `match` to use the correct enum.**
   - `cmd_list`, `cmd_remove`, `cmd_doctor`: match on `StatusFormat::Human | StatusFormat::Json`. No `CdPath` arm — the compiler enforces this.
   - `cmd_add`, `cmd_go`: match on `NavigationFormat::Human | Json | CdPath`.

4. **Verify all pre-commit gates pass.**

## 5. Testing Approach

- No new tests. This is a compile-time safety improvement. If any command mishandles a format variant, the compiler catches it as a non-exhaustive match.
- All existing tests pass unchanged.

## 6. Review and Merge

- Confirm no `CdPath` handling remains in list/remove/doctor.
- Confirm the compiler would reject adding `CdPath` to a `StatusFormat` match.
- Squash merge with message: `refactor: separate output format types for navigation vs status commands`.
